// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Uploading document files to SharePoint and reading them back, verified.

use std::path::Path;

use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgConnection;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::folders::{self, DocumentFolder, Item};
use super::layout::{self, Spot};
use super::quickxor::QuickXorHash;
use super::{UPLOAD_UNAVAILABLE, unavailable};
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
        .sharepoint
        .as_ref()
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
                "SharePoint stored {} bytes with quickXorHash {remote:?}; PADM sent {size} bytes with {local}",
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
