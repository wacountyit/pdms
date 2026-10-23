// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Keeping SharePoint in step with PDMS: folder placement, file names, and
//! column values (spec §7.3). PDMS is authoritative. The job is idempotent and
//! convergent: running it again after a crash or a manual change in
//! SharePoint restores the desired state.

use std::time::Duration;

use pdms_audit::{Actor, AuditEvent, Category};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgExecutor, PgPool};
use uuid::Uuid;

use super::folders::{department_folder_id, move_item};
use super::layout::{self, Area, PlacementDoc, PlacementFile, Spot};
use crate::Core;
use crate::documents::VersionKind;
use crate::error::{CoreError, CoreResult};
use crate::jobs::Worker;

pub const MIRROR_JOB: &str = "sharepoint.mirror_document";
pub const MIRROR_SWEEP_JOB: &str = "sharepoint.mirror_sweep";

/// Queue a mirror run for the document unless one is already pending.
/// (`jobs.idempotency_key` is unique across all job states, so it can't be
/// used to deduplicate a job that runs many times.)
pub async fn enqueue_mirror<'e>(exec: impl PgExecutor<'e>, document_id: Uuid) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO jobs (id, kind, payload, max_attempts)
         SELECT $1, 'sharepoint.mirror_document', jsonb_build_object('document_id', $2::uuid), 10
          WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE kind = 'sharepoint.mirror_document'
                              AND status = 'pending' AND payload->>'document_id' = $3)",
        Uuid::now_v7(),
        document_id,
        document_id.to_string()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Call inside every transaction that changes something shown in SharePoint,
/// before the audit append. Does nothing unless SharePoint is the backend.
pub(crate) async fn mirror_later(
    core: &Core,
    conn: &mut PgConnection,
    document_id: Uuid,
) -> CoreResult<()> {
    if core.sharepoint.is_some() {
        enqueue_mirror(&mut *conn, document_id).await?;
    }
    Ok(())
}

/// Queue a mirror run for every document with files in SharePoint. Picks up
/// renamed departments, types, tags, and people. Returns how many were queued.
pub async fn mirror_sweep(core: &Core) -> CoreResult<u64> {
    if core.sharepoint.is_none() {
        return Ok(0);
    }
    let r = sqlx::query!(
        "INSERT INTO jobs (id, kind, payload, max_attempts)
         SELECT gen_random_uuid(), 'sharepoint.mirror_document', jsonb_build_object('document_id', f.document_id), 10
           FROM sharepoint_document_folders f
          WHERE NOT EXISTS (SELECT 1 FROM jobs j WHERE j.kind = 'sharepoint.mirror_document'
                              AND j.status = 'pending' AND j.payload->>'document_id' = f.document_id::text)"
    )
    .execute(&core.db)
    .await?;
    Ok(r.rows_affected())
}

/// Eight characters of an ID, for stable fallback names.
fn short(id: Uuid) -> String {
    id.simple().to_string().chars().take(8).collect()
}

