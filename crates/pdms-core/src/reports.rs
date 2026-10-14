// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Reports. Every report is a table that can be viewed or exported to CSV
//! (exports are audited). Document reports are built from the authorized
//! search, so they contain only what the viewer may see; access and audit
//! reports require their capabilities.

use std::collections::{BTreeMap, HashMap};

use chrono::{Duration, NaiveDate, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::{Capability, Permission};
use serde::Serialize;

use crate::Core;
use crate::calendar;
use crate::context::Ctx;
use crate::documents::DocumentStatus;
use crate::documents::search::{self, DocumentFilter, DocumentSummary};
use crate::error::{CoreError, CoreResult};
use crate::schedule;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReportInfo {
    pub key: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    #[serde(skip)]
    pub requires: Option<Capability>,
    /// Which parameters apply.
    pub uses_days: bool,
    pub uses_dates: bool,
}

pub const REPORTS: &[ReportInfo] = &[
    ReportInfo {
        key: "upcoming",
        title: "Upcoming reviews, renewals, and expirations",
        description: "Dates in the next N days, including appointment term ends.",
        requires: None,
        uses_days: true,
        uses_dates: false,
    },
    ReportInfo {
        key: "overdue",
        title: "Overdue items",
        description: "Reviews, renewals, and appointment decisions that are past due, with missed review cycles.",
        requires: None,
        uses_days: false,
        uses_dates: false,
    },
    ReportInfo {
        key: "by_department",
        title: "Documents by department",
        description: "Counts by department and status.",
        requires: None,
        uses_days: false,
        uses_dates: false,
    },
    ReportInfo {
        key: "by_owner",
        title: "Documents by owner",
        description: "Counts by primary owner and status.",
        requires: None,
        uses_days: false,
        uses_dates: false,
    },
    ReportInfo {
        key: "by_tag",
        title: "Documents by tag",
        description: "Counts by tag and status (a document with several tags counts under each).",
        requires: None,
        uses_days: false,
        uses_dates: false,
    },
    ReportInfo {
        key: "ownership",
        title: "Ownership",
        description: "Every document's primary owner, flagging owners who are no longer active.",
        requires: None,
        uses_days: false,
        uses_dates: false,
    },
    ReportInfo {
        key: "access_grants",
        title: "Access review: grants",
        description: "Who has what access. Flags grants older than N days, expired grants, and grants held by inactive or departed users.",
        requires: Some(Capability::ViewAccessReports),
        uses_days: true,
        uses_dates: false,
    },
    ReportInfo {
        key: "access_roles",
        title: "Access review: role assignments",
        description: "Every role held, org-wide or per department, flagging inactive or departed holders.",
        requires: Some(Capability::ViewAccessReports),
        uses_days: true,
        uses_dates: false,
    },
    ReportInfo {
        key: "orphaned_accounts",
        title: "Access review: orphaned and departed accounts",
        description: "Disabled or departed users who still own documents, hold grants or roles, or have live API tokens.",
        requires: Some(Capability::ViewAccessReports),
        uses_days: false,
        uses_dates: false,
    },
    ReportInfo {
        key: "audit_summary",
        title: "Audit summary",
        description: "Audit entries by category and action in a date range, with denials and failures.",
        requires: Some(Capability::ViewAudit),
        uses_days: false,
        uses_dates: true,
    },
    ReportInfo {
        key: "audit_denials",
        title: "Denied access attempts",
        description: "Every denied action in a date range.",
        requires: Some(Capability::ViewAudit),
        uses_days: false,
        uses_dates: true,
    },
    ReportInfo {
        key: "records_requests",
        title: "Records requests",
        description: "Export packages built in a date range, with their request numbers and hashes.",
        requires: Some(Capability::ViewAudit),
        uses_days: false,
        uses_dates: true,
    },
];

pub fn info(key: &str) -> Option<&'static ReportInfo> {
    REPORTS.iter().find(|r| r.key == key)
}

