// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Integrations: outbound events (webhooks and integration providers) and
//! the audit forwarder.
//!
//! The audit log is the event stream. Every change is already an audit entry,
//! written in the same transaction as the change, with a sequence number that
//! is committed in order. A dispatcher reads new entries after a stored cursor
//! and turns the published ones into deliveries, one per target, each sent by
//! a retried job with backoff and a delivery log. Nothing in the services
//! knows about webhooks, so a new integration never needs changes there.
//!
//! Targets see only what they are allowed to: a webhook acts as a service
//! user, and an event about a document (or position) is delivered only if
//! that user can view it, checked by the central authorization engine.
//!
//! Future integrations (for example a records system or a records-request
//! portal) implement [`IntegrationProvider`] and are enabled by key in
//! `[integrations] providers`; they receive the same events with the same
//! retries and logs. Nothing depends on any particular product.

pub mod forward;
pub mod webhooks;

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use pdms_authz::{Permission, Principal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Core;
use crate::context::{ClientInfo, Ctx};
use crate::error::CoreResult;
use crate::jobs::{self, NewJob};

/// Job kind that delivers one event to one target.
pub const DELIVER_JOB: &str = "integrations.deliver";
/// Periodic job that turns new audit entries into deliveries.
pub const DISPATCH_JOB: &str = "integrations.dispatch";

/// Provider keys that can be enabled in configuration.
pub const KNOWN_PROVIDERS: &[&str] = &["log"];

/// Audit actions published as events (by prefix), and reads that are not.
const PUBLISHED: &[&str] = &[
    "document.",
    "workflow.",
    "signature.",
    "signatory.",
    "review_task.",
    "appointment.",
    "position.",
    "comment.",
    "date_suggestion.",
];
const NOT_PUBLISHED: &[&str] = &[
    "document.viewed",
    "document.downloaded",
    "document.upload_rejected",
    "document.list_exported",
    "workflow.decision_denied",
    "workflow.comment_denied",
    "workflow.cancel_denied",
    "comment.post_denied",
    "comment.edit_denied",
    "comment.delete_denied",
    "comment.history_denied",
];

/// Whether an audit action is published to integrations.
pub fn is_published(action: &str) -> bool {
    PUBLISHED.iter().any(|p| action.starts_with(p)) && !NOT_PUBLISHED.contains(&action)
}

/// Whether an event matches a subscription pattern: `*`, `document.*`, or an
/// exact action.
pub fn matches(pattern: &str, event: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some("") => true,
        Some(prefix) => event.starts_with(prefix),
        None => pattern == event,
    }
}

/// The document an event is about, as the target may see it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventDocument {
    pub id: Uuid,
    pub external_id: String,
    pub title: String,
    pub document_type: String,
    pub department: String,
    pub status: String,
    pub url: String,
}

/// The JSON body every target receives.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboundEvent {
    /// The delivery's ID (stable across retries; use it to deduplicate).
    pub id: Uuid,
    pub event: String,
    pub occurred_at: DateTime<Utc>,
    pub audit_seq: i64,
    pub organization: String,
    pub actor: Value,
    pub object: Value,
    pub document: Option<EventDocument>,
    pub changes: Option<Value>,
    pub context: Value,
}

/// A provider for an outside system. Implementations must be idempotent per
/// event `id`: a delivery may be retried.
pub trait IntegrationProvider: Send + Sync + fmt::Debug {
    /// The configuration key, e.g. `log`.
    fn key(&self) -> &'static str;
    /// Whether the provider wants this event.
    fn wants(&self, event: &str) -> bool;
    /// Deliver one event. `Err` means "retry later".
    fn deliver<'a>(&'a self, event: &'a OutboundEvent) -> BoxFuture<'a, Result<(), String>>;
}

/// Writes every event to the application log. A reference implementation,
/// and useful to see what a provider would receive.
#[derive(Debug, Default)]
pub struct LogProvider;

impl IntegrationProvider for LogProvider {
    fn key(&self) -> &'static str {
        "log"
    }

    fn wants(&self, _event: &str) -> bool {
        true
    }

    fn deliver<'a>(&'a self, event: &'a OutboundEvent) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let body = serde_json::to_string(event).map_err(|e| e.to_string())?;
            tracing::info!(target: "pdms::integrations", event = %event.event, %body, "integration event");
            Ok(())
        })
    }
}