/// Bring one document's folder and files in SharePoint in line with PDMS.
pub async fn mirror_document(core: &Core, document_id: Uuid) -> CoreResult<()> {
    let Some(sp) = core.sharepoint.as_ref() else {
        return Ok(());
    };
    let mut conn = core.db.acquire().await?;
    let Some(doc) = crate::documents::fetch(&mut conn, document_id).await? else {
        return Ok(());
    };
    let Some(folder) = sqlx::query!(
        "SELECT item_id, parent_item_id, name FROM sharepoint_document_folders WHERE document_id = $1",
        document_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(()); // no files in SharePoint yet
    };
    let files = sqlx::query!(
        r#"SELECT sf.document_version_id, sf.item_id, sf.list_item_id, sf.name, sf.fields_hash,
                  v.version_no, v.kind AS "kind: VersionKind", v.file_name AS "file_name!", b.media_type
             FROM sharepoint_files sf
             JOIN document_versions v ON v.id = sf.document_version_id
             JOIN blobs b ON b.sha256 = v.blob_sha256
            WHERE sf.document_id = $1
            ORDER BY v.version_no"#,
        document_id
    )
    .fetch_all(&mut *conn)
    .await?;
    drop(conn);
    let r = sp.resolved().await?;
    let g = sp.graph();

    // 1. The folder: the right area, department, and name.
    let spot = Spot::of(doc.visibility, doc.department_id, doc.deleted_at.is_some());
    let parent = department_folder_id(core, spot.area, spot.department_id).await?;
    let want = layout::document_folder_name(&doc.external_id, &doc.title);
    let mut folder_name = folder.name.clone();
    if folder.parent_item_id != parent || folder.name != want {
        let fallback = layout::document_folder_name(
            &doc.external_id,
            &format!("{} {}", doc.title, short(document_id)),
        );
        let moving = (folder.parent_item_id != parent).then_some(parent.as_str());
        folder_name = move_item(g, &r.drive_id, &folder.item_id, moving, &want, &fallback).await?;
        record_folder_move(
            &core.db,
            document_id,
            (&folder.parent_item_id, &folder.name),
            (&parent, &folder_name),
            "mirror",
        )
        .await?;
    }

    // 2. Files: final names, `Current` on the newest, column values.
    let newest = files.iter().map(|f| f.version_no).max();
    let tags: Vec<String> = doc.tags.iter().map(|t| t.name.clone()).collect();
    let link = format!(
        "{}/documents/{}",
        core.config.server.public_url.as_str().trim_end_matches('/'),
        doc.id
    );
    let placement = PlacementDoc {
        external_id: &doc.external_id,
        title: &doc.title,
        type_name: &doc.type_name,
        tags: &tags,
        status: doc.status.label(),
        visibility: doc.visibility,
        department: &doc.department_name,
        owner: &doc.owner_name,
        effective_date: doc.effective_date,
        review_date: doc.next_review_date,
        renewal_date: doc.renewal_date,
        expiration_date: doc.expiration_date,
        link: &link,
    };
    for f in &files {
        let ext = layout::extension_for(&f.media_type, &f.file_name);
        let label = layout::version_label(f.kind);
        let want = layout::file_name(&doc.external_id, f.version_no, label, &ext);
        let mut name = f.name.clone();
        if name != want {
            let fallback = layout::file_name(
                &doc.external_id,
                f.version_no,
                &format!("{label} {}", short(f.document_version_id)),
                &ext,
            );
            name = move_item(g, &r.drive_id, &f.item_id, None, &want, &fallback).await?;
        }
        let fields = layout::columns(
            &placement,
            &PlacementFile {
                version_no: f.version_no,
                current: Some(f.version_no) == newest,
            },
        );
        let hash = hex::encode(Sha256::digest(
            serde_json::to_vec(&fields).map_err(CoreError::internal)?,
        ));
        if f.fields_hash.as_deref() != Some(hash.as_str()) {
            let _: serde_json::Value = g
                .patch(
                    &format!(
                        "/sites/{}/lists/{}/items/{}/fields",
                        r.site_id, r.list_id, f.list_item_id
                    ),
                    &fields,
                )
                .await?;
        }
        sqlx::query!(
            "UPDATE sharepoint_files SET name = $2, fields_hash = $3, last_known_path = $4
              WHERE document_version_id = $1",
            f.document_version_id,
            name,
            hash,
            format!("{folder_name}/{name}")
        )
        .execute(&core.db)
        .await?;
    }
    Ok(())
}

