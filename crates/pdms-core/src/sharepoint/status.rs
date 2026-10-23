// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! SharePoint background work: run records for the status panel (in P2's
//! `sync_cursors`), notices to administrators, and the queries behind
//! Administration → SharePoint.

use std::collections::BTreeSet;
use std::fmt;

use chrono::Utc;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::role_keys;
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
