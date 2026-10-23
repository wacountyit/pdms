// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Read access on `Departments/<department>/` folders (spec §7.4, D-084).
//! PDMS grants each active member of a department who came from Entra read
//! access on that department's folder, one person at a time, and removes it
//! when they leave or are deactivated. Permissions PDMS did not grant are
//! reported, never changed.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::Area;
use super::status::{self, RECONCILE_CURSOR, RunError};
use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;
use crate::graph::{GraphClient, GraphError};
use crate::jobs::{self, NewJob};

pub const RECONCILE_JOB: &str = "sharepoint.reconcile_access";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub departments: u32,
    pub granted: u32,
    pub revoked: u32,
    pub foreign: u32,
    pub errors: Vec<String>,
}

/// Every change in one minute shares one run, scheduled 5 seconds after
/// that minute ends, so the run sees all of them. Returns (key, run at).
pub fn reconcile_slot(now: i64) -> (String, i64) {
    let bucket = now.div_euclid(60);
    (format!("{RECONCILE_JOB}:{bucket}"), (bucket + 1) * 60 + 5)
}

/// Queue a folder-access run (cheap; harmless without SharePoint).
pub async fn enqueue_reconcile<'e>(exec: impl PgExecutor<'e>) -> CoreResult<()> {
    let (key, at) = reconcile_slot(Utc::now().timestamp());
    let run_at = DateTime::from_timestamp(at, 0).unwrap_or_else(Utc::now);
    jobs::enqueue(
        exec,
        NewJob::new(RECONCILE_JOB, json!({}))
            .idempotency_key(key)
            .run_at(run_at)
            .max_attempts(6),
    )
    .await?;
    Ok(())
}

/// For one folder: who to add (user, email) and who to remove (user,
/// permission id).
pub type Plan = (Vec<(Uuid, String)>, Vec<(Uuid, String)>);

