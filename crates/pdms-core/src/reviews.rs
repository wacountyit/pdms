// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Review and renewal tasks: start, complete (which reschedules the
//! document), and approve. Completed tasks are the review history.

use chrono::{DateTime, NaiveDate, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::{Permission, check_self_approval};
use serde::Serialize;
use uuid::Uuid;

use crate::context::Ctx;
use crate::documents::{self, access};
use crate::error::{CoreError, CoreResult};
use crate::notifications::rules::DueEvent;
use crate::{Core, schedule};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "task_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Initiated,
    Completed,
    Approved,
    Overdue,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::Initiated => "initiated",
            TaskStatus::Completed => "completed",
            TaskStatus::Approved => "approved",
            TaskStatus::Overdue => "overdue",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Open => "Open",
            TaskStatus::Initiated => "In progress",
            TaskStatus::Completed => "Completed",
            TaskStatus::Approved => "Approved",
            TaskStatus::Overdue => "Overdue",
            TaskStatus::Cancelled => "Cancelled",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(
            self,
            TaskStatus::Open | TaskStatus::Initiated | TaskStatus::Overdue
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Task {
    pub id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub kind: DueEvent,
    pub due_date: NaiveDate,
    pub status: TaskStatus,
    pub initiated_by: Option<String>,
    pub initiated_at: Option<DateTime<Utc>>,
    pub completed_by_id: Option<Uuid>,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub completed_on: Option<NaiveDate>,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub outcome_note: String,
}

async fn query_tasks(
    core: &Core,
    document: Option<Uuid>,
    owner: Option<Uuid>,
    open_only: bool,
) -> CoreResult<Vec<Task>> {
    Ok(sqlx::query_as!(
        Task,
        r#"SELECT t.id, t.document_id, d.external_id, d.title, t.kind AS "kind: DueEvent", t.due_date,
                  t.status AS "status: TaskStatus",
                  ui.display_name AS "initiated_by?", t.initiated_at,
                  t.completed_by AS completed_by_id, uc.display_name AS "completed_by?", t.completed_at, t.completed_on,
                  ua.display_name AS "approved_by?", t.approved_at, t.outcome_note
             FROM review_tasks t
             JOIN documents d ON d.id = t.document_id
             LEFT JOIN users ui ON ui.id = t.initiated_by
             LEFT JOIN users uc ON uc.id = t.completed_by
             LEFT JOIN users ua ON ua.id = t.approved_by
            WHERE d.deleted_at IS NULL
              AND ($1::uuid IS NULL OR t.document_id = $1)
              AND ($2::uuid IS NULL OR d.owner_id = $2)
              AND (NOT $3 OR t.status IN ('open', 'initiated', 'overdue'))
            ORDER BY (t.status IN ('open', 'initiated', 'overdue')) DESC, t.due_date DESC"#,
        document,
        owner,
        open_only
    )
    .fetch_all(&core.db)
    .await?)
}

/// Tasks for a document, open first, then history (requires `view`).
pub async fn for_document(core: &Core, ctx: &Ctx, document_id: Uuid) -> CoreResult<Vec<Task>> {
    access::authorize(core, ctx, document_id, Permission::View).await?;
    query_tasks(core, Some(document_id), None, false).await
}

/// Open tasks on documents the caller owns.
pub async fn my_open(core: &Core, ctx: &Ctx) -> CoreResult<Vec<Task>> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let mut tasks = query_tasks(core, None, Some(user), true).await?;
    tasks.sort_by_key(|t| t.due_date);
    Ok(tasks)
}

async fn load(core: &Core, id: Uuid) -> CoreResult<Task> {
    sqlx::query_scalar!("SELECT document_id FROM review_tasks WHERE id = $1", id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(CoreError::NotFound)?;
    let doc = sqlx::query_scalar!("SELECT document_id FROM review_tasks WHERE id = $1", id)
        .fetch_one(&core.db)
        .await?;
    query_tasks(core, Some(doc), None, false)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or(CoreError::NotFound)
}

fn event(ctx: &Ctx, doc: &access::DocRef, action: &str, t: &Task) -> AuditEvent {
    doc.event(ctx, Category::Workflow, action)
        .context("task_id", t.id)
        .context("kind", t.kind.as_str())
        .context("due_date", t.due_date)
}

/// Mark a task as started (requires `edit`).
pub async fn start(core: &Core, ctx: &Ctx, task_id: Uuid) -> CoreResult<()> {
    let t = load(core, task_id).await?;
    let doc = access::authorize(core, ctx, t.document_id, Permission::Edit).await?;
    if !matches!(t.status, TaskStatus::Open | TaskStatus::Overdue) {
        return Err(CoreError::validation("only open tasks can be started"));
    }
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE review_tasks SET status = CASE WHEN status = 'overdue' THEN 'overdue'::task_status ELSE 'initiated'::task_status END,
                initiated_by = $2, initiated_at = now(), updated_at = now() WHERE id = $1",
        task_id,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(&mut tx, vec![event(ctx, &doc, "review_task.initiated", &t)]).await?;
    tx.commit().await?;
    Ok(())
}

/// How a task was completed.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    /// When the review or renewal actually happened (defaults to today).
    pub completed_on: Option<NaiveDate>,
    pub note: String,
    /// Renewals: the new renewal and/or expiration date (at least one required).
    pub new_renewal_date: Option<NaiveDate>,
    pub new_expiration_date: Option<NaiveDate>,
}

/// Complete a task (requires `edit`). A review sets the last review date and
/// schedules the next one; a renewal records the new dates. The document's
/// status is re-evaluated right away.
pub async fn complete(core: &Core, ctx: &Ctx, task_id: Uuid, c: Completion) -> CoreResult<()> {
    let t = load(core, task_id).await?;
    let doc = access::authorize(core, ctx, t.document_id, Permission::Edit).await?;
    if !t.status.is_open() {
        return Err(CoreError::validation("this task is already finished"));
    }
    let now = Utc::now();
    let today = schedule::local_today(now, core.config.timezone());
    let completed_on = c.completed_on.unwrap_or(today);
    if completed_on > today {
        return Err(CoreError::validation(
            "the completion date cannot be in the future",
        ));
    }
    let note: String = c.note.trim().chars().take(2000).collect();
    let d = sqlx::query!(
        r#"SELECT d.last_review_date, d.next_review_date, d.renewal_date, d.expiration_date,
                  coalesce(d.review_interval_months, t.review_interval_months) AS interval
             FROM documents d JOIN document_types t ON t.id = d.document_type_id WHERE d.id = $1"#,
        t.document_id
    )
    .fetch_one(&core.db)
    .await?;
    let change = match t.kind {
        DueEvent::Review => documents::ScheduleChange {
            last_review_date: Some(completed_on),
            next_review_date: d.interval.map(|m| schedule::next_review(completed_on, m)),
            renewal_date: d.renewal_date,
            expiration_date: d.expiration_date,
        },
        DueEvent::Renewal => {
            let later = |new: Option<NaiveDate>, old: Option<NaiveDate>| {
                new.is_some_and(|n| old.is_none_or(|o| n > o))
            };
            if !later(c.new_renewal_date, d.renewal_date)
                && !later(c.new_expiration_date, d.expiration_date)
            {
                return Err(CoreError::validation(
                    "enter the new renewal date or expiration date (later than the current one)",
                ));
            }
            documents::ScheduleChange {
                last_review_date: d.last_review_date,
                next_review_date: d.next_review_date,
                renewal_date: c.new_renewal_date.or(d.renewal_date),
                expiration_date: c.new_expiration_date.or(d.expiration_date),
            }
        }
        _ => {
            return Err(CoreError::validation(
                "only reviews and renewals have tasks",
            ));
        }
    };
    let defs = documents::fields::list(&core.db).await?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE review_tasks SET status = 'completed', completed_by = $2, completed_at = now(), completed_on = $3,
                outcome_note = $4, updated_at = now() WHERE id = $1",
        task_id,
        ctx.user_id(),
        completed_on,
        note
    )
    .execute(&mut *tx)
    .await?;
    let label = match t.kind {
        DueEvent::Review => "Review completed",
        _ => "Renewal completed",
    };
    let doc_event =
        documents::apply_schedule_change(&mut tx, ctx, &doc, &change, label, &defs).await?;
    sqlx::query("SELECT refresh_document_search($1)")
        .bind(t.document_id)
        .execute(&mut *tx)
        .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, &doc, "review_task.completed", &t)
                .context("completed_on", completed_on)
                .context("note", note),
            doc_event,
        ],
    )
    .await?;
    tx.commit().await?;
    crate::notifications::refresh_document(core, t.document_id, now).await?;
    Ok(())
}

/// Approve a completed review or renewal (requires `approve`; the person who
/// completed it may not approve it unless the policy allows self-approval).
pub async fn approve(core: &Core, ctx: &Ctx, task_id: Uuid) -> CoreResult<()> {
    let t = load(core, task_id).await?;
    let doc = access::authorize(core, ctx, t.document_id, Permission::Approve).await?;
    if t.status != TaskStatus::Completed {
        return Err(CoreError::validation(
            "only completed tasks can be approved",
        ));
    }
    if let (Some(approver), Some(completer)) = (ctx.user_id(), t.completed_by_id)
        && let Err(reason) = check_self_approval(&core.policy, approver, completer)
    {
        pdms_audit::record(
            &core.db,
            vec![
                event(ctx, &doc, "review_task.approval_denied", &t)
                    .outcome(pdms_audit::Outcome::Denied)
                    .context("reason", "self_approval"),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(Some(reason)));
    }
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE review_tasks SET status = 'approved', approved_by = $2, approved_at = now(), updated_at = now()
          WHERE id = $1 AND status = 'completed'",
        task_id,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(&mut tx, vec![event(ctx, &doc, "review_task.approved", &t)]).await?;
    tx.commit().await?;
    Ok(())
}
