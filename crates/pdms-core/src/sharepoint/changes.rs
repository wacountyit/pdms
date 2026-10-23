// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Changes made directly in SharePoint (spec §7.6, D-089). Every few minutes
//! PDMS reads the library's change feed (Graph delta) and compares each file
//! it tracks with what it recorded at upload: size and QuickXorHash. A file
//! that differs, or is gone, is flagged, audited, and reported to
//! administrators. Who made the change is recorded but never trusted: the
//! hashes decide. Files PDMS does not track are listed for review and never
//! deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::Utc;
use pdms_audit::{AuditEvent, Category};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::status::{self, DELTA_CURSOR, RunError};
use super::{Area, MIRROR_SWEEP_JOB};
use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;
use crate::graph::GraphError;
use crate::jobs::{self, NewJob};

pub const DELTA_JOB: &str = "sharepoint.delta";

/// Properties read from the feed and from single items.
const SELECT: &str =
    "id,name,size,file,folder,deleted,root,parentReference,lastModifiedBy,createdBy";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveItem {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub file: Option<FileFacet>,
    #[serde(default)]
    pub folder: Option<Value>,
    #[serde(default)]
    pub deleted: Option<Value>,
    #[serde(default)]
    pub root: Option<Value>,
    #[serde(default)]
    pub parent_reference: Option<ParentReference>,
    #[serde(default)]
    pub last_modified_by: Option<IdentitySet>,
    #[serde(default)]
    pub created_by: Option<IdentitySet>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FileFacet {
    #[serde(default)]
    pub hashes: Option<Hashes>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hashes {
    #[serde(default)]
    pub quick_xor_hash: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ParentReference {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct IdentitySet {
    #[serde(default)]
    pub application: Option<Identity>,
    #[serde(default)]
    pub user: Option<Identity>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl IdentitySet {
    /// Written by PDMS's own app registration.
    pub fn is_app(&self, client_id: &str) -> bool {
        self.application
            .as_ref()
            .and_then(|a| a.id.as_deref())
            .is_some_and(|id| id.eq_ignore_ascii_case(client_id))
    }

    /// A readable name for the audit log and the status panel.
    pub fn describe(&self) -> String {
        let user = self.user.as_ref();
        let name = user.and_then(|u| u.display_name.clone());
        let email = user.and_then(|u| u.email.clone());
        match (name, email) {
            (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
            (Some(n), _) => n,
            (None, Some(e)) => e,
            (None, None) => self
                .application
                .as_ref()
                .and_then(|a| a.display_name.clone())
                .unwrap_or_else(|| "unknown".into()),
        }
    }
}

impl DriveItem {
    pub fn quick_xor_hash(&self) -> Option<&str> {
        self.file
            .as_ref()?
            .hashes
            .as_ref()?
            .quick_xor_hash
            .as_deref()
    }

    fn modified_by_pdms(&self, client_id: &str) -> bool {
        self.last_modified_by
            .as_ref()
            .is_some_and(|w| w.is_app(client_id))
    }

    fn modifier(&self) -> String {
        self.last_modified_by
            .as_ref()
            .map_or_else(|| "unknown".into(), IdentitySet::describe)
    }
}

/// What PDMS recorded about a file it put in SharePoint.
#[derive(Clone, Debug)]
pub struct Tracked {
    pub version_id: Uuid,
    pub document_id: Uuid,
    pub quick_xor_hash: String,
    pub size: i64,
    pub state: String,
    /// Who changed it, when known (a download check doesn't know).
    pub changed_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A tracked file now differs from PDMS's record, or is gone.
    Flag {
        version_id: Uuid,
        document_id: Uuid,
        by: String,
        missing: bool,
    },
    /// A file a download check already flagged: record who changed it (no
    /// second alarm).
    Attribute {
        version_id: Uuid,
        by: String,
    },
    /// A flagged file matches PDMS's record again.
    Cleared {
        version_id: Uuid,
        document_id: Uuid,
    },
    /// A person renamed, moved, or relabelled a tracked file without changing
    /// its content: put PDMS's name, place, and columns back.
    Touched {
        document_id: Uuid,
    },
    /// A file PDMS does not track.
    Unexpected {
        item_id: String,
        name: String,
        path: String,
        by: String,
        by_pdms: bool,
    },
    /// A file PDMS does not track is gone.
    Forget {
        item_id: String,
    },
    /// A person renamed or moved a folder.
    FolderChanged {
        item_id: String,
    },
    Ignore,
}

/// What one change-feed entry means. Pure: the caller supplies PDMS's record.
pub fn observe(item: &DriveItem, tracked: Option<&Tracked>, client_id: &str) -> Observation {
    if item.root.is_some() {
        return Observation::Ignore;
    }
    if item.deleted.is_some() {
        return match tracked {
            Some(t) if t.state == "missing" => Observation::Ignore,
            Some(t) => Observation::Flag {
                version_id: t.version_id,
                document_id: t.document_id,
                by: item.modifier(),
                missing: true,
            },
            None => Observation::Forget {
                item_id: item.id.clone(),
            },
        };
    }
    if item.folder.is_some() {
        return if item.modified_by_pdms(client_id) {
            Observation::Ignore
        } else {
            Observation::FolderChanged {
                item_id: item.id.clone(),
            }
        };
    }
    let Some(t) = tracked else {
        let by_pdms = item
            .created_by
            .as_ref()
            .is_some_and(|w| w.is_app(client_id));
        return Observation::Unexpected {
            item_id: item.id.clone(),
            name: item.name.clone().unwrap_or_default(),
            path: String::new(),
            by: if by_pdms {
                "PDMS".into()
            } else {
                item.created_by
                    .as_ref()
                    .map_or_else(|| "unknown".into(), IdentitySet::describe)
            },
            by_pdms,
        };
    };
    let same =
        item.size == Some(t.size) && item.quick_xor_hash() == Some(t.quick_xor_hash.as_str());
    match (same, t.state.as_str()) {
        (false, "changed_outside") if t.changed_by.is_none() => Observation::Attribute {
            version_id: t.version_id,
            by: item.modifier(),
        },
        (false, "changed_outside") => Observation::Ignore,
        (false, _) => Observation::Flag {
            version_id: t.version_id,
            document_id: t.document_id,
            by: item.modifier(),
            missing: false,
        },
        (true, "ok") if item.modified_by_pdms(client_id) => Observation::Ignore,
        (true, "ok") => Observation::Touched {
            document_id: t.document_id,
        },
        (true, _) => Observation::Cleared {
            version_id: t.version_id,
            document_id: t.document_id,
        },
    }
}

/// `/drives/{id}/root:/Departments/X` (or `/drive/root:/…`) and a name → `Departments/X/name`.
pub fn display_path(parent_path: Option<&str>, name: &str) -> String {
    let folder = parent_path
        .and_then(|p| {
            p.split_once("root:")
                .map(|(_, rest)| rest.trim_matches('/'))
        })
        .unwrap_or("");
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaReport {
    pub items: u32,
    pub modified: u32,
    pub missing: u32,
    pub cleared: u32,
    pub unexpected: u32,
    pub mirrors: u32,
    pub resynced: bool,
}

/// Ask the worker to check for changes now (the status panel's button).
pub async fn enqueue_now(db: &PgPool) -> CoreResult<()> {
    let key = format!(
        "{DELTA_JOB}:manual:{}",
        Utc::now().timestamp().div_euclid(60)
    );
    jobs::enqueue(
        db,
        NewJob::new(DELTA_JOB, json!({}))
            .idempotency_key(key)
            .max_attempts(3),
    )
    .await?;
    Ok(())
}

/// Read the change feed since the last run and act on it (one run at a
/// time). A no-op when SharePoint storage is off.
pub async fn detect_changes(core: &Core) -> CoreResult<DeltaReport> {
    if core.sharepoint.is_none() {
        return Ok(DeltaReport::default());
    }
    match run(core).await {
        Ok(r) => Ok(r),
        Err(e) => {
            status::report_failure(core, DELTA_CURSOR, &e).await?;
            Err(e.into())
        }
    }
}

/// Documents laid out inside a folder: the document folder itself, or every
/// document folder in a department folder.
async fn documents_in_folder(db: &PgPool, item_id: &str) -> CoreResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT document_id AS "document_id!" FROM sharepoint_document_folders WHERE item_id = $1
           UNION
           SELECT f.document_id FROM sharepoint_document_folders f
             JOIN sharepoint_department_folders d ON d.item_id = f.parent_item_id
            WHERE d.item_id = $1"#,
        item_id
    )
    .fetch_all(db)
    .await?)
}

async fn run(core: &Core) -> Result<DeltaReport, RunError> {
    let (graph, resolved) = status::parts(core).await?;
    let drive = resolved.drive_id.clone();
    let client_id = core.config.graph.client_id.clone();
    let mut lock = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pdms.sharepoint_delta'))")
        .execute(&mut *lock)
        .await?;

    let fresh = format!("/drives/{drive}/root/delta?$select={SELECT}");
    let cursor: Option<String> = sqlx::query_scalar!(
        "SELECT delta_link FROM sync_cursors WHERE name = $1",
        DELTA_CURSOR
    )
    .fetch_optional(&core.db)
    .await?
    .flatten();
    let mut report = DeltaReport::default();
    let page = match graph
        .get_delta::<DriveItem>(cursor.as_deref().unwrap_or(&fresh))
        .await
    {
        Err(GraphError::ResyncRequired) => {
            report.resynced = true;
            graph.get_delta::<DriveItem>(&fresh).await?
        }
        other => other?,
    };
    report.items = u32::try_from(page.items.len()).unwrap_or(u32::MAX);
    // An item can appear more than once; the last occurrence is current.
    let mut latest: BTreeMap<String, DriveItem> = BTreeMap::new();
    for it in page.items {
        latest.insert(it.id.clone(), it);
    }
    let ids: Vec<String> = latest.keys().cloned().collect();
    let tracked: HashMap<String, Tracked> = sqlx::query!(
        "SELECT item_id, document_version_id, document_id, quick_xor_hash, size, state, changed_by
           FROM sharepoint_files WHERE item_id = ANY($1)",
        &ids
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| {
        let t = Tracked {
            version_id: r.document_version_id,
            document_id: r.document_id,
            quick_xor_hash: r.quick_xor_hash,
            size: r.size,
            state: r.state,
            changed_by: r.changed_by,
        };
        (r.item_id, t)
    })
    .collect();

    // Read everything needed from Graph before opening the transaction.
    let mut observations = Vec::new();
    let mut folders = Vec::new();
    for (id, mut item) in latest {
        let t = tracked.get(&id);
        if t.is_some() && item.deleted.is_none() && item.quick_xor_hash().is_none() {
            // The feed may leave out hashes; read the file itself.
            match graph
                .get::<DriveItem>(&format!("/drives/{drive}/items/{id}?$select={SELECT}"))
                .await
            {
                Ok(full) => item = full,
                Err(GraphError::NotFound) => item.deleted = Some(json!({})),
                Err(e) => return Err(e.into()),
            }
        }
        match observe(&item, t, &client_id) {
            Observation::Unexpected {
                item_id,
                name,
                by,
                by_pdms,
                ..
            } => {
                // The feed leaves out folder paths; read the item for its location.
                let url =
                    format!("/drives/{drive}/items/{item_id}?$select=id,name,parentReference");
                let path = match graph.get::<DriveItem>(&url).await {
                    Ok(full) => display_path(
                        full.parent_reference
                            .as_ref()
                            .and_then(|p| p.path.as_deref()),
                        &name,
                    ),
                    Err(GraphError::NotFound) => continue,
                    Err(e) => return Err(e.into()),
                };
                observations.push(Observation::Unexpected {
                    item_id,
                    name,
                    path,
                    by,
                    by_pdms,
                });
            }
            Observation::FolderChanged { item_id } => {
                folders.push((item_id, item.name.clone().unwrap_or_default()))
            }
            o => observations.push(o),
        }
    }
    let mut mirror: BTreeSet<Uuid> = BTreeSet::new();
    let mut sweep = false;
    for (f, name) in folders {
        match Area::ALL.into_iter().find(|a| resolved.area_id(*a) == f) {
            // The feed can list an area folder whenever something inside it
            // changes, and people may have created it by hand (setup guide).
            // Only a rename matters.
            Some(area) => sweep |= name != area.folder_name(),
            None => mirror.extend(documents_in_folder(&core.db, &f).await?),
        }
    }

    let ctx = Ctx::system("sharepoint-changes");
    let mut events = Vec::new();
    let mut tx = core.db.begin().await?;
    for o in observations {
        match o {
            Observation::Flag {
                version_id,
                document_id,
                by,
                missing,
            } => {
                let state = if missing {
                    "missing"
                } else {
                    "changed_outside"
                };
                let r = sqlx::query!(
                    r#"UPDATE sharepoint_files f
                          SET state = $2, changed_by = $3, changed_at = now(), checked_at = now()
                         FROM document_versions v, documents d
                        WHERE f.document_version_id = $1 AND v.id = f.document_version_id AND d.id = v.document_id
                    RETURNING v.version_no AS "version_no!", f.item_id AS "item_id!",
                              f.last_known_path AS "path!""#,
                    version_id,
                    state,
                    by.as_str()
                )
                .fetch_one(&mut *tx)
                .await?;
                let detail = format!(
                    "{} in SharePoint by {by} (version {}, {})",
                    if missing { "deleted" } else { "modified" },
                    r.version_no,
                    r.path
                );
                events.push(super::files::changed_outside_event(
                    document_id,
                    &r.item_id,
                    "change_check",
                    &detail,
                ));
                if missing {
                    report.missing += 1;
                } else {
                    report.modified += 1;
                }
            }
            Observation::Attribute { version_id, by } => {
                sqlx::query!(
                    "UPDATE sharepoint_files SET changed_by = $2, checked_at = now()
                      WHERE document_version_id = $1 AND changed_by IS NULL",
                    version_id,
                    by
                )
                .execute(&mut *tx)
                .await?;
            }
            Observation::Cleared {
                version_id,
                document_id,
            } => {
                let r = sqlx::query!(
                    r#"UPDATE sharepoint_files f
                          SET state = 'ok', changed_by = NULL, changed_at = NULL, checked_at = now()
                         FROM document_versions v, documents d
                        WHERE f.document_version_id = $1 AND v.id = f.document_version_id AND d.id = v.document_id
                    RETURNING v.version_no AS "version_no!", d.external_id AS "external_id!", d.title AS "title!",
                              d.department_id AS "department_id!""#,
                    version_id
                )
                .fetch_one(&mut *tx)
                .await?;
                events.push(
                    AuditEvent::new(
                        Category::Content,
                        "sharepoint.change_resolved",
                        ctx.actor.clone(),
                        "document",
                    )
                    .object(document_id, Some(&format!("{} {}", r.external_id, r.title)))
                    .document(document_id)
                    .department(Some(r.department_id))
                    .context("version", r.version_no)
                    .context("resolution", "matches_again"),
                );
                report.cleared += 1;
                mirror.insert(document_id);
            }
            Observation::Touched { document_id } => {
                mirror.insert(document_id);
            }
            Observation::Unexpected {
                item_id,
                name,
                path,
                by,
                by_pdms,
            } => {
                sqlx::query!(
                    "INSERT INTO sharepoint_unexpected_items (item_id, name, path, created_by, by_pdms)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (item_id) DO UPDATE SET name = EXCLUDED.name, path = EXCLUDED.path, last_seen = now()",
                    item_id,
                    name,
                    path,
                    by,
                    by_pdms
                )
                .execute(&mut *tx)
                .await?;
                report.unexpected += 1;
            }
            Observation::Forget { item_id } => {
                sqlx::query!(
                    "DELETE FROM sharepoint_unexpected_items WHERE item_id = $1",
                    item_id
                )
                .execute(&mut *tx)
                .await?;
            }
            Observation::FolderChanged { .. } | Observation::Ignore => {}
        }
    }
    for document_id in &mirror {
        super::enqueue_mirror(&mut *tx, *document_id).await?;
    }
    if sweep {
        let key = format!("{MIRROR_SWEEP_JOB}:changes:{}", Uuid::now_v7());
        jobs::enqueue(
            &mut *tx,
            NewJob::new(MIRROR_SWEEP_JOB, json!({}))
                .idempotency_key(key)
                .max_attempts(3),
        )
        .await?;
    }
    report.mirrors = u32::try_from(mirror.len()).unwrap_or(u32::MAX) + u32::from(sweep);
    // Files that were untracked only because their upload had not committed yet.
    sqlx::query!("DELETE FROM sharepoint_unexpected_items u USING sharepoint_files f WHERE f.item_id = u.item_id")
        .execute(&mut *tx)
        .await?;
    status::record_ok(&mut tx, DELTA_CURSOR, Some(&page.delta_link)).await?;
    let flagged = report.modified + report.missing;
    if flagged > 0 {
        status::notify_admins(&mut tx, core, status::outside_changes_notice(flagged)).await?;
    }
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    lock.commit().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    const APP: &str = "client-1";

    fn item(v: Value) -> DriveItem {
        serde_json::from_value(v).unwrap()
    }

    fn file(hash: &str, size: i64, by_app: bool) -> DriveItem {
        let by = if by_app {
            json!({"application": {"id": APP}})
        } else {
            json!({"user": {"displayName": "Pat Clerk", "email": "pat@county.example"}})
        };
        item(json!({"id": "f1", "name": "a.pdf", "size": size,
                    "file": {"hashes": {"quickXorHash": hash}},
                    "lastModifiedBy": by, "createdBy": by}))
    }

    fn tracked(state: &str) -> Tracked {
        Tracked {
            version_id: Uuid::from_u128(1),
            document_id: Uuid::from_u128(2),
            quick_xor_hash: "AAA=".into(),
            size: 10,
            state: state.into(),
            changed_by: Some("Pat Clerk".into()),
        }
    }

    #[test]
    fn a_file_changed_by_a_person_is_flagged() {
        assert_eq!(
            observe(&file("BBB=", 12, false), Some(&tracked("ok")), APP),
            Observation::Flag {
                version_id: Uuid::from_u128(1),
                document_id: Uuid::from_u128(2),
                by: "Pat Clerk <pat@county.example>".into(),
                missing: false,
            }
        );
    }

    #[test]
    fn the_hash_decides_not_the_author() {
        assert!(matches!(
            observe(&file("BBB=", 10, true), Some(&tracked("ok")), APP),
            Observation::Flag { missing: false, .. }
        ));
        assert_eq!(
            observe(&file("AAA=", 10, true), Some(&tracked("ok")), APP),
            Observation::Ignore
        );
        assert_eq!(
            observe(&file("AAA=", 10, false), Some(&tracked("ok")), APP),
            Observation::Touched {
                document_id: Uuid::from_u128(2)
            }
        );
    }

    #[test]
    fn flags_are_raised_once_and_cleared_when_the_file_matches_again() {
        assert_eq!(
            observe(
                &file("BBB=", 12, false),
                Some(&tracked("changed_outside")),
                APP
            ),
            Observation::Ignore
        );
        assert_eq!(
            observe(
                &file("AAA=", 10, false),
                Some(&tracked("changed_outside")),
                APP
            ),
            Observation::Cleared {
                version_id: Uuid::from_u128(1),
                document_id: Uuid::from_u128(2)
            }
        );
    }

    #[test]
    fn a_file_a_download_flagged_gets_its_author_without_a_second_alarm() {
        let flagged_by_download = Tracked {
            changed_by: None,
            ..tracked("changed_outside")
        };
        assert_eq!(
            observe(&file("BBB=", 12, false), Some(&flagged_by_download), APP),
            Observation::Attribute {
                version_id: Uuid::from_u128(1),
                by: "Pat Clerk <pat@county.example>".into(),
            }
        );
    }

    #[test]
    fn deletions() {
        let gone = item(json!({"id": "f1", "deleted": {"state": "deleted"}}));
        assert!(matches!(
            observe(&gone, Some(&tracked("ok")), APP),
            Observation::Flag { missing: true, .. }
        ));
        assert_eq!(
            observe(&gone, Some(&tracked("missing")), APP),
            Observation::Ignore
        );
        assert_eq!(
            observe(&gone, None, APP),
            Observation::Forget {
                item_id: "f1".into()
            }
        );
    }

    #[test]
    fn untracked_files_and_folders() {
        assert!(matches!(
            observe(&file("AAA=", 10, false), None, APP),
            Observation::Unexpected { by_pdms: false, .. }
        ));
        assert!(matches!(
            observe(&file("AAA=", 10, true), None, APP),
            Observation::Unexpected { by_pdms: true, .. }
        ));
        let by_app = item(
            json!({"id": "d1", "name": "X", "folder": {}, "lastModifiedBy": {"application": {"id": APP}}}),
        );
        let by_person = item(
            json!({"id": "d1", "name": "X", "folder": {}, "lastModifiedBy": {"user": {"displayName": "Pat"}}}),
        );
        assert_eq!(observe(&by_app, None, APP), Observation::Ignore);
        assert_eq!(
            observe(&by_person, None, APP),
            Observation::FolderChanged {
                item_id: "d1".into()
            }
        );
        let root = item(json!({"id": "root", "root": {}, "folder": {}}));
        assert_eq!(observe(&root, None, APP), Observation::Ignore);
    }

    #[test]
    fn paths_drop_the_drive_prefix() {
        assert_eq!(
            display_path(
                Some("/drives/b!x/root:/Departments/Road and Bridge"),
                "a.pdf"
            ),
            "Departments/Road and Bridge/a.pdf"
        );
        assert_eq!(display_path(Some("/drive/root:"), "a.pdf"), "a.pdf");
        assert_eq!(display_path(None, "a.pdf"), "a.pdf");
    }
}
