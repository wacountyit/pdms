// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Notification rules: who is told, how far ahead, how often when overdue, and how.
//!
//! For a document and event the most specific rules win: the document's own
//! rules if it has any for that event, else its type's, else the
//! organization defaults. Appointments work the same way with the position's
//! rules, then the organization defaults.

use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::{Capability, Permission};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::Core;
use crate::context::Ctx;
use crate::documents::access;
use crate::error::{CoreError, CoreResult};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, sqlx::Type)]
#[sqlx(type_name = "due_event", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DueEvent {
    Review,
    Renewal,
    Expiration,
    /// An appointment's term ends.
    TermEnd,
    /// A reappointment or replacement decision is due.
    Reappointment,
}

impl DueEvent {
    /// Document events.
    pub const ALL: [DueEvent; 3] = [DueEvent::Review, DueEvent::Renewal, DueEvent::Expiration];
    /// Appointment events.
    pub const APPOINTMENT: [DueEvent; 2] = [DueEvent::TermEnd, DueEvent::Reappointment];

    pub fn as_str(self) -> &'static str {
        match self {
            DueEvent::Review => "review",
            DueEvent::Renewal => "renewal",
            DueEvent::Expiration => "expiration",
            DueEvent::TermEnd => "term_end",
            DueEvent::Reappointment => "reappointment",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DueEvent::Review => "Review",
            DueEvent::Renewal => "Renewal",
            DueEvent::Expiration => "Expiration",
            DueEvent::TermEnd => "Term end",
            DueEvent::Reappointment => "Reappointment",
        }
    }

    pub fn is_appointment(self) -> bool {
        matches!(self, DueEvent::TermEnd | DueEvent::Reappointment)
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain(Self::APPOINTMENT)
            .find(|e| e.as_str() == s)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, sqlx::Type)]
#[sqlx(type_name = "delivery_channel", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Email,
    InApp,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Email => "email",
            Channel::InApp => "in_app",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Email => "Email",
            Channel::InApp => "In-app",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "recipient_kind", rename_all = "snake_case")]