/// The providers enabled in configuration.
pub fn providers(core: &Core) -> Vec<Arc<dyn IntegrationProvider>> {
    core.config
        .integrations
        .providers
        .iter()
        .filter_map(|k| match k.as_str() {
            "log" => Some(Arc::new(LogProvider) as Arc<dyn IntegrationProvider>),
            _ => None,
        })
        .collect()
}

/// Read a consumer's cursor, locking it. A new consumer starts at the
/// current end of the log (history is not replayed).
pub(crate) async fn cursor(conn: &mut PgConnection, name: &str) -> CoreResult<i64> {
    sqlx::query!(
        "INSERT INTO outbound_cursors (name, last_seq)
         VALUES ($1, (SELECT coalesce(max(seq), 0) FROM audit_log))
         ON CONFLICT (name) DO NOTHING",
        name
    )
    .execute(&mut *conn)
    .await?;
    Ok(sqlx::query_scalar!(
        "SELECT last_seq FROM outbound_cursors WHERE name = $1 FOR UPDATE",
        name
    )
    .fetch_one(&mut *conn)
    .await?)
}

pub(crate) async fn advance(conn: &mut PgConnection, name: &str, seq: i64) -> CoreResult<()> {
    sqlx::query!(
        "UPDATE outbound_cursors SET last_seq = $2 WHERE name = $1 AND last_seq < $2",
        name,
        seq
    )
    .execute(conn)
    .await?;
    Ok(())
}

struct Target {
    webhook_id: Uuid,
    events: Vec<String>,
    principal: Principal,
    name: String,
}

/// May the target's user see what this audit entry is about?
async fn visible(
    core: &Core,
    ctx: &Ctx,
    object_type: &str,
    document_id: Option<Uuid>,
    object_id: Option<&str>,
) -> CoreResult<bool> {
    if let Some(doc) = document_id {
        return crate::documents::access::can(core, ctx, doc, Permission::View).await;
    }
    match object_type {
        "appointment" => match object_id.and_then(|s| s.parse::<Uuid>().ok()) {
            Some(a) => crate::appointments::can_view_appointment(core, ctx, a).await,
            None => Ok(false),
        },
        "position" => match object_id.and_then(|s| s.parse::<Uuid>().ok()) {
            Some(p) => crate::appointments::can_view_position_id(core, ctx, p).await,
            None => Ok(false),
        },
        _ => Ok(false),
    }
}

async fn document_summary(core: &Core, id: Uuid) -> CoreResult<Option<EventDocument>> {
    let r = sqlx::query!(
        r#"SELECT d.id, d.external_id, d.title, t.name AS type_name, dep.name AS department, d.status::text AS "status!"
             FROM documents d JOIN document_types t ON t.id = d.document_type_id
             JOIN departments dep ON dep.id = d.department_id
            WHERE d.id = $1"#,
        id
    )
    .fetch_optional(&core.db)
    .await?;
    Ok(r.map(|r| EventDocument {
        url: core
            .config
            .server
            .public_url
            .join(&format!("documents/{}", r.id))
            .map(|u| u.to_string())
            .unwrap_or_default(),
        id: r.id,
        external_id: r.external_id,
        title: r.title,
        document_type: r.type_name,
        department: r.department,
        status: r.status,
    }))
}

/// What one dispatch run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchReport {
    pub entries_read: usize,
    pub deliveries: usize,
}

const BATCH: i64 = 500;

