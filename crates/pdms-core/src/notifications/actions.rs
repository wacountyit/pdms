// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! "Action needed" notices: an approval is waiting, a submission was
//! approved or sent back, a signature was requested. Unlike reminders they
//! are not driven by dates or rules; the workflow sends them as things
//! happen, in the same transaction as the change that caused them, through
//! the same in-app and email delivery as reminders.

use std::collections::BTreeSet;

use pdms_audit::{AuditEvent, Category};
use sqlx::PgConnection;
use uuid::Uuid;

use super::dispatch::{self, Visibility};
use super::rules::Channel;
use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;

/// One notice, sent to several people.
#[derive(Clone, Debug)]
pub(crate) struct Notice {
    /// Short line, e.g. "Approval requested: POL-2026-000001 Leave policy".
    pub subject: String,
    /// One or two plain-text sentences.
    pub body: String,
    /// Path within the application, e.g. `/documents/{id}#workflow`.
    pub link: String,
    pub document_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    /// Sending the same key twice creates nothing the second time.
    pub dedupe_key: String,
}

/// Send `notice` to `users` (inactive users and people who cannot see the
/// subject are skipped). `about` is the audit entry's base (object filled in).
pub(crate) async fn send(
    conn: &mut PgConnection,
    core: &Core,
    ctx: &Ctx,
    notice: Notice,
    users: BTreeSet<Uuid>,
    visibility: &Visibility,
    about: AuditEvent,
) -> CoreResult<usize> {
    let users = dispatch::active_only(&mut *conn, users).await?;
    if users.is_empty() {
        return Ok(0);
    }
    let id = sqlx::query_scalar!(
        "INSERT INTO notifications (id, kind, document_id, appointment_id, subject, body, link, dedupe_key, scheduled_for)
         VALUES ($1, 'action', $2, $3, $4, $5, $6, $7, current_date)
         ON CONFLICT (dedupe_key) DO NOTHING RETURNING id",
        Uuid::now_v7(),
        notice.document_id,
        notice.appointment_id,
        notice.subject,
        notice.body,
        notice.link,
        notice.dedupe_key,
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(id) = id else { return Ok(0) };
    let fan = dispatch::deliver(
        &mut *conn,
        core,
        id,
        &users,
        &[Channel::InApp, Channel::Email],
        visibility,
    )
    .await?;
    let mut e = about;
    e.category = Category::Notification;
    e.action = "notification.created".to_owned();
    e.actor = ctx.actor.clone();
    pdms_audit::append(
        &mut *conn,
        vec![
            e.context("notification_id", id)
                .context("kind", "action")
                .context("subject", notice.subject)
                .context("recipients", i64::try_from(users.len()).unwrap_or(i64::MAX))
                .context("emails_queued", fan.queued)
                .context("skipped", fan.skipped),
        ],
    )
    .await?;
    Ok(fan.created)
}