pub enum RecipientKind {
    Owner,
    DocumentDepartment,
    User,
    Group,
    Department,
    Role,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Recipient {
    /// The document's primary record owner.
    Owner,
    /// Every member of the document's department.
    DocumentDepartment,
    User(Uuid),
    Group(Uuid),
    Department(Uuid),
    /// Holders of a role org-wide, or for the document's department.
    Role(Uuid),
}

impl Recipient {
    /// `owner`, `document_department`, or `kind:uuid`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner" => return Some(Recipient::Owner),
            "document_department" => return Some(Recipient::DocumentDepartment),
            _ => {}
        }
        let (k, id) = s.split_once(':')?;
        let id: Uuid = id.parse().ok()?;
        Some(match k {
            "user" => Recipient::User(id),
            "group" => Recipient::Group(id),
            "department" => Recipient::Department(id),
            "role" => Recipient::Role(id),
            _ => return None,
        })
    }

    pub fn key(&self) -> String {
        match self {
            Recipient::Owner => "owner".into(),
            Recipient::DocumentDepartment => "document_department".into(),
            Recipient::User(id) => format!("user:{id}"),
            Recipient::Group(id) => format!("group:{id}"),
            Recipient::Department(id) => format!("department:{id}"),
            Recipient::Role(id) => format!("role:{id}"),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
pub enum RuleScope {
    Organization,
    DocumentType(Uuid),
    Document(Uuid),
    /// Appointments to one position.
    Position(Uuid),
}

#[derive(Clone, Debug, Serialize)]
pub struct Rule {
    pub id: Uuid,
    pub scope: RuleScope,
    pub event: DueEvent,
    pub offsets_days: Vec<i32>,
    pub overdue_repeat_days: Option<i32>,
    pub channels: Vec<Channel>,
    pub is_active: bool,
    /// (recipient, display label)
    pub recipients: Vec<(Recipient, String)>,
}

impl Rule {
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = self
            .offsets_days
            .iter()
            .map(|d| {
                if *d == 0 {
                    "on the due date".to_owned()
                } else {
                    format!("{d} days before")
                }
            })
            .collect();
        if let Some(r) = self.overdue_repeat_days {
            parts.push(format!("then every {r} days while overdue"));
        }
        parts.join(", ")
    }

    pub fn recipient_labels(&self) -> String {
        self.recipients
            .iter()
            .map(|r| r.1.clone())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn channel_labels(&self) -> String {
        self.channels
            .iter()
            .map(|c| c.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Where a document's effective rules for an event come from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
pub enum RuleSource {
    Document,
    DocumentType,
    Position,
    Organization,
    None,
}

impl RuleSource {
    pub fn label(self) -> &'static str {
        match self {
            RuleSource::Document => "set on this document",
            RuleSource::DocumentType => "document type default",
            RuleSource::Position => "set on this position",
            RuleSource::Organization => "organization default",
            RuleSource::None => "no reminders",
        }
    }
}

/// Every rule with its recipients (labels resolved).
pub async fn load_all(db: &PgPool) -> CoreResult<Vec<Rule>> {
    let rules = sqlx::query!(
        r#"SELECT id, document_type_id, document_id, position_id, event AS "event: DueEvent", offsets_days,
                  overdue_repeat_days, channels AS "channels: Vec<Channel>", is_active
             FROM notification_rules
            ORDER BY document_id NULLS FIRST, document_type_id NULLS FIRST, position_id NULLS FIRST, event, created_at"#
    )
    .fetch_all(db)
    .await?;
    let recipients = sqlx::query!(
        r#"SELECT r.rule_id, r.kind AS "kind: RecipientKind", r.user_id, r.group_id, r.department_id, r.role_id,
                  coalesce(u.display_name, g.name, d.name, ro.name) AS label
             FROM notification_rule_recipients r
             LEFT JOIN users u ON u.id = r.user_id
             LEFT JOIN groups g ON g.id = r.group_id
             LEFT JOIN departments d ON d.id = r.department_id
             LEFT JOIN roles ro ON ro.id = r.role_id
            ORDER BY r.kind, label"#
    )
    .fetch_all(db)
    .await?;
    Ok(rules
        .into_iter()
        .map(|r| Rule {
            id: r.id,
            scope: match (r.document_id, r.document_type_id, r.position_id) {
                (Some(d), _, _) => RuleScope::Document(d),
                (None, Some(t), _) => RuleScope::DocumentType(t),
                (None, None, Some(p)) => RuleScope::Position(p),
                (None, None, None) => RuleScope::Organization,
            },
            event: r.event,
            offsets_days: r.offsets_days,
            overdue_repeat_days: r.overdue_repeat_days,
            channels: r.channels,
            is_active: r.is_active,
            recipients: recipients
                .iter()
                .filter(|x| x.rule_id == r.id)
                .filter_map(|x| {
                    let label = x.label.clone().unwrap_or_default();
                    Some(match x.kind {
                        RecipientKind::Owner if r.event.is_appointment() => {
                            (Recipient::Owner, "Position manager".to_owned())
                        }
                        RecipientKind::Owner => (Recipient::Owner, "Primary owner".to_owned()),
                        RecipientKind::DocumentDepartment if r.event.is_appointment() => (
                            Recipient::DocumentDepartment,
                            "Everyone in the position's department".to_owned(),
                        ),
                        RecipientKind::DocumentDepartment => (
                            Recipient::DocumentDepartment,
                            "Everyone in the document's department".to_owned(),
                        ),
                        RecipientKind::User => (Recipient::User(x.user_id?), label),
                        RecipientKind::Group => {
                            (Recipient::Group(x.group_id?), format!("Group: {label}"))
                        }
                        RecipientKind::Department => (
                            Recipient::Department(x.department_id?),
                            format!("Department: {label}"),
                        ),
                        RecipientKind::Role => (
                            Recipient::Role(x.role_id?),
                            if r.event.is_appointment() {
                                format!("{label} (position's department)")
                            } else {
                                format!("{label} (document's department)")
                            },
                        ),
                    })
                })
                .collect(),
        })
        .collect())
}

/// The rules that apply to a document for one event, and where they come
/// from. Only active rules count, so a deactivated override falls back to the
/// type or organization default.
pub fn effective(
    rules: &[Rule],
    document_id: Uuid,
    type_id: Uuid,
    event: DueEvent,
) -> (Vec<&Rule>, RuleSource) {
    let pick = |scope: RuleScope| -> Vec<&Rule> {
        rules
            .iter()
            .filter(|r| r.scope == scope && r.event == event && r.is_active)
            .collect()
    };
    for (scope, source) in [
        (RuleScope::Document(document_id), RuleSource::Document),
        (RuleScope::DocumentType(type_id), RuleSource::DocumentType),
        (RuleScope::Organization, RuleSource::Organization),
    ] {
        let found = pick(scope);
        if !found.is_empty() {
            return (found, source);
        }
    }
    (Vec::new(), RuleSource::None)
}

/// The rules that apply to appointments to a position for one event: the
/// position's own active rules, else the organization defaults.
pub fn effective_for_position(
    rules: &[Rule],
    position_id: Uuid,
    event: DueEvent,
) -> (Vec<&Rule>, RuleSource) {
    for (scope, source) in [
        (RuleScope::Position(position_id), RuleSource::Position),
        (RuleScope::Organization, RuleSource::Organization),
    ] {
        let found: Vec<&Rule> = rules
            .iter()
            .filter(|r| r.scope == scope && r.event == event && r.is_active)
            .collect();
        if !found.is_empty() {
            return (found, source);
        }
    }
    (Vec::new(), RuleSource::None)
}

#[derive(Clone, Debug)]
pub struct RuleSpec {
    pub event: DueEvent,
    pub offsets_days: Vec<i32>,
    pub overdue_repeat_days: Option<i32>,
    pub channels: Vec<Channel>,
    pub recipients: Vec<Recipient>,
    pub is_active: bool,
}

fn check(scope: RuleScope, spec: &mut RuleSpec) -> CoreResult<()> {
    let fits = match scope {
        RuleScope::Organization => true,
        RuleScope::Position(_) => spec.event.is_appointment(),
        RuleScope::Document(_) | RuleScope::DocumentType(_) => !spec.event.is_appointment(),
    };
    if !fits {
        return Err(CoreError::validation(
            "that reminder event does not apply here",
        ));
    }
    spec.offsets_days.sort_unstable_by(|a, b| b.cmp(a));
    spec.offsets_days.dedup();
    if spec.offsets_days.is_empty() || spec.offsets_days.len() > 20 {
        return Err(CoreError::validation(
            "enter 1–20 reminder offsets (days before the due date; 0 = on the date)",
        ));
    }
    if spec.offsets_days.iter().any(|d| !(0..=3650).contains(d)) {
        return Err(CoreError::validation(
            "reminder offsets must be 0–3650 days",
        ));
    }
    if spec
        .overdue_repeat_days
        .is_some_and(|r| !(1..=365).contains(&r))
    {
        return Err(CoreError::validation("overdue repeat must be 1–365 days"));
    }
    spec.channels.dedup();
    if spec.channels.is_empty() {
        return Err(CoreError::validation("choose at least one channel"));
    }
    spec.recipients.dedup();
    if spec.recipients.is_empty() || spec.recipients.len() > 50 {
        return Err(CoreError::validation("choose 1–50 recipients"));
    }
    Ok(())
}

async fn authorize(core: &Core, ctx: &Ctx, scope: RuleScope) -> CoreResult<Option<access::DocRef>> {
    match scope {
        RuleScope::Organization | RuleScope::DocumentType(_) => {
            ctx.require(Capability::ManageDocumentSchema)?;
            Ok(None)
        }
        RuleScope::Document(id) => Ok(Some(
            access::authorize(core, ctx, id, Permission::Edit).await?,
        )),
        RuleScope::Position(_) => {
            ctx.require(Capability::ManageAppointments)?;
            Ok(None)
        }
    }
}

async fn insert_recipients(
    conn: &mut PgConnection,
    rule_id: Uuid,
    recipients: &[Recipient],
) -> CoreResult<()> {
    for r in recipients {
        let (kind, user, group, dept, role) = match *r {
            Recipient::Owner => (RecipientKind::Owner, None, None, None, None),
            Recipient::DocumentDepartment => {
                (RecipientKind::DocumentDepartment, None, None, None, None)
            }
            Recipient::User(id) => (RecipientKind::User, Some(id), None, None, None),
            Recipient::Group(id) => (RecipientKind::Group, None, Some(id), None, None),
            Recipient::Department(id) => (RecipientKind::Department, None, None, Some(id), None),
            Recipient::Role(id) => (RecipientKind::Role, None, None, None, Some(id)),
        };
        sqlx::query!(
            "INSERT INTO notification_rule_recipients (id, rule_id, kind, user_id, group_id, department_id, role_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            Uuid::now_v7(),
            rule_id,
            kind as RecipientKind,
            user,
            group,
            dept,
            role
        )
        .execute(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => CoreError::validation("a chosen recipient no longer exists"),
            _ => CoreError::Db(e),
        })?;
    }
    Ok(())
}

fn rule_event(core_ctx: &Ctx, action: &str, id: Uuid, doc: Option<&access::DocRef>) -> AuditEvent {
    match doc {
        Some(d) => d
            .event(core_ctx, Category::Notification, action)
            .context("rule_id", id),
        None => AuditEvent::new(
            Category::Notification,
            action,
            core_ctx.actor.clone(),
            "notification_rule",
        )
        .object(id, None),
    }
}

fn spec_changes(e: AuditEvent, before: Option<&Rule>, s: &RuleSpec) -> AuditEvent {
    let recips = |r: &[Recipient]| r.iter().map(Recipient::key).collect::<Vec<_>>();
    e.change("event", before.map(|b| b.event.as_str()), s.event.as_str())
        .change(
            "offsets_days",
            before.map(|b| {
                b.offsets_days
                    .iter()
                    .map(|d| i64::from(*d))
                    .collect::<Vec<_>>()
            }),
            s.offsets_days
                .iter()
                .map(|d| i64::from(*d))
                .collect::<Vec<_>>(),
        )
        .change(
            "overdue_repeat_days",
            before.and_then(|b| b.overdue_repeat_days),
            s.overdue_repeat_days,
        )
        .change(
            "channels",
            before.map(|b| b.channels.iter().map(|c| c.as_str()).collect::<Vec<_>>()),
            s.channels.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        )
        .change(
            "recipients",
            before.map(|b| recips(&b.recipients.iter().map(|r| r.0).collect::<Vec<_>>())),
            recips(&s.recipients),
        )
        .change("is_active", before.map(|b| b.is_active), s.is_active)
}

pub async fn create(
    core: &Core,
    ctx: &Ctx,
    scope: RuleScope,
    mut spec: RuleSpec,
) -> CoreResult<Uuid> {
    let doc = authorize(core, ctx, scope).await?;
    check(scope, &mut spec)?;
    let (type_id, doc_id, position_id) = match scope {
        RuleScope::Organization => (None, None, None),
        RuleScope::DocumentType(t) => (Some(t), None, None),
        RuleScope::Document(d) => (None, Some(d), None),
        RuleScope::Position(p) => (None, None, Some(p)),
    };
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO notification_rules (id, document_type_id, document_id, position_id, event, offsets_days,
                                         overdue_repeat_days, channels, is_active, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        id,
        type_id,
        doc_id,
        position_id,
        spec.event as DueEvent,
        &spec.offsets_days,
        spec.overdue_repeat_days,
        &spec.channels as &[Channel],
        spec.is_active,
        ctx.user_id(),
    )
    .execute(&mut *tx)
    .await?;
    insert_recipients(&mut tx, id, &spec.recipients).await?;
    let e = rule_event(ctx, "notification_rule.created", id, doc.as_ref()).context(
        "scope",
        match scope {
            RuleScope::Organization => AuditValue::from("organization"),
            RuleScope::DocumentType(t) => AuditValue::from(format!("document_type:{t}")),
            RuleScope::Document(d) => AuditValue::from(format!("document:{d}")),
            RuleScope::Position(p) => AuditValue::from(format!("position:{p}")),
        },
    );
    pdms_audit::append(&mut tx, vec![spec_changes(e, None, &spec)]).await?;
    tx.commit().await?;
    Ok(id)
}

async fn get(core: &Core, id: Uuid) -> CoreResult<Rule> {
    load_all(&core.db)
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(CoreError::NotFound)
}

pub async fn update(core: &Core, ctx: &Ctx, id: Uuid, mut spec: RuleSpec) -> CoreResult<()> {
    let before = get(core, id).await?;
    let doc = authorize(core, ctx, before.scope).await?;
    check(before.scope, &mut spec)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE notification_rules SET event = $2, offsets_days = $3, overdue_repeat_days = $4, channels = $5,
                is_active = $6, updated_at = now() WHERE id = $1",
        id,
        spec.event as DueEvent,
        &spec.offsets_days,
        spec.overdue_repeat_days,
        &spec.channels as &[Channel],
        spec.is_active,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM notification_rule_recipients WHERE rule_id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    insert_recipients(&mut tx, id, &spec.recipients).await?;
    let e = spec_changes(
        rule_event(ctx, "notification_rule.updated", id, doc.as_ref()),
        Some(&before),
        &spec,
    );
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Delete a rule. Rules that already produced notifications are deactivated
/// instead, so the notification history keeps its rule.
pub async fn delete(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let before = get(core, id).await?;
    let doc = authorize(core, ctx, before.scope).await?;
    let mut tx = core.db.begin().await?;
    let used = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM notifications WHERE rule_id = $1) AS "e!""#,
        id
    )
    .fetch_one(&mut *tx)
    .await?;
    if used {
        sqlx::query!(
            "UPDATE notification_rules SET is_active = false, updated_at = now() WHERE id = $1",
            id
        )
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query!("DELETE FROM notification_rules WHERE id = $1", id)
            .execute(&mut *tx)
            .await?;
    }
    pdms_audit::append(
        &mut tx,
        vec![
            rule_event(ctx, "notification_rule.deleted", id, doc.as_ref()).change(
                "event",
                before.event.as_str(),
                AuditValue::Null,
            ),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(scope: RuleScope, event: DueEvent, active: bool) -> Rule {
        Rule {
            id: Uuid::new_v4(),
            scope,
            event,
            offsets_days: vec![30],
            overdue_repeat_days: None,
            channels: vec![Channel::Email],
            is_active: active,
            recipients: vec![(Recipient::Owner, "Owner".into())],
        }
    }

    #[test]
    fn most_specific_active_rules_win() {
        let (doc, ty, other_ty) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let rules = vec![
            rule(RuleScope::Organization, DueEvent::Review, true),
            rule(RuleScope::DocumentType(ty), DueEvent::Review, true),
            rule(RuleScope::Document(doc), DueEvent::Renewal, true),
            rule(RuleScope::Document(doc), DueEvent::Expiration, false),
        ];
        assert_eq!(
            effective(&rules, doc, ty, DueEvent::Review).1,
            RuleSource::DocumentType
        );
        assert_eq!(
            effective(&rules, doc, other_ty, DueEvent::Review).1,
            RuleSource::Organization
        );
        assert_eq!(
            effective(&rules, doc, ty, DueEvent::Renewal).1,
            RuleSource::Document
        );
        assert_eq!(
            effective(&rules, doc, ty, DueEvent::Expiration).1,
            RuleSource::None,
            "inactive override does not count"
        );
    }

    #[test]
    fn recipient_keys_round_trip() {
        let id = Uuid::from_u128(9);
        for r in [
            Recipient::Owner,
            Recipient::DocumentDepartment,
            Recipient::User(id),
            Recipient::Group(id),
            Recipient::Department(id),
            Recipient::Role(id),
        ] {
            assert_eq!(Recipient::parse(&r.key()), Some(r));
        }
        assert_eq!(Recipient::parse("everyone"), None);
    }
}
