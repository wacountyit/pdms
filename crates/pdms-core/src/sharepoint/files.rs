// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Uploading document files to SharePoint and reading them back, verified.

use std::path::Path;

use bytes::Bytes;
use futures_util::StreamExt;
use pdms_audit::{Actor, AuditEvent, Category, Outcome};
use pdms_storage::spool::{SpoolWriter, SpooledFile};
use pdms_storage::{BlobHash, ByteStream};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::folders::{self, DocumentFolder, Item};
use super::layout::{self, Spot};
use super::quickxor::QuickXorHash;
use super::{READ_UNAVAILABLE, UPLOAD_UNAVAILABLE, unavailable};
use crate::Core;
use crate::error::{CoreError, CoreResult};

pub struct UploadTarget<'a> {
    /// `None` while the document is being created.
    pub document_id: Option<Uuid>,
    pub external_id: &'a str,
    pub title: &'a str,
    pub spot: Spot,
}

/// A file stored in SharePoint and not yet recorded in the database.
#[derive(Clone, Debug)]
pub struct UploadedFile {
    pub folder: DocumentFolder,
    pub drive_id: String,
    pub item_id: String,
    pub list_item_id: String,
    pub name: String,
    pub quick_xor_hash: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct Session {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
}

#[derive(Deserialize)]
struct Uploaded {
    id: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    file: Option<FileFacet>,
}

#[derive(Deserialize)]
struct FileFacet {
    #[serde(default)]
    hashes: Hashes,
}

#[derive(Deserialize, Default)]
struct Hashes {
    #[serde(rename = "quickXorHash", default)]
    quick_xor_hash: Option<String>,
}

#[derive(Deserialize)]
struct WithListItem {
    #[serde(rename = "listItem")]
    list_item: Option<Item>,
}

/// Upload `src` under a temporary name in the document's folder, which is
/// created if needed. The mirror job gives the file its final name once the
/// version number is known (spec §7.1). Nothing is written to the database
/// here. If SharePoint stores something other than what was sent, the item
/// is deleted and the upload fails.
pub(crate) async fn upload_staged(
    core: &Core,
    target: &UploadTarget<'_>,
    src: &Path,
    size: u64,
    ext: &str,
) -> CoreResult<UploadedFile> {
    let sp = core
        .sharepoint()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let run = async {
        let r = sp.resolved().await?;
        let folder = folders::document_folder(
            core,
            target.document_id,
            target.external_id,
            target.title,
            target.spot,
        )
        .await?;
        let name = layout::staging_name(target.external_id, &folders::token(), ext);
        let g = sp.graph();
        let session: Session = g
            .post(
                &format!(
                    "/drives/{}/items/{}:/{}:/createUploadSession",
                    r.drive_id,
                    folder.item_id,
                    layout::encode_segment(&name)
                ),
                &json!({"item": {"@microsoft.graph.conflictBehavior": "fail", "name": name}}),
            )
            .await?;
        let mut file = tokio::fs::File::open(src)
            .await
            .map_err(CoreError::internal)?;
        let mut buf = vec![0u8; usize::try_from(sp.chunk_bytes()).unwrap_or(327_680)];
        let mut qxh = QuickXorHash::new();
        let mut start = 0u64;
        let mut finished = None;
        while start < size {
            let n = read_full(&mut file, &mut buf).await?;
            let piece = buf.get(..n).unwrap_or_default();
            if piece.is_empty() {
                return Err(CoreError::internal("the spooled upload ended early"));
            }
            qxh.update(piece);
            let end = start + piece.len() as u64 - 1;
            finished = g
                .put_bytes(
                    &session.upload_url,
                    Bytes::copy_from_slice(piece),
                    (start, end, size),
                )
                .await?;
            start = end + 1;
        }
        let item: Uploaded = serde_json::from_value(
            finished.ok_or_else(|| CoreError::internal("SharePoint did not confirm the upload"))?,
        )
        .map_err(CoreError::internal)?;
        let local = qxh.finish_base64();
        let remote = item.file.and_then(|f| f.hashes.quick_xor_hash);
        if item.size != size || remote.as_deref() != Some(local.as_str()) {
            let _ = g
                .delete(&format!("/drives/{}/items/{}", r.drive_id, item.id))
                .await;
            return Err(CoreError::internal(format!(
                "SharePoint stored {} bytes with quickXorHash {remote:?}; PDMS sent {size} bytes with {local}",
                item.size
            )));
        }
        let with: WithListItem = g
            .get(&format!(
                "/drives/{}/items/{}?$select=id&$expand=listItem($select=id)",
                r.drive_id, item.id
            ))
            .await?;
        let list_item_id = with.list_item.map(|l| l.id).ok_or_else(|| {
            CoreError::internal("SharePoint returned no list item for the upload")
        })?;
        Ok::<UploadedFile, CoreError>(UploadedFile {
            folder,
            drive_id: r.drive_id.clone(),
            item_id: item.id,
            list_item_id,
            name,
            quick_xor_hash: local,
            size,
        })
    };
    run.await.map_err(|e| match e {
        // Graph failures (including conflicts) arrive as Internal or Conflict (P2's
        // mapping); people see the plain "unavailable" message, the detail is logged.
        e @ (CoreError::Validation(_) | CoreError::NotFound) => e,
        other => unavailable(UPLOAD_UNAVAILABLE, other),
    })
}

async fn read_full(file: &mut tokio::fs::File, buf: &mut [u8]) -> CoreResult<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..) {
        if rest.is_empty() {
            break;
        }
        let n = file.read(rest).await.map_err(CoreError::internal)?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Record an uploaded file against the version that now owns it. Runs in the
/// version's transaction.
pub(crate) async fn record_upload(
    conn: &mut PgConnection,
    document_id: Uuid,
    version_no: i32,
    up: &UploadedFile,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO sharepoint_document_folders (document_id, item_id, parent_item_id, name)
         VALUES ($1, $2, $3, $4) ON CONFLICT (document_id) DO NOTHING",
        document_id,
        up.folder.item_id,
        up.folder.parent_item_id,
        up.folder.name
    )
    .execute(&mut *conn)
    .await?;
    let inserted = sqlx::query!(
        "INSERT INTO sharepoint_files (document_version_id, document_id, drive_id, item_id, list_item_id, name,
                                       quick_xor_hash, size, last_known_path)
         SELECT v.id, $1, $3, $4, $5, $6, $7, $8, $9 FROM document_versions v
          WHERE v.document_id = $1 AND v.version_no = $2",
        document_id,
        version_no,
        up.drive_id,
        up.item_id,
        up.list_item_id,
        up.name,
        up.quick_xor_hash,
        i64::try_from(up.size).unwrap_or(i64::MAX),
        format!("{}/{}", up.folder.name, up.name)
    )
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(CoreError::internal(
            "the new version was not found for its SharePoint file",
        ));
    }
    Ok(())
}

