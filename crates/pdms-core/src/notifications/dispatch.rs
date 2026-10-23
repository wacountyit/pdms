// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Shared fan-out for every kind of notification: who receives it, whether
//! they may see what it is about, and one delivery per channel (email
//! deliveries are queued as jobs). Callers supply the transaction, so a
//! notification and its deliveries are written together with whatever caused
//! them.

use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use pdms_authz::{
    DocumentAttrs, Grant, Permission, PositionAttrs, Principal, can_view_position, decide,
};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::planner::{DeliveryStatus, EMAIL_JOB};
use super::rules::{Channel, Recipient};
use crate::error::CoreResult;
use crate::jobs::{self, NewJob};
use crate::{Core, acl};

/// What a notification is about, for deciding who may receive it.
#[derive(Clone, Debug)]
pub(crate) enum Visibility {
    Document {
        attrs: DocumentAttrs,
        grants: Vec<Grant>,
    },
    Position(PositionAttrs),
    /// Not about a document or a position: the caller chose the recipients,
    /// and the notice names no document (for example SharePoint status for
    /// administrators).
    Anyone,
}

impl Visibility {
    pub(crate) async fn document(core: &Core, attrs: DocumentAttrs) -> CoreResult<Visibility> {
        let grants = acl::relevant_grants(&core.db, attrs.department_id, Some(attrs.id)).await?;
        Ok(Visibility::Document { attrs, grants })
    }

    pub(crate) fn allows(&self, core: &Core, principal: &Principal) -> bool {
        match self {
            Visibility::Document { attrs, grants } => decide(
                &core.policy,
                principal,
                Permission::View,
                attrs,
                grants,
                Utc::now(),
            )
            .is_allowed(),
            Visibility::Position(p) => can_view_position(principal, p),
            Visibility::Anyone => true,
        }
    }

    pub(crate) fn denied_reason(&self) -> &'static str {
        match self {
            Visibility::Document { .. } => "recipient cannot view the document",
            Visibility::Position(_) => "recipient cannot view the position",
            Visibility::Anyone => "recipient is not active",
        }
    }
}

/// Context for resolving rule recipients: the document's owner and
/// department, or the position's manager and department.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Audience {
    pub owner: Option<Uuid>,
    pub department_id: Option<Uuid>,
}

/// Holders of a role org-wide, or for `department_id` when given.
pub(crate) async fn role_holders(
    conn: &mut PgConnection,
    role_id: Uuid,
    department_id: Option<Uuid>,
) -> CoreResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        "SELECT user_id FROM role_assignments
          WHERE role_id = $1 AND (department_id IS NULL OR department_id = $2)",
        role_id,
        department_id
    )
    .fetch_all(conn)
    .await?)
}

/// Users the recipients resolve to (active users only).
pub(crate) async fn resolve(
    conn: &mut PgConnection,
    audience: Audience,
    recipients: &[Recipient],
) -> CoreResult<BTreeSet<Uuid>> {
    let mut out = BTreeSet::new();
    for r in recipients {
        let ids: Vec<Uuid> = match *r {
            Recipient::Owner => audience.owner.into_iter().collect(),
            Recipient::User(id) => vec![id],
            Recipient::DocumentDepartment => match audience.department_id {
                Some(d) => {
                    sqlx::query_scalar!(
                        "SELECT user_id FROM department_members WHERE department_id = $1",
                        d
                    )
                    .fetch_all(&mut *conn)
                    .await?
                }
                None => Vec::new(),
            },
            Recipient::Department(id) => {
                sqlx::query_scalar!(
                    "SELECT user_id FROM department_members WHERE department_id = $1",
                    id
                )
                .fetch_all(&mut *conn)
                .await?
            }
            Recipient::Group(id) => {
                sqlx::query_scalar!("SELECT user_id FROM group_members WHERE group_id = $1", id)
                    .fetch_all(&mut *conn)
                    .await?
            }
            Recipient::Role(id) => role_holders(&mut *conn, id, audience.department_id).await?,
        };
        out.extend(ids);
    }
    active_only(conn, out).await
}

pub(crate) async fn active_only(
    conn: &mut PgConnection,
    users: BTreeSet<Uuid>,
) -> CoreResult<BTreeSet<Uuid>> {
    let active: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM users WHERE id = ANY($1) AND status = 'active'",
        &users.into_iter().collect::<Vec<_>>()
    )
    .fetch_all(conn)
    .await?;
    Ok(active.into_iter().collect())
}

/// What [`deliver`] did.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Fanout {
    pub created: usize,
    pub queued: i64,
    pub skipped: i64,
}

/// Create one delivery per user and channel for a notification. People who
/// cannot see what the notification is about get a `skipped` delivery (kept
/// for the delivery log) and are never told anything.
pub(crate) async fn deliver(
    conn: &mut PgConnection,
    core: &Core,
    notification_id: Uuid,
    users: &BTreeSet<Uuid>,
    channels: &[Channel],
    visibility: &Visibility,
) -> CoreResult<Fanout> {
    let mut principals = HashMap::new();
    for u in users {
        principals.insert(*u, crate::auth::principal::load(&core.db, *u).await?);
    }
    let mut out = Fanout::default();
    for u in users {
        let can_view = principals
            .get(u)
            .is_some_and(|p| visibility.allows(core, p));
        for ch in channels {
            let (status, reason) = match (can_view, ch) {
                (false, _) => (DeliveryStatus::Skipped, Some(visibility.denied_reason())),
                (true, Channel::InApp) => (DeliveryStatus::Sent, None),
                (true, Channel::Email) if core.mailer().enabled() => {
                    (DeliveryStatus::Pending, None)
                }
                (true, Channel::Email) => {
                    (DeliveryStatus::Skipped, Some("email is not configured"))
                }
            };
            let delivery = sqlx::query_scalar!(
                "INSERT INTO notification_deliveries (id, notification_id, user_id, channel, status, skip_reason, sent_at)
                 VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $5 = 'sent'::delivery_status THEN now() END)
                 ON CONFLICT ON CONSTRAINT notification_deliveries_key DO NOTHING RETURNING id",
                Uuid::now_v7(),
                notification_id,
                u,
                *ch as Channel,
                status as DeliveryStatus,
                reason,
            )
            .fetch_optional(&mut *conn)
            .await?;
            let Some(delivery) = delivery else { continue };
            out.created += 1;
            match status {
                DeliveryStatus::Skipped => out.skipped += 1,
                DeliveryStatus::Pending => {
                    out.queued += 1;
                    jobs::enqueue(
                        &mut *conn,
                        NewJob::new(EMAIL_JOB, json!({ "delivery_id": delivery }))
                            .idempotency_key(format!("email:{delivery}"))
                            .max_attempts(6),
                    )
                    .await?;
                }
                _ => {}
            }
        }
    }
    Ok(out)
}
