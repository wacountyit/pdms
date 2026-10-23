// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Resolving a file changed directly in SharePoint (spec §7.6). An
//! administrator either puts PDMS's recorded file back (picked from
//! SharePoint's version history by its SHA-256, never by label), or keeps the
//! change as a new document version through the normal upload checks and then
//! puts the earlier file back. Also: re-checking a file after a manual fix, and
//! dismissing files PDMS does not track.

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Capability;
use pdms_storage::ByteStream;
use pdms_storage::spool::{SpoolWriter, sanitize_file_name};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::changes::DriveItem;
use super::status;
use crate::Core;
use crate::context::Ctx;
use crate::documents::files::Upload;
use crate::documents::{self, VersionKind};
use crate::error::{CoreError, CoreResult};
use crate::graph::{GraphClient, GraphError};

/// A Microsoft Graph failure while resolving: nothing was changed, so the
/// administrator sees "unavailable" and can try again (D-102).
fn graph_err(e: impl std::fmt::Display) -> CoreError {
    super::unavailable(super::MOVE_UNAVAILABLE, e)
}

/// `status::parts`, with a SharePoint outage shown as "unavailable".
async fn parts(core: &Core) -> CoreResult<&GraphClient> {
    match status::parts(core).await {
        Ok((graph, _)) => Ok(graph),
        Err(e @ CoreError::Conflict(_)) => Err(e),
        Err(e) => Err(graph_err(e)),
    }
}

const MISSING_HELP: &str = "The file was deleted in SharePoint. A site owner can restore it from the \
    site's recycle bin (Site contents → Recycle bin); then choose Re-check.";
const ALREADY_OK: &str = "This file already matches PDMS's record.";
const NO_ORIGINAL: &str = "No earlier SharePoint version of this file matches PDMS's record. Accept the \
    change as a new version, or restore the file by hand in SharePoint and choose Re-check.";

struct Flagged {
    version_id: Uuid,
    document_id: Uuid,
    version_no: i32,
    label: String,
    department_id: Uuid,
    drive_id: String,
    item_id: String,
    size: i64,
    quick_xor_hash: String,
    state: String,
    sha256: Vec<u8>,
    file_name: String,
    changed_by: Option<String>,
    changed_at: Option<DateTime<Utc>>,
}