/// Who to add (user, email) and who to remove (user, permission id) on one folder.
pub fn plan(desired: &BTreeMap<Uuid, String>, granted: &BTreeMap<Uuid, String>) -> Plan {
    let add = desired
        .iter()
        .filter(|(u, _)| !granted.contains_key(u))
        .map(|(u, e)| (*u, e.clone()))
        .collect();
    let remove = granted
        .iter()
        .filter(|(u, _)| !desired.contains_key(u))
        .map(|(u, p)| (*u, p.clone()))
        .collect();
    (add, remove)
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    value: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPermission {
    id: String,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    inherited_from: Option<Value>,
    #[serde(default)]
    granted_to_v2: Option<Value>,
    #[serde(default)]
    link: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

impl GraphPermission {
    fn grantee(&self) -> String {
        let g = self.granted_to_v2.as_ref();
        for k in ["user", "siteGroup", "group", "siteUser", "application"] {
            let name = g
                .and_then(|g| g.get(k))
                .and_then(|x| x.get("displayName").or_else(|| x.get("email")))
                .and_then(Value::as_str);
            if let Some(n) = name {
                return n.to_owned();
            }
        }
        if self.link.is_some() {
            "a sharing link".into()
        } else {
            "unknown".into()
        }
    }

    /// Who and with which roles, to compare a folder's access with the site's.
    fn key(&self) -> String {
        format!("{} {}", self.grantee(), self.roles.join(","))
    }
}

/// Bring every department folder's readers in line with PDMS (one run at a
/// time): departments with Entra members, with access PDMS granted, or with
/// a folder already (so access added by hand is still reported). A no-op
/// when SharePoint storage is off.
pub async fn reconcile(core: &Core) -> CoreResult<ReconcileReport> {
    if core.sharepoint.is_none() {
        return Ok(ReconcileReport::default());
    }
    match run(core).await {
        Ok(r) => Ok(r),
        Err(e) => {
            status::report_failure(core, RECONCILE_CURSOR, &e).await?;
            Err(e.into())
        }
    }
}

async fn run(core: &Core) -> Result<ReconcileReport, RunError> {
    let (graph, resolved) = status::parts(core).await?;
    let drive = resolved.drive_id.clone();
    let mut lock = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pdms.sharepoint_reconcile'))")
        .execute(&mut *lock)
        .await?;
    let departments = sqlx::query!(
        r#"SELECT d.id, d.name FROM departments d
            WHERE EXISTS (SELECT 1 FROM department_members m JOIN users u ON u.id = m.user_id
                           WHERE m.department_id = d.id AND u.status = 'active' AND u.source = 'entra')
               OR EXISTS (SELECT 1 FROM sharepoint_folder_access a WHERE a.department_id = d.id)
               OR EXISTS (SELECT 1 FROM sharepoint_department_folders f
                           WHERE f.department_id = d.id AND f.area = 'departments')
            ORDER BY d.name"#
    )
    .fetch_all(&core.db)
    .await?;
    let mut report = ReconcileReport::default();
    // What department folders inherit from the site (its own groups). The
    // first invite on a folder breaks inheritance, and SharePoint copies these
    // onto it as direct permissions; they are not "other access".
    let area: Page<GraphPermission> = graph
        .get(&format!(
            "/drives/{drive}/items/{}/permissions",
            resolved.departments_id
        ))
        .await?;
    let inherited: BTreeSet<String> = area.value.iter().map(GraphPermission::key).collect();
    for d in departments {
        report.departments += 1;
        if let Err(e) =
            department(core, graph, &drive, &inherited, d.id, &d.name, &mut report).await
        {
            if e.is_auth() {
                return Err(e);
            }
            report.errors.push(format!("{}: {e}", d.name));
        }
    }
    match report.errors.first() {
        None => {
            let mut conn = core.db.acquire().await?;
            status::record_ok(&mut conn, RECONCILE_CURSOR, None).await?;
        }
        Some(first) => {
            let message = format!("{} problem(s); first: {first}", report.errors.len());
            status::record_error(&core.db, RECONCILE_CURSOR, &message).await?;
        }
    }
    lock.commit().await?;
    Ok(report)
}

async fn department(
    core: &Core,
    graph: &GraphClient,
    drive: &str,
    inherited: &BTreeSet<String>,
    department_id: Uuid,
    name: &str,
    report: &mut ReconcileReport,
) -> Result<(), RunError> {
    let folder = super::department_folder_id(core, Area::Departments, department_id).await?;
    let desired: BTreeMap<Uuid, String> = sqlx::query!(
        "SELECT u.id, u.email FROM department_members m JOIN users u ON u.id = m.user_id
          WHERE m.department_id = $1 AND u.status = 'active' AND u.source = 'entra'",
        department_id
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r.email))
    .collect();
    let granted: BTreeMap<Uuid, String> = sqlx::query!(
        "SELECT user_id, permission_id FROM sharepoint_folder_access WHERE department_id = $1",
        department_id
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.permission_id))
    .collect();
    let (add, remove) = plan(&desired, &granted);
    let ctx = Ctx::system("sharepoint-access");
    let place = format!("Departments/{name}");

    for (user_id, email) in add {
        let body = json!({
            "recipients": [{"email": email}],
            "roles": ["read"],
            "requireSignIn": true,
            "sendInvitation": false,
        });
        let resp: Page<GraphPermission> = match graph
            .post(&format!("/drives/{drive}/items/{folder}/invite"), &body)
            .await
        {
            Ok(r) => r,
            Err(e @ GraphError::Auth(_)) => return Err(e.into()),
            Err(e) => {
                report
                    .errors
                    .push(format!("{name}: could not grant {email}: {e}"));
                continue;
            }
        };
        let Some(p) = resp.value.into_iter().find(|p| p.error.is_none()) else {
            report
                .errors
                .push(format!("{name}: could not grant {email}"));
            continue;
        };
        let mut tx = core.db.begin().await?;
        // Replace any earlier record (the app may insert and delete these
        // rows, not update them).
        sqlx::query!(
            "DELETE FROM sharepoint_folder_access WHERE department_id = $1 AND user_id = $2",
            department_id,
            user_id
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO sharepoint_folder_access (department_id, user_id, permission_id) VALUES ($1, $2, $3)",
            department_id,
            user_id,
            p.id
        )
        .execute(&mut *tx)
        .await?;
        pdms_audit::append(
            &mut tx,
            vec![
                AuditEvent::new(
                    Category::Permission,
                    "sharepoint.access_granted",
                    ctx.actor.clone(),
                    "user",
                )
                .object(user_id, Some(&email))
                .department(Some(department_id))
                .context("folder", place.as_str())
                .context("permission_id", p.id.as_str()),
            ],
        )
        .await?;
        tx.commit().await?;
        report.granted += 1;
    }

    for (user_id, permission) in remove {
        match graph
            .delete(&format!(
                "/drives/{drive}/items/{folder}/permissions/{permission}"
            ))
            .await
        {
            Ok(()) | Err(GraphError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let mut tx = core.db.begin().await?;
        sqlx::query!(
            "DELETE FROM sharepoint_folder_access WHERE department_id = $1 AND user_id = $2",
            department_id,
            user_id
        )
        .execute(&mut *tx)
        .await?;
        pdms_audit::append(
            &mut tx,
            vec![
                AuditEvent::new(
                    Category::Permission,
                    "sharepoint.access_revoked",
                    ctx.actor.clone(),
                    "user",
                )
                .object(user_id, None)
                .department(Some(department_id))
                .context("folder", place.as_str())
                .context("permission_id", permission.as_str()),
            ],
        )
        .await?;
        tx.commit().await?;
        report.revoked += 1;
    }

    // Report access PDMS did not grant.
    let mine: BTreeSet<String> = sqlx::query_scalar!(
        "SELECT permission_id FROM sharepoint_folder_access WHERE department_id = $1",
        department_id
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .collect();
    let all: Page<GraphPermission> = graph
        .get(&format!("/drives/{drive}/items/{folder}/permissions"))
        .await?;
    let foreign: Vec<GraphPermission> = all
        .value
        .into_iter()
        .filter(|p| {
            p.inherited_from.is_none() && !mine.contains(&p.id) && !inherited.contains(&p.key())
        })
        .collect();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "DELETE FROM sharepoint_foreign_grants WHERE department_id = $1",
        department_id
    )
    .execute(&mut *tx)
    .await?;
    for p in &foreign {
        sqlx::query!(
            "INSERT INTO sharepoint_foreign_grants (department_id, permission_id, granted_to, roles)
             VALUES ($1, $2, $3, $4)",
            department_id,
            p.id,
            p.grantee(),
            p.roles.join(", ")
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    report.foreign += u32::try_from(foreign.len()).unwrap_or(u32::MAX);
    Ok(())
}
