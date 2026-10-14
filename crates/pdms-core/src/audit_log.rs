// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Reading, exporting, and verifying the audit log. Each of these is itself audited.

use chrono::{DateTime, Utc};
use pdms_audit::{
    AUDIT_COLUMNS, AuditEvent, AuditRow, AuditValue, Category, Outcome, SealedEvent, VerifyReport,
    format_timestamp,
};
use pdms_authz::Capability;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

pub const MAX_PAGE: i64 = 500;
pub const MAX_EXPORT_ROWS: i64 = 100_000;

#[derive(Clone, Debug, Default)]
pub struct AuditFilter {
    pub actor_user_id: Option<Uuid>,
    /// Matches actions equal to or starting with this (e.g. `auth.` or `document.viewed`).
    pub action_prefix: Option<String>,
    pub category: Option<Category>,
    pub outcome: Option<Outcome>,
    pub object_type: Option<String>,
    pub object_id: Option<String>,
    pub document_id: Option<Uuid>,
    pub department_id: Option<Uuid>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Keyset pagination: only entries with `seq` below this.
    pub before_seq: Option<i64>,
}

impl AuditFilter {
    fn describe(&self) -> AuditValue {
        let mut m = std::collections::BTreeMap::new();
        let mut put = |k: &str, v: AuditValue| {
            if v != AuditValue::Null {
                m.insert(k.to_owned(), v);
            }
        };
        put("actor_user_id", self.actor_user_id.into());
        put("action_prefix", self.action_prefix.clone().into());
        put("category", self.category.map(|c| c.as_str()).into());
        put("outcome", self.outcome.map(|o| o.as_str()).into());
        put("object_type", self.object_type.clone().into());
        put("object_id", self.object_id.clone().into());
        put("document_id", self.document_id.into());
        put("department_id", self.department_id.into());
        put("from", self.from.into());
        put("to", self.to.into());
        put("before_seq", self.before_seq.into());
        AuditValue::Map(m)
    }

    fn query(&self, limit: i64) -> QueryBuilder<Postgres> {
        let mut qb = QueryBuilder::new(format!("SELECT {AUDIT_COLUMNS} FROM audit_log WHERE TRUE"));
        if let Some(v) = self.actor_user_id {
            qb.push(" AND actor_user_id = ").push_bind(v);
        }
        if let Some(v) = &self.action_prefix {
            qb.push(" AND action LIKE ")
                .push_bind(format!("{}%", crate::directory::escape_like(v)));
        }
        if let Some(v) = self.category {
            qb.push(" AND category = ").push_bind(v);
        }
        if let Some(v) = self.outcome {
            qb.push(" AND outcome = ").push_bind(v);
        }
        if let Some(v) = &self.object_type {
            qb.push(" AND object_type = ").push_bind(v.clone());
        }
        if let Some(v) = &self.object_id {
            qb.push(" AND object_id = ").push_bind(v.clone());
        }
        if let Some(v) = self.document_id {
            qb.push(" AND document_id = ").push_bind(v);
        }
        if let Some(v) = self.department_id {
            qb.push(" AND department_id = ").push_bind(v);
        }
        if let Some(v) = self.from {
            qb.push(" AND occurred_at >= ").push_bind(v);
        }
        if let Some(v) = self.to {
            qb.push(" AND occurred_at < ").push_bind(v);
        }
        if let Some(v) = self.before_seq {
            qb.push(" AND seq < ").push_bind(v);
        }
        qb.push(" ORDER BY seq DESC LIMIT ").push_bind(limit);
        qb
    }
}

pub(crate) async fn fetch(
    db: &PgPool,
    filter: &AuditFilter,
    limit: i64,
) -> CoreResult<Vec<SealedEvent>> {
    let rows: Vec<AuditRow> = filter.query(limit).build_query_as().fetch_all(db).await?;
    rows.into_iter()
        .map(|r| r.into_sealed().map_err(CoreError::internal))
        .collect()
}

