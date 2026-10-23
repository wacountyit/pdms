// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The in-app notification center.

use chrono::{DateTime, NaiveDate, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Permission;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::NotificationKind;
use super::rules::DueEvent;
use crate::Core;
use crate::context::Ctx;
use crate::documents::access;
use crate::error::{CoreError, CoreResult};

#[derive(Clone, Debug, Serialize)]
pub struct InboxItem {
    pub delivery_id: Uuid,
    pub kind: NotificationKind,
    pub document_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    /// A document's external ID or a board's name.
    pub code: String,
    pub title: String,
    /// Where the notice leads.
    pub href: String,
    pub event: Option<DueEvent>,
    pub due_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// As of today, e.g. "Review due in 12 days" or "Approval requested".
    pub headline: String,
}

pub async fn unread_count(db: &PgPool, user_id: Uuid) -> CoreResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM notification_deliveries
            WHERE user_id = $1 AND channel = 'in_app' AND status = 'sent' AND acknowledged_at IS NULL"#,
        user_id
    )
    .fetch_one(db)
    .await?)
}

/// The caller's in-app notifications, newest first. Items about documents or
/// positions they can no longer view are hidden.
pub async fn list(
    core: &Core,
    ctx: &Ctx,
    include_acknowledged: bool,
) -> CoreResult<Vec<InboxItem>> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let rows = sqlx::query!(
        r#"SELECT dl.id, n.kind AS "kind: NotificationKind", n.document_id, n.appointment_id,
                  d.external_id AS "external_id?", d.title AS "doc_title?",
                  a.person_name AS "person_name?", p.id AS "position_id?", p.name AS "position_name?",
                  p.body_name AS "body_name?",
                  n.event AS "event: DueEvent", n.due_date, n.subject, n.link, dl.created_at, dl.acknowledged_at
             FROM notification_deliveries dl
             JOIN notifications n ON n.id = dl.notification_id
             LEFT JOIN documents d ON d.id = n.document_id
             LEFT JOIN appointments a ON a.id = n.appointment_id
             LEFT JOIN positions p ON p.id = a.position_id
            WHERE dl.user_id = $1 AND dl.channel = 'in_app' AND dl.status = 'sent'
              AND (d.id IS NULL OR d.deleted_at IS NULL)
              AND ($2 OR dl.acknowledged_at IS NULL)
            ORDER BY dl.created_at DESC LIMIT 500"#,
        user,
        include_acknowledged
    )
    .fetch_all(&core.db)
    .await?;
    let today = crate::schedule::local_today(Utc::now(), core.config().timezone());
    let mut out = Vec::new();
    for r in rows {
        if let Some(doc) = r.document_id
            && !access::can(core, ctx, doc, Permission::View).await?
        {
            continue;
        }
        if let Some(a) = r.appointment_id
            && !crate::appointments::can_view_appointment(core, ctx, a).await?
        {
            continue;
        }
        let (code, title) = match (&r.external_id, &r.person_name) {
            (Some(ext), _) => (ext.clone(), r.doc_title.clone().unwrap_or_default()),
            (None, Some(person)) => (
                r.body_name.clone().unwrap_or_default(),
                format!("{person} — {}", r.position_name.clone().unwrap_or_default()),
            ),
            (None, None) => (String::new(), String::new()),
        };
        let href = match (r.kind, r.document_id, r.position_id) {
            (NotificationKind::Action, _, _) => r.link.clone().unwrap_or_else(|| "/".into()),
            (_, Some(doc), _) => format!("/documents/{doc}"),
            (_, None, Some(p)) => format!("/positions/{p}"),
            _ => "/".into(),
        };
        let headline = match (r.kind, r.event, r.due_date) {
            (NotificationKind::Reminder, Some(event), Some(due)) => {
                super::delivery::describe(event, due, today).0
            }
            _ => r.subject.clone().unwrap_or_default(),
        };
        out.push(InboxItem {
            delivery_id: r.id,
            kind: r.kind,
            document_id: r.document_id,
            appointment_id: r.appointment_id,
            code,
            // Action notices carry their whole message in the headline.
            title: if r.kind == NotificationKind::Action {
                String::new()
            } else {
                title
            },
            href,
            event: r.event,
            due_date: r.due_date,
            created_at: r.created_at,
            acknowledged_at: r.acknowledged_at,
            headline,
        });
    }
    Ok(out)
}

/// Acknowledge one of the caller's notifications (audited).
pub async fn acknowledge(core: &Core, ctx: &Ctx, delivery_id: Uuid) -> CoreResult<()> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let mut tx = core.db.begin().await?;
    let r = sqlx::query!(
        r#"UPDATE notification_deliveries dl SET acknowledged_at = now(), read_at = coalesce(read_at, now()), updated_at = now()
             FROM notifications n
            WHERE dl.id = $1 AND dl.user_id = $2 AND dl.channel = 'in_app' AND dl.acknowledged_at IS NULL
              AND n.id = dl.notification_id
          RETURNING n.kind AS "kind: NotificationKind", n.document_id, n.appointment_id, n.event AS "event: DueEvent",
                    n.due_date, n.subject"#,
        delivery_id,
        user
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    let doc = match r.document_id {
        Some(id) => sqlx::query!(
            "SELECT external_id, title, department_id FROM documents WHERE id = $1",
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(|d| {
            (
                id,
                format!("{} {}", d.external_id, d.title),
                d.department_id,
            )
        }),
        None => None,
    };
    let mut e = match (&doc, r.appointment_id) {
        (Some((id, label, dept)), _) => AuditEvent::new(
            Category::Notification,
            "notification.acknowledged",
            ctx.actor.clone(),
            "document",
        )
        .object(*id, Some(label))
        .document(*id)
        .department(Some(*dept)),
        (None, Some(a)) => AuditEvent::new(
            Category::Notification,
            "notification.acknowledged",
            ctx.actor.clone(),
            "appointment",
        )
        .object(a, None),
        (None, None) => AuditEvent::new(
            Category::Notification,
            "notification.acknowledged",
            ctx.actor.clone(),
            "notification",
        )
        .object(delivery_id, None),
    };
    e = e
        .context("delivery_id", delivery_id)
        .context("kind", r.kind.as_str());
    if let Some(ev) = r.event {
        e = e.context("event", ev.as_str());
    }
    if let Some(due) = r.due_date {
        e = e.context("due_date", due);
    }
    if let Some(subject) = r.subject {
        e = e.context("subject", subject);
    }
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}