/// Shown instead of a file that is marked `changed_outside` or `missing`,
/// until an administrator resolves it on Administration → SharePoint.
const PAUSED: &str = "A file of this document was changed outside PDMS. Downloading and viewing it are paused until an administrator checks it.";

/// Stream a version's file from SharePoint, checked against the SHA-256 and
/// size PDMS recorded. For a version that carried an earlier file forward,
/// the newest file at or before `version_no` is the one read. A file marked
/// `changed_outside` or `missing` is refused (`Conflict`) before SharePoint
/// is asked for it. A mismatch ends the stream with an error (the client
/// sees a failed download), marks the file `changed_outside`, and is audited.
pub(crate) async fn open(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    expected: BlobHash,
    size: u64,
) -> CoreResult<ByteStream> {
    let sp = core
        .sharepoint()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let f = sqlx::query!(
        "SELECT sf.drive_id, sf.item_id, sf.state FROM sharepoint_files sf
           JOIN document_versions v ON v.id = sf.document_version_id
          WHERE v.document_id = $1 AND v.version_no <= $2
          ORDER BY v.version_no DESC LIMIT 1",
        document_id,
        version_no
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| {
        CoreError::internal(format!(
            "document {document_id} v{version_no} has no file in SharePoint"
        ))
    })?;
    if f.state != "ok" {
        return Err(CoreError::Conflict(PAUSED.into()));
    }
    let stream = sp
        .graph()
        .download(&format!(
            "/drives/{}/items/{}/content",
            f.drive_id, f.item_id
        ))
        .await
        .map_err(|e| unavailable(READ_UNAVAILABLE, e))?;
    Ok(verified(
        stream,
        core.clone(),
        document_id,
        f.item_id,
        expected,
        size,
    ))
}