/// Search the audit log (newest first). Requires `ViewAudit`; the search is recorded.
pub async fn search(
    db: &PgPool,
    ctx: &Ctx,
    filter: &AuditFilter,
    limit: i64,
) -> CoreResult<Vec<SealedEvent>> {
    ctx.require(Capability::ViewAudit)?;
    let entries = fetch(db, filter, limit.clamp(1, MAX_PAGE)).await?;
    pdms_audit::record(
        db,
        vec![
            AuditEvent::new(
                Category::Access,
                "audit.searched",
                ctx.actor.clone(),
                "audit_log",
            )
            .context("filter", filter.describe())
            .context("results", i64::try_from(entries.len()).unwrap_or(i64::MAX)),
        ],
    )
    .await?;
    Ok(entries)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

/// Export matching entries. Requires `ExportAudit`; the export is recorded.
pub async fn export(
    db: &PgPool,
    ctx: &Ctx,
    filter: &AuditFilter,
    format: ExportFormat,
) -> CoreResult<Vec<u8>> {
    ctx.require(Capability::ExportAudit)?;
    let entries = fetch(db, filter, MAX_EXPORT_ROWS).await?;
    let body = match format {
        ExportFormat::Csv => to_csv(&entries),
        ExportFormat::Json => to_json(&entries)?,
    };
    pdms_audit::record(
        db,
        vec![
            AuditEvent::new(
                Category::Access,
                "audit.exported",
                ctx.actor.clone(),
                "audit_log",
            )
            .context("filter", filter.describe())
            .context(
                "format",
                if format == ExportFormat::Csv {
                    "csv"
                } else {
                    "json"
                },
            )
            .context("rows", i64::try_from(entries.len()).unwrap_or(i64::MAX)),
        ],
    )
    .await?;
    Ok(body)
}

/// Verify the hash chain. Requires `VerifyAudit`; the result is recorded.
pub async fn verify(db: &PgPool, ctx: &Ctx) -> CoreResult<VerifyReport> {
    ctx.require(Capability::VerifyAudit)?;
    let report = pdms_audit::verify(db).await?;
    let mut e = AuditEvent::new(
        Category::System,
        "audit.verified",
        ctx.actor.clone(),
        "audit_log",
    )
    .context("intact", report.is_intact())
    .context(
        "entries_checked",
        i64::try_from(report.entries_checked).unwrap_or(i64::MAX),
    )
    .context(
        "breaks",
        i64::try_from(report.breaks.len()).unwrap_or(i64::MAX),
    )
    .context("last_seq", report.last.seq);
    if !report.is_intact() {
        e = e.outcome(Outcome::Failure).context(
            "first_breaks",
            report
                .breaks
                .iter()
                .take(10)
                .map(|b| b.to_string())
                .collect::<Vec<_>>(),
        );
    }
    pdms_audit::record(db, vec![e]).await?;
    Ok(report)
}

const CSV_HEADER: &[&str] = &[
    "seq",
    "occurred_at",
    "category",
    "action",
    "outcome",
    "actor_user_id",
    "actor",
    "auth_method",
    "ip",
    "user_agent",
    "object_type",
    "object_id",
    "object_label",
    "document_id",
    "department_id",
    "changes",
    "context",
    "entry_hash",
];

/// Quote every field; neutralize spreadsheet formula injection.
pub(crate) fn csv_field(out: &mut String, s: &str) {
    out.push('"');
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        out.push('\'');
    }
    out.push_str(&s.replace('"', "\"\""));
    out.push('"');
}

fn to_csv(entries: &[SealedEvent]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&CSV_HEADER.join(","));
    out.push_str("\r\n");
    for s in entries {
        let e = &s.event;
        let opt = |v: Option<String>| v.unwrap_or_default();
        let fields = [
            s.seq.to_string(),
            format_timestamp(e.occurred_at),
            e.category.as_str().to_owned(),
            e.action.clone(),
            e.outcome.as_str().to_owned(),
            opt(e.actor.user_id.map(|u| u.to_string())),
            opt(e.actor.display.clone()),
            e.actor.auth_method.as_str().to_owned(),
            opt(e.actor.ip.map(|i| i.to_string())),
            opt(e.actor.user_agent.clone()),
            e.object_type.clone(),
            opt(e.object_id.clone()),
            opt(e.object_label.clone()),
            opt(e.document_id.map(|u| u.to_string())),
            opt(e.department_id.map(|u| u.to_string())),
            e.changes_json().map(|v| v.to_string()).unwrap_or_default(),
            e.context_json().to_string(),
            hex::encode(s.entry_hash),
        ];
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            csv_field(&mut out, f);
        }
        out.push_str("\r\n");
    }
    out.into_bytes()
}

fn to_json(entries: &[SealedEvent]) -> CoreResult<Vec<u8>> {
    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|s| {
            let e = &s.event;
            serde_json::json!({
                "seq": s.seq,
                "id": e.id,
                "occurred_at": format_timestamp(e.occurred_at),
                "category": e.category,
                "action": e.action,
                "outcome": e.outcome,
                "actor": {
                    "user_id": e.actor.user_id,
                    "display": e.actor.display,
                    "auth_method": e.actor.auth_method,
                    "ip": e.actor.ip.map(|i| i.to_string()),
                    "user_agent": e.actor.user_agent,
                    "api_token_id": e.actor.api_token_id,
                    "request_id": e.actor.request_id,
                },
                "object": {"type": e.object_type, "id": e.object_id, "label": e.object_label},
                "document_id": e.document_id,
                "department_id": e.department_id,
                "changes": e.changes_json(),
                "context": e.context_json(),
                "hash_version": s.hash_version,
                "prev_hash": hex::encode(s.prev_hash),
                "entry_hash": hex::encode(s.entry_hash),
            })
        })
        .collect();
    serde_json::to_vec_pretty(&items).map_err(CoreError::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_neutralizes_formulas_and_quotes() {
        let mut s = String::new();
        csv_field(&mut s, "=HYPERLINK(\"x\")");
        assert_eq!(s, "\"'=HYPERLINK(\"\"x\"\")\"");
    }
}