/// Turn new audit entries into deliveries (one batch). Safe to run
/// concurrently: the cursor row is locked for the duration.
pub async fn dispatch(core: &Core) -> CoreResult<DispatchReport> {
    let mut report = DispatchReport::default();
    let mut tx = core.db.begin().await?;
    let last = cursor(&mut tx, "outbound").await?;
    let rows = sqlx::query!(
        r#"SELECT seq, occurred_at, action, outcome::text AS "outcome!", actor_user_id, actor_display,
                  object_type, object_id, object_label, document_id, changes, context
             FROM audit_log WHERE seq > $1 ORDER BY seq LIMIT $2"#,
        last,
        BATCH
    )
    .fetch_all(&mut *tx)
    .await?;
    let Some(max_seq) = rows.last().map(|r| r.seq) else {
        tx.commit().await?; // keeps a newly created cursor
        return Ok(report);
    };
    report.entries_read = rows.len();

    let mut targets = Vec::new();
    if core.config.integrations.webhooks_enabled {
        let hooks = sqlx::query!(
            "SELECT w.id, w.name, w.events, w.acts_as_user_id FROM webhooks w
               JOIN users u ON u.id = w.acts_as_user_id
              WHERE w.is_active AND u.status = 'active'"
        )
        .fetch_all(&mut *tx)
        .await?;
        for h in hooks {
            targets.push(Target {
                webhook_id: h.id,
                events: h.events,
                principal: crate::auth::principal::load(&core.db, h.acts_as_user_id).await?,
                name: h.name,
            });
        }
    }
    let providers = providers(core);
    let attempts = core.config.integrations.webhook_max_attempts.max(1);
    let org = crate::branding::org_name(core).await;

    for r in rows
        .iter()
        .filter(|r| r.outcome == "success" && is_published(&r.action))
    {
        let interested: Vec<&Target> = targets
            .iter()
            .filter(|t| t.events.iter().any(|p| matches(p, &r.action)))
            .collect();
        let wanting: Vec<&Arc<dyn IntegrationProvider>> =
            providers.iter().filter(|p| p.wants(&r.action)).collect();
        if interested.is_empty() && wanting.is_empty() {
            continue;
        }
        let document = match r.document_id {
            Some(d) => document_summary(core, d).await?,
            None => None,
        };
        let base = OutboundEvent {
            id: Uuid::nil(),
            event: r.action.clone(),
            occurred_at: r.occurred_at,
            audit_seq: r.seq,
            organization: org.clone(),
            actor: json!({ "user_id": r.actor_user_id, "display": r.actor_display }),
            object: json!({ "type": r.object_type, "id": r.object_id, "label": r.object_label }),
            document,
            changes: r.changes.clone(),
            context: r.context.clone(),
        };
        for t in interested {
            let ctx = Ctx::user(
                t.principal.clone(),
                &t.name,
                pdms_audit::AuthMethod::System,
                &ClientInfo::default(),
            );
            if !visible(
                core,
                &ctx,
                &r.object_type,
                r.document_id,
                r.object_id.as_deref(),
            )
            .await?
            {
                continue;
            }
            if insert_delivery(&mut tx, Some(t.webhook_id), None, &base, attempts).await? {
                report.deliveries += 1;
            }
        }
        // Providers run inside the organization (like the audit forwarder)
        // and receive every published event.
        for p in wanting {
            if insert_delivery(&mut tx, None, Some(p.key()), &base, attempts).await? {
                report.deliveries += 1;
            }
        }
    }
    advance(&mut tx, "outbound", max_seq).await?;
    tx.commit().await?;
    Ok(report)
}

/// Dispatch until the backlog is empty.
pub async fn dispatch_all(core: &Core) -> CoreResult<DispatchReport> {
    let mut total = DispatchReport::default();
    loop {
        let r = dispatch(core).await?;
        total.entries_read += r.entries_read;
        total.deliveries += r.deliveries;
        if r.entries_read < usize::try_from(BATCH).unwrap_or(usize::MAX) {
            return Ok(total);
        }
    }
}

async fn insert_delivery(
    conn: &mut PgConnection,
    webhook: Option<Uuid>,
    provider: Option<&str>,
    base: &OutboundEvent,
    max_attempts: i32,
) -> CoreResult<bool> {
    let id = Uuid::now_v7();
    let event = OutboundEvent { id, ..base.clone() };
    let payload = serde_json::to_value(&event).map_err(crate::CoreError::internal)?;
    let inserted = sqlx::query_scalar!(
        "INSERT INTO webhook_deliveries (id, webhook_id, provider, audit_seq, event, payload)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING RETURNING id",
        id,
        webhook,
        provider,
        base.audit_seq,
        base.event,
        payload
    )
    .fetch_optional(&mut *conn)
    .await?;
    if inserted.is_none() {
        return Ok(false);
    }
    jobs::enqueue(
        &mut *conn,
        NewJob::new(DELIVER_JOB, json!({ "delivery_id": id }))
            .idempotency_key(format!("outbound:{id}"))
            .max_attempts(max_attempts),
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(matches("*", "document.created"));
        assert!(matches("document.*", "document.status_changed"));
        assert!(!matches("document.*", "workflow.approved"));
        assert!(matches("workflow.approved", "workflow.approved"));
        assert!(!matches("workflow.approved", "workflow.approved_x"));
    }

    #[test]
    fn reads_and_denials_are_not_published() {
        assert!(is_published("document.created"));
        assert!(is_published("document.exported"));
        assert!(is_published("appointment.reappointed"));
        assert!(!is_published("document.viewed"));
        assert!(!is_published("document.downloaded"));
        assert!(!is_published("workflow.decision_denied"));
        assert!(!is_published("auth.login_failed"));
        assert!(!is_published("access.denied"));
    }
}