/// Reports the caller may run.
pub fn available(ctx: &Ctx) -> Vec<&'static ReportInfo> {
    REPORTS
        .iter()
        .filter(|r| r.requires.is_none_or(|c| ctx.can(c)))
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct ReportParams {
    /// Horizon for "upcoming"; age threshold for access reviews.
    pub days: Option<i64>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub department_id: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub key: &'static str,
    pub title: &'static str,
    pub subtitle: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

fn cols(c: &[&str]) -> Vec<String> {
    c.iter().map(|s| (*s).to_owned()).collect()
}

const MAX_DOCS: i64 = 20_000;

async fn docs(core: &Core, ctx: &Ctx, p: &ReportParams) -> CoreResult<Vec<DocumentSummary>> {
    let f = DocumentFilter {
        department_id: p.department_id,
        include_archived: true,
        ..DocumentFilter::default()
    };
    search::all(core, ctx, &f, Permission::View, MAX_DOCS).await
}

fn grouped<'a>(
    docs: &'a [DocumentSummary],
    keys: impl Fn(&'a DocumentSummary) -> Vec<String>,
    label: &str,
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut counts: BTreeMap<String, HashMap<DocumentStatus, usize>> = BTreeMap::new();
    for doc in docs {
        for k in keys(doc) {
            *counts.entry(k).or_default().entry(doc.status).or_default() += 1;
        }
    }
    let mut columns = vec![label.to_owned(), "Total".to_owned()];
    columns.extend(DocumentStatus::ALL.iter().map(|s| s.label().to_owned()));
    let rows = counts
        .into_iter()
        .map(|(k, by)| {
            let mut row = vec![k, by.values().sum::<usize>().to_string()];
            row.extend(
                DocumentStatus::ALL
                    .iter()
                    .map(|s| by.get(s).copied().unwrap_or(0).to_string()),
            );
            row
        })
        .collect();
    (columns, rows)
}

/// Run a report as the caller.
pub async fn run(core: &Core, ctx: &Ctx, key: &str, p: &ReportParams) -> CoreResult<Report> {
    let info = info(key).ok_or(CoreError::NotFound)?;
    if let Some(cap) = info.requires {
        ctx.require(cap)?;
    }
    let tz = core.config.timezone();
    let today = schedule::local_today(Utc::now(), tz);
    let days = p.days.unwrap_or(90).clamp(1, 3660);
    let from = p.from.unwrap_or(today - Duration::days(30));
    let to = p.to.unwrap_or(today);
    let (subtitle, columns, rows) = match key {
        "upcoming" => {
            let f = DocumentFilter {
                department_id: p.department_id,
                ..DocumentFilter::default()
            };
            let ev =
                calendar::events(core, ctx, &f, today, today + Duration::days(days), false).await?;
            (
                format!("{today} to {}", today + Duration::days(days)),
                cols(&[
                    "Date",
                    "What",
                    "ID or board",
                    "Title",
                    "Department",
                    "Type",
                    "Status",
                ]),
                ev.into_iter()
                    .map(|e| {
                        vec![
                            e.date.to_string(),
                            e.label().to_owned(),
                            e.external_id.clone(),
                            e.title.clone(),
                            e.department.clone(),
                            e.type_name.clone(),
                            e.status.map(|s| s.label().to_owned()).unwrap_or_default(),
                        ]
                    })
                    .collect(),
            )
        }
        "overdue" => {
            let f = DocumentFilter {
                department_id: p.department_id,
                ..DocumentFilter::default()
            };
            let dash = calendar::dashboard(core, ctx, &f, 1).await?;
            (
                format!("As of {today}"),
                cols(&[
                    "What",
                    "ID or board",
                    "Title",
                    "Department",
                    "Was due",
                    "Days overdue",
                    "Missed review cycles",
                ]),
                dash.overdue
                    .into_iter()
                    .map(|o| {
                        vec![
                            o.event.label().to_owned(),
                            o.event.external_id.clone(),
                            o.event.title.clone(),
                            o.event.department.clone(),
                            o.event.date.to_string(),
                            o.days_overdue.to_string(),
                            o.missed_cycles.len().to_string(),
                        ]
                    })
                    .collect(),
            )
        }
        "by_department" | "by_owner" | "by_tag" => {
            let all = docs(core, ctx, p).await?;
            let (columns, rows) = match key {
                "by_department" => grouped(&all, |d| vec![d.department_name.clone()], "Department"),
                "by_owner" => grouped(&all, |d| vec![d.owner_name.clone()], "Owner"),
                _ => grouped(
                    &all,
                    |d| {
                        if d.tags.0.is_empty() {
                            vec!["(no tags)".to_owned()]
                        } else {
                            d.tags.0.iter().map(|t| t.name.clone()).collect()
                        }
                    },
                    "Tag",
                ),
            };
            (
                format!("{} documents you can view", all.len()),
                columns,
                rows,
            )
        }
        "ownership" => {
            let all = docs(core, ctx, p).await?;
            let ids: Vec<uuid::Uuid> = all.iter().map(|d| d.id).collect();
            let owners = sqlx::query!(
                r#"SELECT d.id, u.email, u.status::text AS "status!",
                          EXISTS (SELECT 1 FROM department_members m WHERE m.user_id = u.id
                                     AND m.department_id = d.department_id) AS "in_department!"
                     FROM documents d JOIN users u ON u.id = d.owner_id WHERE d.id = ANY($1)"#,
                &ids
            )
            .fetch_all(&core.db)
            .await?;
            let by_id: BTreeMap<uuid::Uuid, _> = owners.into_iter().map(|o| (o.id, o)).collect();
            (
                format!("{} documents you can view", all.len()),
                cols(&[
                    "Document ID",
                    "Title",
                    "Department",
                    "Status",
                    "Owner",
                    "Owner email",
                    "Owner account",
                    "Owner in department",
                    "Flag",
                ]),
                all.iter()
                    .map(|doc| {
                        let o = by_id.get(&doc.id);
                        let status = o.map(|o| o.status.clone()).unwrap_or_default();
                        let in_dept = o.is_some_and(|o| o.in_department);
                        let flag = if status != "active" {
                            "Owner is not active: transfer ownership"
                        } else if !in_dept {
                            "Owner is outside the document's department"
                        } else {
                            ""
                        };
                        vec![
                            doc.external_id.clone(),
                            doc.title.clone(),
                            doc.department_name.clone(),
                            doc.status.label().to_owned(),
                            doc.owner_name.clone(),
                            o.map(|o| o.email.clone()).unwrap_or_default(),
                            status,
                            if in_dept { "yes" } else { "no" }.to_owned(),
                            flag.to_owned(),
                        ]
                    })
                    .collect(),
            )
        }
        "access_grants" => {
            let rows = sqlx::query!(
                r#"SELECT a.id, a.scope::text AS "scope!", a.permission::text AS "permission!", a.granted_at, a.expires_at,
                          a.reason,
                          CASE WHEN a.subject_user_id IS NOT NULL THEN 'User'
                               WHEN a.subject_group_id IS NOT NULL THEN 'Group'
                               WHEN a.subject_department_id IS NOT NULL THEN 'Department'
                               ELSE 'Role' END AS "subject_kind!",
                          coalesce(su.display_name, sg.name, sd.name, sr.name, '?') AS "subject!",
                          su.status::text AS subject_status,
                          coalesce(td.name, doc.external_id || ' ' || doc.title, 'Everything (organization-wide)') AS "target!",
                          doc.deleted_at IS NOT NULL AS "doc_deleted!",
                          gb.display_name AS "granted_by?"
                     FROM acl_entries a
                     LEFT JOIN users su ON su.id = a.subject_user_id
                     LEFT JOIN groups sg ON sg.id = a.subject_group_id
                     LEFT JOIN departments sd ON sd.id = a.subject_department_id
                     LEFT JOIN roles sr ON sr.id = a.subject_role_id
                     LEFT JOIN departments td ON td.id = a.scope_department_id
                     LEFT JOIN documents doc ON doc.id = a.scope_document_id
                     LEFT JOIN users gb ON gb.id = a.granted_by
                    WHERE ($1::uuid IS NULL OR a.scope_department_id = $1 OR doc.department_id = $1)
                    ORDER BY a.granted_at"#,
                p.department_id
            )
            .fetch_all(&core.db)
            .await?;
            let now = Utc::now();
            (
                format!("Flags grants older than {}", days_label(days)),
                cols(&[
                    "Subject type",
                    "Subject",
                    "Permission",
                    "Scope",
                    "Applies to",
                    "Granted",
                    "Age (days)",
                    "Granted by",
                    "Expires",
                    "Reason",
                    "Flags",
                ]),
                rows.into_iter()
                    .map(|r| {
                        let age = (now - r.granted_at).num_days();
                        let mut flags = Vec::new();
                        if age > days {
                            flags.push(format!("older than {}", days_label(days)));
                        }
                        if r.expires_at.is_some_and(|e| e <= now) {
                            flags.push("expired".into());
                        }
                        if let Some(s) = r.subject_status.as_deref().filter(|s| *s != "active") {
                            flags.push(format!("user is {s}"));
                        }
                        if r.doc_deleted {
                            flags.push("document deleted".into());
                        }
                        vec![
                            r.subject_kind,
                            r.subject,
                            r.permission,
                            r.scope,
                            r.target,
                            r.granted_at.with_timezone(&tz).date_naive().to_string(),
                            age.to_string(),
                            r.granted_by.unwrap_or_else(|| "System".into()),
                            r.expires_at
                                .map(|e| e.with_timezone(&tz).date_naive().to_string())
                                .unwrap_or_default(),
                            r.reason,
                            flags.join("; "),
                        ]
                    })
                    .collect(),
            )
        }
        "access_roles" => {
            let rows = sqlx::query!(
                r#"SELECT r.name AS role, u.display_name, u.email, u.status::text AS "status!",
                          d.name AS "department?", a.source::text AS "source!", a.granted_at
                     FROM role_assignments a
                     JOIN roles r ON r.id = a.role_id
                     JOIN users u ON u.id = a.user_id
                     LEFT JOIN departments d ON d.id = a.department_id
                    WHERE ($1::uuid IS NULL OR a.department_id = $1 OR a.department_id IS NULL)
                    ORDER BY r.name, u.display_name"#,
                p.department_id
            )
            .fetch_all(&core.db)
            .await?;
            let now = Utc::now();
            (
                format!("Flags assignments older than {}", days_label(days)),
                cols(&[
                    "Role",
                    "User",
                    "Email",
                    "Account",
                    "Applies to",
                    "Source",
                    "Assigned",
                    "Age (days)",
                    "Flags",
                ]),
                rows.into_iter()
                    .map(|r| {
                        let age = (now - r.granted_at).num_days();
                        let mut flags = Vec::new();
                        if age > days {
                            flags.push(format!("older than {}", days_label(days)));
                        }
                        if r.status != "active" {
                            flags.push(format!("user is {}", r.status));
                        }
                        vec![
                            r.role,
                            r.display_name,
                            r.email,
                            r.status,
                            r.department.unwrap_or_else(|| "Organization-wide".into()),
                            r.source,
                            r.granted_at.with_timezone(&tz).date_naive().to_string(),
                            age.to_string(),
                            flags.join("; "),
                        ]
                    })
                    .collect(),
            )
        }
        "orphaned_accounts" => {
            let rows = sqlx::query!(
                r#"SELECT u.display_name, u.email, u.status::text AS "status!",
                          (SELECT count(*) FROM documents d WHERE d.owner_id = u.id AND d.deleted_at IS NULL) AS "owned!",
                          (SELECT count(*) FROM acl_entries a WHERE a.subject_user_id = u.id) AS "grants!",
                          (SELECT count(*) FROM role_assignments r WHERE r.user_id = u.id) AS "roles!",
                          (SELECT count(*) FROM api_tokens t WHERE t.user_id = u.id AND t.revoked_at IS NULL
                              AND t.expires_at > now()) AS "tokens!",
                          (SELECT count(*) FROM positions p WHERE p.manager_user_id = u.id AND p.is_active) AS "positions!"
                     FROM users u
                    WHERE u.status <> 'active'
                    ORDER BY u.status, u.display_name"#
            )
            .fetch_all(&core.db)
            .await?;
            (
                "Disabled and departed accounts with anything still attached".to_owned(),
                cols(&[
                    "User",
                    "Email",
                    "Account",
                    "Documents owned",
                    "Direct grants",
                    "Roles",
                    "Live API tokens",
                    "Positions managed",
                ]),
                rows.into_iter()
                    .filter(|r| r.owned + r.grants + r.roles + r.tokens + r.positions > 0)
                    .map(|r| {
                        vec![
                            r.display_name,
                            r.email,
                            r.status,
                            r.owned.to_string(),
                            r.grants.to_string(),
                            r.roles.to_string(),
                            r.tokens.to_string(),
                            r.positions.to_string(),
                        ]
                    })
                    .collect(),
            )
        }
        "audit_summary" => {
            let (a, b) = range(from, to, tz);
            let rows = sqlx::query!(
                r#"SELECT category::text AS "category!", action, count(*) AS "total!",
                          count(*) FILTER (WHERE outcome = 'denied') AS "denied!",
                          count(*) FILTER (WHERE outcome = 'failure') AS "failed!",
                          count(DISTINCT actor_user_id) AS "people!"
                     FROM audit_log WHERE occurred_at >= $1 AND occurred_at < $2
                    GROUP BY category, action ORDER BY category, action"#,
                a,
                b
            )
            .fetch_all(&core.db)
            .await?;
            (
                format!("{from} to {to}"),
                cols(&[
                    "Category", "Action", "Entries", "Denied", "Failed", "People",
                ]),
                rows.into_iter()
                    .map(|r| {
                        vec![
                            r.category,
                            r.action,
                            r.total.to_string(),
                            r.denied.to_string(),
                            r.failed.to_string(),
                            r.people.to_string(),
                        ]
                    })
                    .collect(),
            )
        }
        "audit_denials" => {
            let (a, b) = range(from, to, tz);
            let rows = sqlx::query!(
                r#"SELECT seq, occurred_at, action, actor_display, host(actor_ip) AS ip, object_type, object_label,
                          context->>'required_permission' AS required
                     FROM audit_log WHERE outcome = 'denied' AND occurred_at >= $1 AND occurred_at < $2
                    ORDER BY seq LIMIT 20000"#,
                a,
                b
            )
            .fetch_all(&core.db)
            .await?;
            (
                format!("{from} to {to}"),
                cols(&[
                    "Seq",
                    "When",
                    "Action",
                    "Who",
                    "IP",
                    "Object type",
                    "Object",
                    "Needed",
                ]),
                rows.into_iter()
                    .map(|r| {
                        vec![
                            r.seq.to_string(),
                            r.occurred_at
                                .with_timezone(&tz)
                                .format("%Y-%m-%d %H:%M %Z")
                                .to_string(),
                            r.action,
                            r.actor_display.unwrap_or_default(),
                            r.ip.unwrap_or_default(),
                            r.object_type,
                            r.object_label.unwrap_or_default(),
                            r.required.unwrap_or_default(),
                        ]
                    })
                    .collect(),
            )
        }
        "records_requests" => {
            let (a, b) = range(from, to, tz);
            let rows = sqlx::query!(
                r#"SELECT p.request_reference, p.created_at, u.display_name, cardinality(p.document_ids) AS "docs!",
                          p.include_versions, p.include_audit, p.file_count, p.size_bytes, p.package_sha256
                     FROM export_packages p JOIN users u ON u.id = p.requested_by
                    WHERE p.created_at >= $1 AND p.created_at < $2 ORDER BY p.created_at"#,
                a,
                b
            )
            .fetch_all(&core.db)
            .await?;
            (
                format!("{from} to {to}"),
                cols(&[
                    "Request",
                    "Built",
                    "By",
                    "Documents",
                    "Versions",
                    "Audit history",
                    "Files",
                    "Bytes",
                    "Package SHA-256",
                ]),
                rows.into_iter()
                    .map(|r| {
                        vec![
                            r.request_reference,
                            r.created_at
                                .with_timezone(&tz)
                                .format("%Y-%m-%d %H:%M %Z")
                                .to_string(),
                            r.display_name,
                            r.docs.to_string(),
                            r.include_versions,
                            if r.include_audit { "yes" } else { "no" }.to_owned(),
                            r.file_count.to_string(),
                            r.size_bytes.to_string(),
                            hex::encode(r.package_sha256),
                        ]
                    })
                    .collect(),
            )
        }
        _ => return Err(CoreError::NotFound),
    };
    Ok(Report {
        key: info.key,
        title: info.title,
        subtitle,
        columns,
        rows,
    })
}

fn days_label(days: i64) -> String {
    if days == 1 {
        "1 day".into()
    } else {
        format!("{days} days")
    }
}

/// `[from 00:00, to + 1 day 00:00)` in the organization's zone, as UTC.
fn range(
    from: NaiveDate,
    to: NaiveDate,
    tz: chrono_tz::Tz,
) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let at = |d: NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .and_then(|t| t.and_local_timezone(tz).earliest())
            .map_or_else(Utc::now, |t| t.with_timezone(&Utc))
    };
    (at(from), at(to + Duration::days(1)))
}

