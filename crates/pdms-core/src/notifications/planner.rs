// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The planner: applies date-driven statuses, keeps review/renewal tasks
//! current, and materializes due reminders (for documents and for
//! appointments) with their deliveries.
//!
//! Every step is idempotent. Notifications have a natural unique key, so
//! running the planner twice, concurrently, or again after a crash creates
//! nothing new. Each notification, its deliveries, the email jobs, and the
//! audit entry are written in one transaction.

use chrono::{DateTime, NaiveDate, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::{DocumentAttrs, PositionAttrs};
use sqlx::PgConnection;
use uuid::Uuid;

use super::dispatch::{self, Audience, Visibility};
use super::rules::{self, DueEvent, Rule};
use crate::Core;
use crate::context::Ctx;
use crate::documents::{self, DocumentStatus};
use crate::error::CoreResult;
use crate::schedule::{self, Trigger, TriggerKind};

pub const EMAIL_JOB: &str = "notifications.email";

#[derive(Copy, Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "notification_trigger", rename_all = "snake_case")]
pub enum DbTrigger {
    Advance,
    Due,
    Overdue,
}

impl From<TriggerKind> for DbTrigger {
    fn from(k: TriggerKind) -> Self {
        match k {
            TriggerKind::Advance => DbTrigger::Advance,
            TriggerKind::Due => DbTrigger::Due,
            TriggerKind::Overdue => DbTrigger::Overdue,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "delivery_status", rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Sending,
    Sent,
    Failed,
    Skipped,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryStatus::Pending => "pending",
            DeliveryStatus::Sending => "sending",
            DeliveryStatus::Sent => "sent",
            DeliveryStatus::Failed => "failed",
            DeliveryStatus::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanReport {
    pub status_changes: usize,
    pub tasks_created: usize,
    pub tasks_overdue: usize,
    pub notifications: usize,
    pub deliveries: usize,
}

fn scheduler() -> Ctx {
    Ctx::system("scheduler")
}

struct DocDates {
    id: Uuid,
    document_type_id: Uuid,
    department_id: Uuid,
    owner_id: Uuid,
    inherit_acl: bool,
    is_confidential: bool,
    status: DocumentStatus,
    next_review_date: Option<NaiveDate>,
    renewal_date: Option<NaiveDate>,
    expiration_date: Option<NaiveDate>,
    external_id: String,
    title: String,
}

async fn load_docs(conn: &mut PgConnection, only: Option<Uuid>) -> CoreResult<Vec<DocDates>> {
    Ok(sqlx::query_as!(
        DocDates,
        r#"SELECT id, document_type_id, department_id, owner_id, inherit_acl, is_confidential,
                  status AS "status: DocumentStatus", next_review_date, renewal_date, expiration_date,
                  external_id, title
             FROM documents
            WHERE deleted_at IS NULL
              AND status IN ('active', 'renewal_due', 'overdue', 'expired')
              AND ($1::uuid IS NULL OR id = $1)"#,
        only
    )
    .fetch_all(conn)
    .await?)
}

/// Run the full planner as of `now`.
pub async fn plan(core: &Core, now: DateTime<Utc>) -> CoreResult<PlanReport> {
    plan_inner(core, now, None).await
}

/// Re-plan one document right away (after its dates change).
pub async fn refresh_document(
    core: &Core,
    document_id: Uuid,
    now: DateTime<Utc>,
) -> CoreResult<PlanReport> {
    plan_inner(core, now, Some(document_id)).await
}

async fn plan_inner(core: &Core, now: DateTime<Utc>, only: Option<Uuid>) -> CoreResult<PlanReport> {
    let cfg = &core.config.notifications;
    let tz = core.config.timezone();
    let today = schedule::local_today(now, tz);
    let mut report = PlanReport::default();
    let ctx = scheduler();

    // 1. Date-driven statuses.
    let mut conn = core.db.acquire().await?;
    let docs = load_docs(&mut conn, only).await?;
    drop(conn);
    for d in &docs {
        if let Some(target) = schedule::evaluate_status(
            d.status,
            d.next_review_date,
            d.renewal_date,
            d.expiration_date,
            today,
            cfg.renewal_due_days,
        ) {
            let why = match target {
                DocumentStatus::Expired => {
                    format!("expiration date {} has passed", fmt(d.expiration_date))
                }
                DocumentStatus::Overdue => "a review or renewal date has passed".to_owned(),
                DocumentStatus::RenewalDue => format!(
                    "renewal date {} is within {} days",
                    fmt(d.renewal_date),
                    cfg.renewal_due_days
                ),
                _ => "dates no longer require attention".to_owned(),
            };
            match documents::change_status(core, &ctx, d.id, target, &why).await {
                Ok(()) => report.status_changes += 1,
                // Another planner run or a person changed it first.
                Err(crate::CoreError::Conflict(_)) => {}
                Err(e) => return Err(e),
            }
        }
    }

    // Reload: statuses may have changed.
    let mut conn = core.db.acquire().await?;
    let docs: Vec<DocDates> = load_docs(&mut conn, only)
        .await?
        .into_iter()
        .filter(|d| d.status != DocumentStatus::Expired)
        .collect();
    drop(conn);

    // 2. Tasks.
    for d in &docs {
        for (kind, due) in [
            (DueEvent::Review, d.next_review_date),
            (DueEvent::Renewal, d.renewal_date),
        ] {
            report.tasks_created +=
                sync_task(core, &ctx, d, kind, due, today, cfg.task_lead_days).await?;
        }
    }
    report.tasks_overdue += mark_overdue_tasks(core, &ctx, today, only).await?;

    // 3. Notifications, once today's send hour has arrived.
    if !schedule::send_window_open(now, tz, cfg.send_hour) {
        return Ok(report);
    }
    let all_rules = rules::load_all(&core.db).await?;
    for d in &docs {
        let mut target = None;
        for (event, due) in [
            (DueEvent::Review, d.next_review_date),
            (DueEvent::Renewal, d.renewal_date),
            (DueEvent::Expiration, d.expiration_date),
        ] {
            let Some(due) = due else { continue };
            let (rules, _) = rules::effective(&all_rules, d.id, d.document_type_id, event);
            for rule in rules {
                let Some(trigger) = schedule::latest_trigger(
                    due,
                    &rule.offsets_days,
                    rule.overdue_repeat_days,
                    today,
                    cfg.stale_after_days,
                ) else {
                    continue;
                };
                if target.is_none() {
                    target = Some(Target::document(core, d).await?);
                }
                let Some(t) = &target else { continue };
                if let Some(n) = create_notification(core, t, rule, event, due, trigger).await? {
                    report.notifications += 1;
                    report.deliveries += n;
                }
            }
        }
    }
    if only.is_none() {
        plan_appointments(core, &all_rules, today, None, &mut report).await?;
    }
    Ok(report)
}

/// Re-plan one appointment's reminders right away (after its dates change).
pub async fn refresh_appointment(
    core: &Core,
    appointment_id: Uuid,
    now: DateTime<Utc>,
) -> CoreResult<PlanReport> {
    let mut report = PlanReport::default();
    if !schedule::send_window_open(
        now,
        core.config.timezone(),
        core.config.notifications.send_hour,
    ) {
        return Ok(report);
    }
    let today = schedule::local_today(now, core.config.timezone());
    let all_rules = rules::load_all(&core.db).await?;
    plan_appointments(core, &all_rules, today, Some(appointment_id), &mut report).await?;
    Ok(report)
}

struct ApptDates {
    id: Uuid,
    position_id: Uuid,
    person_name: String,
    position_name: String,
    body_name: String,
    department_id: Option<Uuid>,
    manager_user_id: Option<Uuid>,
    end_date: Option<NaiveDate>,
    reappointment_due_date: Option<NaiveDate>,
}

/// Term-end and reappointment reminders for active appointments.
async fn plan_appointments(
    core: &Core,
    all_rules: &[Rule],
    today: NaiveDate,
    only: Option<Uuid>,
    report: &mut PlanReport,
) -> CoreResult<()> {
    let appts = sqlx::query_as!(
        ApptDates,
        r#"SELECT a.id, a.position_id, a.person_name, p.name AS position_name, p.body_name,
                  p.department_id, p.manager_user_id, a.end_date, a.reappointment_due_date
             FROM appointments a JOIN positions p ON p.id = a.position_id
            WHERE a.status = 'active' AND p.is_active
              AND ($1::uuid IS NULL OR a.id = $1)"#,
        only
    )
    .fetch_all(&core.db)
    .await?;
    let stale = core.config.notifications.stale_after_days;
    for a in &appts {
        let target = Target {
            document_id: None,
            appointment_id: Some(a.id),
            audience: Audience {
                owner: a.manager_user_id,
                department_id: a.department_id,
            },
            visibility: Visibility::Position(PositionAttrs {
                department_id: a.department_id,
                manager_user_id: a.manager_user_id,
            }),
            object: (
                "appointment",
                a.id,
                format!("{} — {}, {}", a.person_name, a.position_name, a.body_name),
                a.department_id,
            ),
        };
        for (event, due) in [
            (DueEvent::TermEnd, a.end_date),
            (DueEvent::Reappointment, a.reappointment_due_date),
        ] {
            let Some(due) = due else { continue };
            let (rules, _) = rules::effective_for_position(all_rules, a.position_id, event);
            for rule in rules {
                let Some(trigger) = schedule::latest_trigger(
                    due,
                    &rule.offsets_days,
                    rule.overdue_repeat_days,
                    today,
                    stale,
                ) else {
                    continue;
                };
                if let Some(n) =
                    create_notification(core, &target, rule, event, due, trigger).await?
                {
                    report.notifications += 1;
                    report.deliveries += n;
                }
            }
        }
    }
    Ok(())
}

fn fmt(d: Option<NaiveDate>) -> String {
    d.map(|d| d.to_string()).unwrap_or_default()
}

/// Ensure a task exists for the current due date (within the lead window),
/// and cancel open tasks for due dates that no longer apply.
async fn sync_task(
    core: &Core,
    ctx: &Ctx,
    d: &DocDates,
    kind: DueEvent,
    due: Option<NaiveDate>,
    today: NaiveDate,
    lead_days: i64,
) -> CoreResult<usize> {
    let mut tx = core.db.begin().await?;
    let cancelled = sqlx::query_scalar!(
        r#"UPDATE review_tasks SET status = 'cancelled', updated_at = now()
            WHERE document_id = $1 AND kind = $2 AND status IN ('open', 'initiated', 'overdue')
              AND ($3::date IS NULL OR due_date <> $3)
          RETURNING due_date"#,
        d.id,
        kind as DueEvent,
        due
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut events: Vec<AuditEvent> = cancelled
        .into_iter()
        .map(|old| {
            task_event(ctx, d, "review_task.cancelled", kind)
                .context("due_date", old)
                .context("reason", "the due date changed")
        })
        .collect();
    let mut created = 0;
    if let Some(due) = due
        && (due - today).num_days() <= lead_days
    {
        let id = sqlx::query_scalar!(
            "INSERT INTO review_tasks (id, document_id, kind, due_date, status)
             VALUES ($1, $2, $3, $4, CASE WHEN $4::date < $5::date THEN 'overdue'::task_status ELSE 'open'::task_status END)
             ON CONFLICT (document_id, kind, due_date) DO NOTHING RETURNING id",
            Uuid::now_v7(),
            d.id,
            kind as DueEvent,
            due,
            today
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = id {
            created = 1;
            events.push(
                task_event(ctx, d, "review_task.created", kind)
                    .context("task_id", id)
                    .context("due_date", due),
            );
        }
    }
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    Ok(created)
}

fn task_event(ctx: &Ctx, d: &DocDates, action: &str, kind: DueEvent) -> AuditEvent {
    AuditEvent::new(Category::Workflow, action, ctx.actor.clone(), "document")
        .object(d.id, Some(&format!("{} {}", d.external_id, d.title)))
        .document(d.id)
        .department(Some(d.department_id))
        .context("kind", kind.as_str())
}

async fn mark_overdue_tasks(
    core: &Core,
    ctx: &Ctx,
    today: NaiveDate,
    only: Option<Uuid>,
) -> CoreResult<usize> {
    let mut tx = core.db.begin().await?;
    let rows = sqlx::query!(
        r#"UPDATE review_tasks t SET status = 'overdue', updated_at = now()
             FROM documents d
            WHERE d.id = t.document_id AND t.status IN ('open', 'initiated') AND t.due_date < $1
              AND ($2::uuid IS NULL OR t.document_id = $2)
          RETURNING t.id, t.document_id, t.kind AS "kind: DueEvent", t.due_date, d.external_id, d.title, d.department_id"#,
        today,
        only
    )
    .fetch_all(&mut *tx)
    .await?;
    let events: Vec<AuditEvent> = rows
        .iter()
        .map(|r| {
            AuditEvent::new(
                Category::Workflow,
                "review_task.overdue",
                ctx.actor.clone(),
                "document",
            )
            .object(
                r.document_id,
                Some(&format!("{} {}", r.external_id, r.title)),
            )
            .document(r.document_id)
            .department(Some(r.department_id))
            .context("task_id", r.id)
            .context("kind", r.kind.as_str())
            .context("due_date", r.due_date)
        })
        .collect();
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

/// What a reminder is about.
struct Target {
    document_id: Option<Uuid>,
    appointment_id: Option<Uuid>,
    audience: Audience,
    visibility: Visibility,
    /// (object type, id, label, department) for the audit entry.
    object: (&'static str, Uuid, String, Option<Uuid>),
}

impl Target {
    async fn document(core: &Core, d: &DocDates) -> CoreResult<Target> {
        let attrs = DocumentAttrs {
            id: d.id,
            department_id: d.department_id,
            owner_id: d.owner_id,
            inherit_acl: d.inherit_acl,
            is_confidential: d.is_confidential,
        };
        Ok(Target {
            document_id: Some(d.id),
            appointment_id: None,
            audience: Audience {
                owner: Some(d.owner_id),
                department_id: Some(d.department_id),
            },
            visibility: Visibility::document(core, attrs).await?,
            object: (
                "document",
                d.id,
                format!("{} {}", d.external_id, d.title),
                Some(d.department_id),
            ),
        })
    }
}

/// Insert one notification with its deliveries. Returns `None` if it already
/// existed (planning is idempotent), else the number of deliveries created.
async fn create_notification(
    core: &Core,
    t: &Target,
    rule: &Rule,
    event: DueEvent,
    due: NaiveDate,
    trigger: Trigger,
) -> CoreResult<Option<usize>> {
    let mut tx = core.db.begin().await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO notifications (id, document_id, appointment_id, rule_id, event, due_date, trigger, offset_days,
                                    scheduled_for)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT ON CONSTRAINT notifications_key DO NOTHING RETURNING id",
        Uuid::now_v7(),
        t.document_id,
        t.appointment_id,
        rule.id,
        event as DueEvent,
        due,
        DbTrigger::from(trigger.kind) as DbTrigger,
        trigger.offset_days,
        trigger.date,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(notification_id) = id else {
        return Ok(None);
    };
    let recipients: Vec<_> = rule.recipients.iter().map(|r| r.0).collect();
    let users = dispatch::resolve(&mut tx, t.audience, &recipients).await?;
    let fan = dispatch::deliver(
        &mut tx,
        core,
        notification_id,
        &users,
        &rule.channels,
        &t.visibility,
    )
    .await?;
    let (kind, object_id, label, department) = &t.object;
    let mut e = AuditEvent::new(
        Category::Notification,
        "notification.created",
        scheduler().actor,
        kind,
    )
    .object(*object_id, Some(label))
    .document_opt(t.document_id)
    .department(*department);
    if let Some(a) = t.appointment_id {
        e = e.context("appointment_id", a);
    }
    pdms_audit::append(
        &mut tx,
        vec![
            e.context("notification_id", notification_id)
                .context("rule_id", rule.id)
                .context("event", event.as_str())
                .context("due_date", due)
                .context("trigger", trigger.kind.as_str())
                .context("offset_days", trigger.offset_days)
                .context("recipients", i64::try_from(users.len()).unwrap_or(i64::MAX))
                .context("emails_queued", fan.queued)
                .context("skipped", fan.skipped),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(Some(fan.created))
}
