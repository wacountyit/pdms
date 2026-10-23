// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! SharePoint background work: run records for the status panel (in P2's
//! `sync_cursors`), notices to administrators, and the queries behind
//! Administration → SharePoint.

use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::{Capability, Permission, role_keys};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{Resolved, SharePoint};
use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::graph::{GraphClient, GraphError};
use crate::notifications::actions::{self, Notice};
use crate::notifications::dispatch::Visibility;

/// `sync_cursors` names.
pub const DELTA_CURSOR: &str = "sharepoint.delta";
pub const RECONCILE_CURSOR: &str = "sharepoint.reconcile";
/// P2's Entra sync cursor (read only, for the panel).
pub const ENTRA_CURSOR: &str = crate::directory::entra_sync::CURSOR;

/// A failed SharePoint background run. Graph errors are kept as they are, so
/// sign-in failures can be told apart.
#[derive(Debug)]
pub enum RunError {
    Graph(GraphError),
    Core(CoreError),
}

impl RunError {
    /// Microsoft Graph refused PDMS's sign-in or access (HTTP 401/403).
    pub fn is_auth(&self) -> bool {
        match self {
            RunError::Graph(GraphError::Auth(_)) => true,
            // P3's lazy library lookup (`SharePoint::resolved`) hands Graph
            // errors back as `CoreError::Internal(<GraphError's text>)`.
            RunError::Core(CoreError::Internal(m)) => {
                m.starts_with(&GraphError::Auth(String::new()).to_string())
            }
            _ => false,
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Graph(e) => write!(f, "{e}"),
            RunError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl From<GraphError> for RunError {
    fn from(e: GraphError) -> Self {
        RunError::Graph(e)
    }
}

impl From<CoreError> for RunError {
    fn from(e: CoreError) -> Self {
        RunError::Core(e)
    }
}

impl From<sqlx::Error> for RunError {
    fn from(e: sqlx::Error) -> Self {
        RunError::Core(e.into())
    }
}

impl From<pdms_audit::AuditError> for RunError {
    fn from(e: pdms_audit::AuditError) -> Self {
        RunError::Core(e.into())
    }
}

impl From<RunError> for CoreError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Graph(g) => g.into(),
            RunError::Core(c) => c,
        }
    }
}

/// The Graph client and the resolved library, when SharePoint storage is on.
pub(crate) async fn parts(core: &Core) -> CoreResult<(&GraphClient, &Resolved)> {
    let sp: &SharePoint = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::Conflict("SharePoint storage is not turned on.".into()))?;
    let resolved = sp.resolved().await?;
    Ok((sp.graph(), resolved))
}