struct Check {
    inner: ByteStream,
    hasher: Sha256,
    seen: u64,
    expected: BlobHash,
    size: u64,
    core: Core,
    document_id: Uuid,
    item_id: String,
    done: bool,
}

fn verified(
    inner: ByteStream,
    core: Core,
    document_id: Uuid,
    item_id: String,
    expected: BlobHash,
    size: u64,
) -> ByteStream {
    let state = Check {
        inner,
        hasher: Sha256::new(),
        seen: 0,
        expected,
        size,
        core,
        document_id,
        item_id,
        done: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut s| async move {
        if s.done {
            return None;
        }
        match s.inner.next().await {
            Some(Ok(chunk)) => {
                s.hasher.update(&chunk);
                s.seen += chunk.len() as u64;
                Some((Ok(chunk), s))
            }
            Some(Err(e)) => {
                s.done = true;
                Some((Err(e), s))
            }
            None => {
                s.done = true;
                let got: [u8; 32] = s.hasher.clone().finalize().into();
                if s.seen == s.size && got == s.expected.0 {
                    return None;
                }
                let detail = format!(
                    "read {} bytes with SHA-256 {}; PDMS recorded {} bytes with {}",
                    s.seen,
                    hex::encode(got),
                    s.size,
                    s.expected.hex()
                );
                flag_changed(
                    &s.core,
                    s.document_id,
                    &s.item_id,
                    "download_check",
                    &detail,
                )
                .await;
                Some((
                    Err(std::io::Error::other(
                        "the file in SharePoint was changed outside PDMS; the download was stopped and an administrator was alerted",
                    )),
                    s,
                ))
            }
        }
    }))
}

/// The `sharepoint.changed_outside` event, one shape for the download check
/// and the change check (D-102; the owner's answer for P4): who changed the
/// file goes into `detail`, never into the event's actor.
pub(crate) fn changed_outside_event(
    document_id: Uuid,
    item_id: &str,
    source: &str,
    detail: &str,
) -> AuditEvent {
    AuditEvent::new(
        Category::System,
        "sharepoint.changed_outside",
        Actor::system("sharepoint"),
        "document",
    )
    .object(document_id, None)
    .document(document_id)
    .outcome(Outcome::Failure)
    .context("source", source)
    .context("item_id", item_id)
    .context("detail", detail)
}

/// Mark a SharePoint file as changed outside PDMS, audit it and tell the
/// administrators, in one transaction. Only the first time: a file already
/// flagged is left as it is. Failures to record are logged; the caller has
/// already refused the content.
pub(crate) async fn flag_changed(
    core: &Core,
    document_id: Uuid,
    item_id: &str,
    source: &str,
    detail: &str,
) {
    tracing::error!(%document_id, item_id, detail, "SharePoint file does not match PDMS's record");
    let r = async {
        let mut tx = core.db.begin().await?;
        let flagged = sqlx::query!(
            "UPDATE sharepoint_files
                SET state = 'changed_outside', checked_at = now(), changed_at = coalesce(changed_at, now())
              WHERE item_id = $1 AND state <> 'changed_outside'",
            item_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flagged > 0 {
            super::status::notify_admins(&mut tx, core, super::status::outside_changes_notice(1))
                .await?;
            pdms_audit::append(
                &mut tx,
                vec![changed_outside_event(document_id, item_id, source, detail)],
            )
            .await?;
        }
        tx.commit().await?;
        Ok::<(), CoreError>(())
    }
    .await;
    if let Err(e) = r {
        tracing::error!(error = %e, "could not record a SharePoint mismatch");
    }
}

/// Copy a version's file out of SharePoint into the spool directory,
/// verified, so it can be uploaded again as a new version.
pub(crate) async fn spool_copy(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    expected: BlobHash,
    size: u64,
) -> CoreResult<SpooledFile> {
    let mut stream = open(core, document_id, version_no, expected, size).await?;
    let mut w = SpoolWriter::create(&core.config().storage.spool_path, size.max(1))
        .await
        .map_err(CoreError::internal)?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| CoreError::Conflict(format!("The earlier file could not be read: {e}")))?;
        w.write(&chunk).await.map_err(CoreError::internal)?;
    }
    let f = w.finish().await.map_err(CoreError::internal)?;
    if f.sha256 != expected {
        return Err(CoreError::internal(
            "the copied file does not match its recorded SHA-256",
        ));
    }
    Ok(f)
}