/// CSV with a header row. Cells that a spreadsheet would treat as a formula
/// are prefixed with `'`.
pub fn to_csv(r: &Report) -> Vec<u8> {
    let cell = |s: &str| {
        let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            format!("'{s}")
        } else {
            s.to_owned()
        };
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s
        }
    };
    let mut out = String::new();
    for row in std::iter::once(&r.columns).chain(r.rows.iter()) {
        out.push_str(&row.iter().map(|c| cell(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// Run a report and return it as CSV (audited).
pub async fn export_csv(
    core: &Core,
    ctx: &Ctx,
    key: &str,
    p: &ReportParams,
) -> CoreResult<Vec<u8>> {
    let r = run(core, ctx, key, p).await?;
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Access,
                "report.exported",
                ctx.actor.clone(),
                "report",
            )
            .object(r.key, Some(r.title))
            .context("rows", i64::try_from(r.rows.len()).unwrap_or(i64::MAX))
            .context("subtitle", r.subtitle.clone()),
        ],
    )
    .await?;
    Ok(to_csv(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_and_neutralizes_formulas() {
        let r = Report {
            key: "x",
            title: "x",
            subtitle: String::new(),
            columns: cols(&["A", "B"]),
            rows: vec![vec!["=HYPERLINK(\"x\")".into(), "a, \"b\"".into()]],
        };
        let s = String::from_utf8(to_csv(&r)).unwrap();
        assert_eq!(s, "A,B\r\n\"'=HYPERLINK(\"\"x\"\")\",\"a, \"\"b\"\"\"\r\n");
    }

    #[test]
    fn every_report_key_is_unique() {
        let mut keys: Vec<&str> = REPORTS.iter().map(|r| r.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), REPORTS.len());
    }
}