async fn load(core: &Core, version_id: Uuid) -> CoreResult<Flagged> {
    let r = sqlx::query!(
        r#"SELECT f.document_id, v.version_no, d.external_id, d.title, d.department_id, f.drive_id,
                  f.item_id, f.size, f.quick_xor_hash, f.state, v.blob_sha256 AS "sha256!",
                  coalesce(v.file_name, f.name) AS "file_name!", f.changed_by, f.changed_at
             FROM sharepoint_files f
             JOIN document_versions v ON v.id = f.document_version_id
             JOIN documents d ON d.id = f.document_id
            WHERE f.document_version_id = $1"#,
        version_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    Ok(Flagged {
        version_id,
        document_id: r.document_id,
        version_no: r.version_no,
        label: format!("{} {}", r.external_id, r.title),
        department_id: r.department_id,
        drive_id: r.drive_id,
        item_id: r.item_id,
        size: r.size,
        quick_xor_hash: r.quick_xor_hash,
        state: r.state,
        sha256: r.sha256,
        file_name: r.file_name,
        changed_by: r.changed_by,
        changed_at: r.changed_at,
    })
}

fn event(ctx: &Ctx, f: &Flagged, action: &str) -> AuditEvent {
    AuditEvent::new(Category::Content, action, ctx.actor.clone(), "document")
        .object(f.document_id, Some(&f.label))
        .document(f.document_id)
        .department(Some(f.department_id))
        .context("version", f.version_no)
        .context("item_id", f.item_id.as_str())
}

/// The file as SharePoint has it now (`None` if it is gone).
async fn current(graph: &GraphClient, f: &Flagged) -> CoreResult<Option<DriveItem>> {
    let path = format!(
        "/drives/{}/items/{}?$select=id,size,file",
        f.drive_id, f.item_id
    );
    match graph.get::<DriveItem>(&path).await {
        Ok(i) => Ok(Some(i)),
        Err(GraphError::NotFound) => Ok(None),
        Err(e) => Err(graph_err(e)),
    }
}

fn matches_record(f: &Flagged, item: &DriveItem) -> bool {
    item.size == Some(f.size) && item.quick_xor_hash() == Some(f.quick_xor_hash.as_str())
}

async fn sha256_of(mut s: ByteStream) -> CoreResult<Vec<u8>> {
    let mut h = Sha256::new();
    while let Some(chunk) = s.next().await {
        h.update(&chunk.map_err(graph_err)?);
    }
    Ok(h.finalize().to_vec())
}

#[derive(Deserialize)]
struct Versions {
    value: Vec<VersionInfo>,
}

#[derive(Deserialize)]
struct VersionInfo {
    id: String,
    #[serde(default)]
    size: Option<i64>,
}

/// The newest earlier SharePoint version whose content is PDMS's recorded file.
async fn original_version(graph: &GraphClient, f: &Flagged) -> CoreResult<Option<String>> {
    let list: Versions = graph
        .get(&format!(
            "/drives/{}/items/{}/versions",
            f.drive_id, f.item_id
        ))
        .await
        .map_err(graph_err)?;
    // The first entry is the current (changed) content.
    for v in list.value.iter().skip(1).take(20) {
        if v.size.is_some_and(|s| s != f.size) {
            continue;
        }
        let path = format!(
            "/drives/{}/items/{}/versions/{}/content",
            f.drive_id, f.item_id, v.id
        );
        if sha256_of(graph.download(&path).await.map_err(graph_err)?).await? == f.sha256 {
            return Ok(Some(v.id.clone()));
        }
    }
    Ok(None)
}

/// Make PDMS's recorded content current again. `Ok(None)` if SharePoint no
/// longer has it.
async fn put_back(graph: &GraphClient, f: &Flagged) -> CoreResult<Option<String>> {
    let Some(vid) = original_version(graph, f).await? else {
        return Ok(None);
    };
    let path = format!(
        "/drives/{}/items/{}/versions/{vid}/restoreVersion",
        f.drive_id, f.item_id
    );
    graph
        .post::<_, Value>(&path, &json!({}))
        .await
        .map_err(graph_err)?;
    match current(graph, f).await? {
        Some(item) if matches_record(f, &item) => Ok(Some(vid)),
        _ => Err(CoreError::internal(
            "SharePoint restored the earlier version, but the file still differs from PDMS's record",
        )),
    }
}

async fn mark_ok(
    core: &Core,
    ctx: &Ctx,
    f: &Flagged,
    resolution: &str,
    sharepoint_version: Option<&str>,
    new_version: Option<i32>,
) -> CoreResult<()> {
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE sharepoint_files SET state = 'ok', changed_by = NULL, changed_at = NULL, checked_at = now()
          WHERE document_version_id = $1",
        f.version_id
    )
    .execute(&mut *tx)
    .await?;
    let mut e = event(ctx, f, "sharepoint.change_resolved").context("resolution", resolution);
    if let Some(v) = sharepoint_version {
        e = e.context("sharepoint_version", v);
    }
    if let Some(n) = new_version {
        e = e.context("new_version", n);
    }
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

/// Put PDMS's recorded file back (requires `ManageSharePoint`).
pub async fn restore_original(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageSharePoint)?;
    let graph = parts(core).await?;
    let f = load(core, version_id).await?;
    match f.state.as_str() {
        "changed_outside" => {}
        "missing" => return Err(CoreError::Conflict(MISSING_HELP.into())),
        _ => return Err(CoreError::Conflict(ALREADY_OK.into())),
    }
    let vid = put_back(graph, &f)
        .await?
        .ok_or_else(|| CoreError::Conflict(NO_ORIGINAL.into()))?;
    mark_ok(core, ctx, &f, "restored_original", Some(&vid), None).await
}