/// Record a successful run; `delta_link` replaces the stored link when given.
pub(crate) async fn record_ok(
    conn: &mut PgConnection,
    name: &str,
    delta_link: Option<&str>,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO sync_cursors (name, delta_link, last_run, last_error) VALUES ($1, $2, now(), NULL)
         ON CONFLICT (name) DO UPDATE
            SET delta_link = coalesce(EXCLUDED.delta_link, sync_cursors.delta_link),
                last_run = now(), last_error = NULL",
        name,
        delta_link
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Record a failed run (the stored delta link is kept).
pub(crate) async fn record_error(db: &PgPool, name: &str, message: &str) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO sync_cursors (name, last_run, last_error) VALUES ($1, now(), $2)
         ON CONFLICT (name) DO UPDATE SET last_run = now(), last_error = EXCLUDED.last_error",
        name,
        message
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Send `notice` to every org-wide administrator (inactive ones are skipped).
pub(crate) async fn notify_admins(
    conn: &mut PgConnection,
    core: &Core,
    notice: Notice,
) -> CoreResult<usize> {
    let admins: BTreeSet<Uuid> = sqlx::query_scalar!(
        "SELECT a.user_id FROM role_assignments a JOIN roles r ON r.id = a.role_id
          WHERE r.key = $1 AND a.department_id IS NULL",
        role_keys::ADMIN
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let ctx = Ctx::system("sharepoint");
    let about = AuditEvent::new(
        Category::System,
        "notification.created",
        ctx.actor.clone(),
        "system",
    );
    actions::send(conn, core, &ctx, notice, admins, &Visibility::Anyone, about).await
}

/// Record a failed run for the status panel. A Graph sign-in failure also
/// tells administrators, at most once an hour.
pub async fn report_failure(core: &Core, name: &str, err: &RunError) -> CoreResult<()> {
    let message: String = err.to_string().chars().take(500).collect();
    tracing::warn!(job = name, error = %message, "SharePoint background run failed");
    record_error(&core.db, name, &message).await?;
    if err.is_auth() {
        let mut tx = core.db.begin().await?;
        notify_admins(
            &mut tx,
            core,
            Notice {
                subject: "PDMS cannot sign in to Microsoft Graph".into(),
                body: format!(
                    "SharePoint storage and Entra sync are paused until this is fixed: {message}. \
                     Check the PDMS Service certificate and app registration (docs/SHAREPOINT_SETUP.md)."
                ),
                link: "/admin/sharepoint".into(),
                document_id: None,
                appointment_id: None,
                dedupe_key: format!("graph-auth:{}", Utc::now().format("%Y%m%d%H")),
            },
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// The notice administrators get when files were changed or deleted
/// directly in SharePoint. It names no document.
pub(crate) fn outside_changes_notice(count: u32) -> Notice {
    Notice {
        subject: format!("SharePoint: {count} file(s) changed outside PDMS"),
        body: "Someone edited or deleted files directly in the PDMS SharePoint site. \
               Review them under Administration → SharePoint."
            .into(),
        link: "/admin/sharepoint".into(),
        document_id: None,
        appointment_id: None,
        dedupe_key: format!("sharepoint-changes:{}", Uuid::now_v7()),
    }
}

/// Unresolved SharePoint flags on one document's files: (version number, state).
pub async fn document_flags(db: &PgPool, document_id: Uuid) -> CoreResult<Vec<(i32, String)>> {
    Ok(sqlx::query!(
        "SELECT v.version_no, f.state FROM sharepoint_files f
           JOIN document_versions v ON v.id = f.document_version_id
          WHERE f.document_id = $1 AND f.state <> 'ok' ORDER BY v.version_no",
        document_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.version_no, r.state))
    .collect())
}

#[derive(Clone, Debug, Serialize)]
pub struct UnexpectedFile {
    pub item_id: String,
    pub name: String,
    pub path: String,
    pub created_by: String,
    pub by_pdms: bool,
    pub first_seen: DateTime<Utc>,
}

/// Files nobody tracks, seen more than ten minutes ago and not dismissed.
pub async fn unexpected_files(db: &PgPool) -> CoreResult<Vec<UnexpectedFile>> {
    Ok(sqlx::query_as!(
        UnexpectedFile,
        "SELECT item_id, name, path, created_by, by_pdms, first_seen FROM sharepoint_unexpected_items
          WHERE dismissed_at IS NULL AND first_seen < now() - interval '10 minutes'
          ORDER BY first_seen DESC LIMIT 500"
    )
    .fetch_all(db)
    .await?)
}

#[derive(Clone, Debug, Serialize)]
pub struct RunStatus {
    pub name: String,
    pub last_run: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeadJob {
    pub kind: String,
    pub count: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlaggedFile {
    pub version_id: Uuid,
    pub document_id: Uuid,
    /// `None` when the viewer cannot open the document: then the panel shows
    /// only "A Restricted document" and the version (the owner's answer for
    /// P4, D-085), not even the ID.
    pub external_id: Option<String>,
    /// `None` when the viewer cannot open the document.
    pub title: Option<String>,
    /// The SharePoint path names the document, so it is hidden the same way.
    pub path: Option<String>,
    pub version_no: i32,
    pub state: String,
    pub changed_by: Option<String>,
    pub changed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ForeignGrant {
    pub department: String,
    pub granted_to: String,
    pub roles: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Panel {
    pub enabled: bool,
    pub site_url: String,
    pub client_id: String,
    pub runs: Vec<RunStatus>,
    pub dead_jobs: Vec<DeadJob>,
    pub flagged: Vec<FlaggedFile>,
    pub unexpected: Vec<UnexpectedFile>,
    pub foreign: Vec<ForeignGrant>,
}

/// Everything Administration → SharePoint shows (requires `ManageSharePoint`).
pub async fn panel(core: &Core, ctx: &Ctx) -> CoreResult<Panel> {
    ctx.require(Capability::ManageSharePoint)?;
    let names = vec![
        DELTA_CURSOR.to_owned(),
        RECONCILE_CURSOR.to_owned(),
        ENTRA_CURSOR.to_owned(),
    ];
    let runs = sqlx::query_as!(
        RunStatus,
        "SELECT name, last_run, last_error FROM sync_cursors WHERE name = ANY($1) ORDER BY name",
        &names
    )
    .fetch_all(&core.db)
    .await?;
    let dead_jobs = sqlx::query_as!(
        DeadJob,
        r#"SELECT kind, count(*) AS "count!", (array_agg(last_error ORDER BY updated_at DESC))[1] AS last_error
             FROM jobs
            WHERE status = 'dead' AND (kind LIKE 'sharepoint.%' OR kind LIKE 'directory.entra_sync%')
            GROUP BY kind ORDER BY kind"#
    )
    .fetch_all(&core.db)
    .await?;
    let rows = sqlx::query!(
        "SELECT f.document_version_id, f.document_id, d.external_id, d.title, v.version_no, f.state,
                f.changed_by, f.changed_at, f.last_known_path
           FROM sharepoint_files f
           JOIN document_versions v ON v.id = f.document_version_id
           JOIN documents d ON d.id = f.document_id
          WHERE f.state <> 'ok'
          ORDER BY f.changed_at DESC NULLS LAST LIMIT 500"
    )
    .fetch_all(&core.db)
    .await?;
    let mut flagged = Vec::with_capacity(rows.len());
    for r in rows {
        let visible =
            crate::documents::access::can(core, ctx, r.document_id, Permission::View).await?;
        flagged.push(FlaggedFile {
            version_id: r.document_version_id,
            document_id: r.document_id,
            external_id: visible.then_some(r.external_id),
            title: visible.then_some(r.title),
            path: visible.then_some(r.last_known_path),
            version_no: r.version_no,
            state: r.state,
            changed_by: r.changed_by,
            changed_at: r.changed_at,
        });
    }
    let foreign = sqlx::query_as!(
        ForeignGrant,
        "SELECT d.name AS department, g.granted_to, g.roles
           FROM sharepoint_foreign_grants g JOIN departments d ON d.id = g.department_id
          ORDER BY d.name, g.granted_to"
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Panel {
        enabled: core.sharepoint.is_some(),
        site_url: core.config.sharepoint.site_url.clone(),
        client_id: core.config.graph.client_id.clone(),
        runs,
        dead_jobs,
        flagged,
        unexpected: unexpected_files(&core.db).await?,
        foreign,
    })
}