/// Add the SharePoint jobs to the worker (only when SharePoint is the backend).
pub fn register_jobs(worker: Worker, core: &Core) -> Worker {
    if core.sharepoint.is_none() {
        return worker;
    }
    let (c1, c2) = (core.clone(), core.clone());
    worker
        .handle(MIRROR_JOB, move |job| {
            let core = c1.clone();
            async move {
                let id: Uuid = job
                    .payload
                    .get("document_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .ok_or("missing document_id")?;
                mirror_document(&core, id).await.map_err(|e| e.to_string())
            }
        })
        .handle(MIRROR_SWEEP_JOB, move |_| {
            let core = c2.clone();
            async move {
                let n = mirror_sweep(&core).await.map_err(|e| e.to_string())?;
                tracing::info!(documents = n, "SharePoint mirror sweep queued");
                Ok(())
            }
        })
        .every(MIRROR_SWEEP_JOB, Duration::from_secs(24 * 3600))
}

/// A folder's path in the library, for the audit log.
async fn folder_path(db: &PgPool, parent_item_id: &str, name: &str) -> CoreResult<String> {
    let parent = sqlx::query!(
        "SELECT area, name FROM sharepoint_department_folders WHERE item_id = $1",
        parent_item_id
    )
    .fetch_optional(db)
    .await?;
    Ok(match parent {
        Some(p) => {
            let area = Area::ALL
                .iter()
                .find(|a| a.as_str() == p.area)
                .map_or(p.area.as_str(), |a| a.folder_name());
            format!("{area}/{}/{name}", p.name)
        }
        None => format!("{parent_item_id}/{name}"),
    })
}

/// Record that a document's folder moved or was renamed: the folder row and
/// its `sharepoint.folder_moved` event in one transaction (D-102). `reason`
/// is `visibility`, `department`, `deleted`, or `mirror`.
async fn record_folder_move(
    db: &PgPool,
    document_id: Uuid,
    from: (&str, &str),
    to: (&str, &str),
    reason: &str,
) -> CoreResult<()> {
    let from_path = folder_path(db, from.0, from.1).await?;
    let to_path = folder_path(db, to.0, to.1).await?;
    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE sharepoint_document_folders SET parent_item_id = $2, name = $3, updated_at = now()
          WHERE document_id = $1",
        document_id,
        to.0,
        to.1
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::System,
                "sharepoint.folder_moved",
                Actor::system("sharepoint"),
                "document",
            )
            .object(document_id, None)
            .document(document_id)
            .context("from", from_path)
            .context("to", to_path)
            .context("reason", reason),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Move the document's folder to `to` now, before the change that requires
/// it is saved. Used when a change narrows who can browse the document
/// (`layout::narrows`); `reason` is recorded with the move. If SharePoint
/// can't be reached, the error tells the user the change was not made.
pub async fn apply_placement_now(
    core: &Core,
    document_id: Uuid,
    to: Spot,
    reason: &str,
) -> CoreResult<()> {
    let Some(sp) = core.sharepoint.as_ref() else {
        return Ok(());
    };
    let Some(folder) = sqlx::query!(
        "SELECT item_id, parent_item_id, name FROM sharepoint_document_folders WHERE document_id = $1",
        document_id
    )
    .fetch_optional(&core.db)
    .await?
    else {
        return Ok(()); // no files in SharePoint
    };
    let run = async {
        let r = sp.resolved().await?;
        let parent = department_folder_id(core, to.area, to.department_id).await?;
        if parent == folder.parent_item_id {
            return Ok::<(), CoreError>(());
        }
        let fallback = format!(
            "{} {}",
            layout::sanitize_segment(&folder.name, layout::FOLDER_MAX - 9),
            short(document_id)
        );
        let name = move_item(
            sp.graph(),
            &r.drive_id,
            &folder.item_id,
            Some(parent.as_str()),
            &folder.name,
            &fallback,
        )
        .await?;
        record_folder_move(
            &core.db,
            document_id,
            (&folder.parent_item_id, &folder.name),
            (&parent, &name),
            reason,
        )
        .await
    };
    run.await.map_err(|e| match e {
        // Graph failures (including conflicts) arrive as Internal or Conflict (P2's
        // mapping); people see the plain "unavailable" message, the detail is logged.
        e @ (CoreError::Validation(_) | CoreError::NotFound) => e,
        other => super::unavailable(super::MOVE_UNAVAILABLE, other),
    })
}