/// Keep the change as a new version of the document (requires
/// `ManageSharePoint`, and `edit` on the document through the normal upload
/// path), then put the earlier file back. Returns the new version number.
pub async fn accept_as_new_version(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<i32> {
    ctx.require(Capability::ManageSharePoint)?;
    let graph = parts(core).await?;
    let f = load(core, version_id).await?;
    match f.state.as_str() {
        "changed_outside" => {}
        "missing" => return Err(CoreError::Conflict(MISSING_HELP.into())),
        _ => return Err(CoreError::Conflict(ALREADY_OK.into())),
    }
    let mut w = SpoolWriter::create(
        &core.config.storage.spool_path,
        core.config.storage.max_upload_bytes,
    )
    .await
    .map_err(CoreError::internal)?;
    let mut s = graph
        .download(&format!(
            "/drives/{}/items/{}/content",
            f.drive_id, f.item_id
        ))
        .await
        .map_err(graph_err)?;
    while let Some(chunk) = s.next().await {
        let chunk = chunk.map_err(graph_err)?;
        w.write(&chunk).await.map_err(|e| {
            CoreError::validation(format!("The changed file could not be read: {e}"))
        })?;
    }
    let upload = Upload {
        file: w.finish().await.map_err(CoreError::internal)?,
        file_name: sanitize_file_name(&f.file_name),
    };
    let when = f
        .changed_at
        .map(|t| format!(" on {}", t.format("%Y-%m-%d")))
        .unwrap_or_default();
    let note = format!(
        "Change made directly in SharePoint by {}{when}, accepted.",
        f.changed_by.as_deref().unwrap_or("someone")
    );
    let new_version = documents::add_version(
        core,
        ctx,
        f.document_id,
        upload,
        VersionKind::Revision,
        &note,
        None,
    )
    .await?;
    // The earlier file is part of the record too: put it back if SharePoint still has it.
    match put_back(graph, &f).await? {
        Some(vid) => {
            mark_ok(
                core,
                ctx,
                &f,
                "accepted_as_new_version",
                Some(&vid),
                Some(new_version),
            )
            .await?
        }
        None => {
            let mut tx = core.db.begin().await?;
            let e = event(ctx, &f, "sharepoint.change_resolved")
                .context("resolution", "accepted_as_new_version")
                .context("new_version", new_version)
                .context("original_restored", false);
            pdms_audit::append(&mut tx, vec![e]).await?;
            tx.commit().await?;
        }
    }
    Ok(new_version)
}

/// Compare the file with PDMS's record again, after someone fixed it by hand
/// (requires `ManageSharePoint`). True when it matches.
pub async fn recheck(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<bool> {
    ctx.require(Capability::ManageSharePoint)?;
    let graph = parts(core).await?;
    let f = load(core, version_id).await?;
    let item = current(graph, &f).await?;
    let ok = item.as_ref().is_some_and(|i| matches_record(&f, i));
    if ok && f.state != "ok" {
        mark_ok(core, ctx, &f, "matches_on_recheck", None, None).await?;
    } else {
        let state = match (ok, item.is_some()) {
            (true, _) => "ok",
            (false, true) => "changed_outside",
            (false, false) => "missing",
        };
        sqlx::query!(
            "UPDATE sharepoint_files SET state = $2, checked_at = now(), changed_at = coalesce(changed_at, now())
              WHERE document_version_id = $1",
            version_id,
            state
        )
        .execute(&core.db)
        .await?;
    }
    Ok(ok)
}

/// Stop listing a file PDMS does not track (requires `ManageSharePoint`).
pub async fn dismiss_unexpected(core: &Core, ctx: &Ctx, item_id: &str) -> CoreResult<()> {
    ctx.require(Capability::ManageSharePoint)?;
    let mut tx = core.db.begin().await?;
    let r = sqlx::query!(
        "UPDATE sharepoint_unexpected_items SET dismissed_by = $2, dismissed_at = now()
          WHERE item_id = $1 AND dismissed_at IS NULL RETURNING name, path",
        item_id,
        ctx.user_id()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "sharepoint.unexpected_dismissed",
                ctx.actor.clone(),
                "sharepoint_item",
            )
            .object(item_id, Some(&r.name))
            .context("path", r.path),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
