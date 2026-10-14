// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Review and approval workflows.
//!
//! Submitting a document routes it through stages: named reviewers (in
//! parallel), then the legal counsel step when the document type requires it
//! (or the submitter asks for it), then named approvers (in parallel). Each
//! stage starts when the one before it is finished, and the people in it are
//! told their action is needed. When every step is approved the document
//! becomes Active; a rejection or a request for changes ends the round and
//! returns the document to where it was. Every round is kept as history.
//!
//! Separation of duties: the person who submitted a document cannot approve
//! it or give the legal sign-off (unless the organization's policy allows
//! self-approval), and approvers need the `approve` permission on the
//! document. Owners, reviewers, approvers, and administrators are distinct:
//! being an administrator confers nothing here.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::{
    Assignee, DenyReason, Permission, check_self_approval, decide, is_assignee, role_keys,
};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::context::Ctx;
use crate::documents::access::{self, DocRef};
use crate::documents::{self, DocumentStatus, status};
use crate::error::{CoreError, CoreResult};
use crate::notifications::actions::{self, Notice};
use crate::notifications::dispatch::{self, Visibility};
use crate::{Core, acl};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "workflow_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    InProgress,
    Approved,
    Rejected,
    ChangesRequested,
    Cancelled,
}

impl WorkflowStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowStatus::InProgress => "in_progress",
            WorkflowStatus::Approved => "approved",
            WorkflowStatus::Rejected => "rejected",
            WorkflowStatus::ChangesRequested => "changes_requested",
            WorkflowStatus::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkflowStatus::InProgress => "In progress",
            WorkflowStatus::Approved => "Approved",
            WorkflowStatus::Rejected => "Rejected",
            WorkflowStatus::ChangesRequested => "Changes requested",
            WorkflowStatus::Cancelled => "Cancelled",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "step_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Review,
    Approval,
    LegalReview,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Review => "review",
            StepKind::Approval => "approval",
            StepKind::LegalReview => "legal_review",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StepKind::Review => "Review",
            StepKind::Approval => "Approval",
            StepKind::LegalReview => "Legal review",
        }
    }

    /// Steps that sign off on the document (separation of duties applies).
    pub fn is_sign_off(self) -> bool {
        matches!(self, StepKind::Approval | StepKind::LegalReview)
    }

    /// The document permission needed to act on the step.
    pub fn permission(self) -> Permission {
        match self {
            StepKind::Approval => Permission::Approve,
            StepKind::Review | StepKind::LegalReview => Permission::View,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "step_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Waiting,
    Pending,
    Approved,
    Rejected,
    ChangesRequested,
    Skipped,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Waiting => "waiting",
            StepStatus::Pending => "pending",
            StepStatus::Approved => "approved",
            StepStatus::Rejected => "rejected",
            StepStatus::ChangesRequested => "changes_requested",
            StepStatus::Skipped => "skipped",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StepStatus::Waiting => "Waiting",
            StepStatus::Pending => "Awaiting action",
            StepStatus::Approved => "Approved",
            StepStatus::Rejected => "Rejected",
            StepStatus::ChangesRequested => "Changes requested",
            StepStatus::Skipped => "Not needed",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub id: Uuid,
    pub stage: i32,
    pub kind: StepKind,
    pub assignee: Assignee,
    pub assignee_label: String,
    pub status: StepStatus,
    pub acted_by_id: Option<Uuid>,
    pub acted_by: Option<String>,
    pub acted_at: Option<DateTime<Utc>>,
    pub comment: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkflowEvent {
    pub actor: Option<String>,
    pub action: String,
    pub comment: String,
    pub created_at: DateTime<Utc>,
}

impl WorkflowEvent {
    pub fn label(&self) -> &str {
        match self.action.as_str() {
            "submitted" => "Submitted for review",
            "approved" => "Approved",
            "rejected" => "Rejected",
            "changes_requested" => "Requested changes",
            "commented" => "Commented",
            "cancelled" => "Cancelled",
            "completed" => "Approved by everyone; now Active",
            other => other,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Workflow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_no: i32,
    pub status: WorkflowStatus,
    pub previous_status: DocumentStatus,
    pub submitted_by_id: Option<Uuid>,
    pub submitted_by: Option<String>,
    pub submitted_at: DateTime<Utc>,
    pub note: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub steps: Vec<Step>,
    pub events: Vec<WorkflowEvent>,
}

impl Workflow {
    /// The stage whose steps are awaiting action.
    pub fn current_stage(&self) -> Option<i32> {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Pending)
            .map(|s| s.stage)
            .min()
    }
}

/// Who to route a document to.
#[derive(Clone, Debug, Default)]
pub struct Submission {
    pub reviewers: Vec<Assignee>,
    /// At least one.
    pub approvers: Vec<Assignee>,
    /// Add the legal counsel step even when the type does not require it.
    pub legal_review: bool,
    pub note: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StepDecision {
    Approve,
    Reject,
    RequestChanges,
}

impl StepDecision {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(StepDecision::Approve),
            "reject" => Some(StepDecision::Reject),
            "request_changes" => Some(StepDecision::RequestChanges),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// The in-progress workflow for a document, if any.
pub async fn in_progress(db: &PgPool, document_id: Uuid) -> CoreResult<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM workflows WHERE document_id = $1 AND status = 'in_progress'",
        document_id
    )
    .fetch_optional(db)
    .await?)
}

async fn load(
    conn: &mut PgConnection,
    document_id: Option<Uuid>,
    only: Option<Uuid>,
) -> CoreResult<Vec<Workflow>> {
    let rows = sqlx::query!(
        r#"SELECT w.id, w.document_id, w.version_no, w.status AS "status: WorkflowStatus",
                  w.previous_status AS "previous_status: DocumentStatus", w.submitted_by,
                  u.display_name AS "submitted_by_name?", w.submitted_at, w.note, w.completed_at
             FROM workflows w LEFT JOIN users u ON u.id = w.submitted_by
            WHERE ($1::uuid IS NULL OR w.document_id = $1) AND ($2::uuid IS NULL OR w.id = $2)
            ORDER BY w.submitted_at DESC"#,
        document_id,
        only
    )
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let steps = sqlx::query!(
        r#"SELECT s.id, s.workflow_id, s.stage, s.kind AS "kind: StepKind", s.assignee_user_id, s.assignee_role_id,
                  coalesce(au.display_name, ro.name, '') AS "assignee_label!",
                  s.status AS "status: StepStatus", s.acted_by, ub.display_name AS "acted_by_name?", s.acted_at,
                  s.comment
             FROM workflow_steps s
             LEFT JOIN users au ON au.id = s.assignee_user_id
             LEFT JOIN roles ro ON ro.id = s.assignee_role_id
             LEFT JOIN users ub ON ub.id = s.acted_by
            WHERE s.workflow_id = ANY($1)
            ORDER BY s.stage, s.kind, coalesce(au.display_name, ro.name, '')"#,
        &ids
    )
    .fetch_all(&mut *conn)
    .await?;
    let events = sqlx::query!(
        r#"SELECT e.workflow_id, u.display_name AS "actor?", e.action, e.comment, e.created_at
             FROM workflow_events e LEFT JOIN users u ON u.id = e.actor_id
            WHERE e.workflow_id = ANY($1) ORDER BY e.created_at, e.id"#,
        &ids
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Workflow {
            steps: steps
                .iter()
                .filter(|s| s.workflow_id == r.id)
                .filter_map(|s| {
                    let assignee = match (s.assignee_user_id, s.assignee_role_id) {
                        (Some(u), _) => Assignee::User(u),
                        (None, Some(role)) => Assignee::Role(role),
                        (None, None) => return None,
                    };
                    Some(Step {
                        id: s.id,
                        stage: s.stage,
                        kind: s.kind,
                        assignee_label: match assignee {
                            Assignee::User(_) => s.assignee_label.clone(),
                            Assignee::Role(_) => format!("Any {}", s.assignee_label),
                        },
                        assignee,
                        status: s.status,
                        acted_by_id: s.acted_by,
                        acted_by: s.acted_by_name.clone(),
                        acted_at: s.acted_at,
                        comment: s.comment.clone(),
                    })
                })
                .collect(),
            events: events
                .iter()
                .filter(|e| e.workflow_id == r.id)
                .map(|e| WorkflowEvent {
                    actor: e.actor.clone(),
                    action: e.action.clone(),
                    comment: e.comment.clone(),
                    created_at: e.created_at,
                })
                .collect(),
            id: r.id,
            document_id: r.document_id,
            version_no: r.version_no,
            status: r.status,
            previous_status: r.previous_status,
            submitted_by_id: r.submitted_by,
            submitted_by: r.submitted_by_name,
            submitted_at: r.submitted_at,
            note: r.note,
            completed_at: r.completed_at,
        })
        .collect())
}

/// What the document page shows: every round (newest first) and what the
/// caller may do.
#[derive(Clone, Debug, Serialize)]
pub struct Panel {
    pub workflows: Vec<Workflow>,
    /// Pending steps the caller may decide.
    pub actionable: Vec<Uuid>,
    /// Pending steps assigned to the caller that separation of duties stops
    /// them from deciding.
    pub self_blocked: Vec<Uuid>,
    pub can_submit: bool,
    pub can_cancel: bool,
    pub can_comment: bool,
    /// The type requires the legal counsel step.
    pub legal_required: bool,
}

/// Workflow history and actions for a document (requires `view`).
pub async fn panel(core: &Core, ctx: &Ctx, document_id: Uuid) -> CoreResult<Panel> {
    let doc = access::authorize(core, ctx, document_id, Permission::View).await?;
    let perms = access::permissions(core, ctx, &doc).await?;
    let mut conn = core.db.acquire().await?;
    let workflows = load(&mut conn, Some(document_id), None).await?;
    let current = sqlx::query!(
        r#"SELECT d.status AS "status: DocumentStatus", t.requires_legal_review
             FROM documents d JOIN document_types t ON t.id = d.document_type_id WHERE d.id = $1"#,
        document_id
    )
    .fetch_one(&mut *conn)
    .await?;
    drop(conn);
    let active = workflows
        .iter()
        .find(|w| w.status == WorkflowStatus::InProgress);
    let (mut actionable, mut self_blocked) = (Vec::new(), Vec::new());
    if let (Some(w), Some(p)) = (active, ctx.principal()) {
        for s in w.steps.iter().filter(|s| s.status == StepStatus::Pending) {
            if !is_assignee(p, s.assignee, doc.attrs.department_id)
                || !perms.satisfies(s.kind.permission())
            {
                continue;
            }
            let sod = w
                .submitted_by_id
                .map(|sub| check_self_approval(&core.policy, p.user_id, sub));
            if s.kind.is_sign_off() && matches!(sod, Some(Err(_))) {
                self_blocked.push(s.id);
            } else {
                actionable.push(s.id);
            }
        }
    }
    let participant = active.is_some_and(|w| {
        ctx.user_id().is_some_and(|u| w.submitted_by_id == Some(u))
            || ctx.principal().is_some_and(|p| {
                w.steps
                    .iter()
                    .any(|s| is_assignee(p, s.assignee, doc.attrs.department_id))
            })
    });
    let edit = perms.satisfies(Permission::Edit);
    Ok(Panel {
        can_submit: edit && active.is_none() && submittable(current.status),
        can_cancel: active.is_some()
            && (edit
                || ctx
                    .user_id()
                    .is_some_and(|u| active.is_some_and(|w| w.submitted_by_id == Some(u)))),
        can_comment: active.is_some() && (edit || participant),
        legal_required: current.requires_legal_review,
        workflows,
        actionable,
        self_blocked,
    })
}

fn submittable(s: DocumentStatus) -> bool {
    s == DocumentStatus::InReview
        || status::transition(s, DocumentStatus::InReview, status::Actor::Person).is_some()
}

/// A step waiting for the caller.
#[derive(Clone, Debug, Serialize)]
pub struct PendingStep {
    pub step_id: Uuid,
    pub workflow_id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub kind: StepKind,
    pub submitted_by: Option<String>,
    pub submitted_at: DateTime<Utc>,
}

/// Steps awaiting the caller's decision, oldest first (separation of duties
/// and permissions already applied).
pub async fn my_pending(core: &Core, ctx: &Ctx) -> CoreResult<Vec<PendingStep>> {
    let Some(p) = ctx.principal() else {
        return Ok(Vec::new());
    };
    let role_ids: Vec<Uuid> = p.roles.iter().map(|r| r.role_id).collect();
    let rows = sqlx::query!(
        r#"SELECT s.id, s.workflow_id, s.kind AS "kind: StepKind", s.assignee_user_id, s.assignee_role_id,
                  w.document_id, w.submitted_by, w.submitted_at, u.display_name AS "submitted_by_name?",
                  d.external_id, d.title, d.department_id
             FROM workflow_steps s
             JOIN workflows w ON w.id = s.workflow_id
             JOIN documents d ON d.id = w.document_id
             LEFT JOIN users u ON u.id = w.submitted_by
            WHERE s.status = 'pending' AND w.status = 'in_progress' AND d.deleted_at IS NULL
              AND (s.assignee_user_id = $1 OR s.assignee_role_id = ANY($2))
            ORDER BY w.submitted_at"#,
        p.user_id,
        &role_ids
    )
    .fetch_all(&core.db)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let assignee = match (r.assignee_user_id, r.assignee_role_id) {
            (Some(u), _) => Assignee::User(u),
            (None, Some(role)) => Assignee::Role(role),
            (None, None) => continue,
        };
        if !is_assignee(p, assignee, r.department_id)
            || !access::can(core, ctx, r.document_id, r.kind.permission()).await?
        {
            continue;
        }
        if r.kind.is_sign_off()
            && r.submitted_by
                .is_some_and(|s| check_self_approval(&core.policy, p.user_id, s).is_err())
        {
            continue;
        }
        out.push(PendingStep {
            step_id: r.id,
            workflow_id: r.workflow_id,
            document_id: r.document_id,
            external_id: r.external_id,
            title: r.title,
            kind: r.kind,
            submitted_by: r.submitted_by_name,
            submitted_at: r.submitted_at,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Submitting
// ---------------------------------------------------------------------------

async fn legal_role(db: &PgPool) -> CoreResult<Uuid> {
    sqlx::query_scalar!(
        "SELECT id FROM roles WHERE key = $1",
        role_keys::LEGAL_COUNSEL
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| CoreError::validation("the legal counsel role is missing"))
}

/// Check that an assignee exists and can act; returns its display label.
async fn check_assignee(
    core: &Core,
    doc: &DocRef,
    a: Assignee,
    kind: StepKind,
    submitter: Option<Uuid>,
) -> CoreResult<()> {
    let grants = acl::relevant_grants(&core.db, doc.attrs.department_id, Some(doc.id())).await?;
    let can_act = |p: &pdms_authz::Principal| {
        p.active
            && decide(
                &core.policy,
                p,
                kind.permission(),
                &doc.attrs,
                &grants,
                Utc::now(),
            )
            .is_allowed()
            && !(kind.is_sign_off()
                && submitter
                    .is_some_and(|s| check_self_approval(&core.policy, p.user_id, s).is_err()))
    };
    match a {
        Assignee::User(id) => {
            let name = sqlx::query_scalar!(
                "SELECT display_name FROM users WHERE id = $1 AND status = 'active'",
                id
            )
            .fetch_optional(&core.db)
            .await?
            .ok_or_else(|| CoreError::validation("a chosen person is not an active user"))?;
            if kind.is_sign_off()
                && submitter.is_some_and(|s| check_self_approval(&core.policy, id, s).is_err())
            {
                return Err(CoreError::validation(
                    "you cannot approve your own submission; choose someone else (separation of duties)",
                ));
            }
            let p = crate::auth::principal::load(&core.db, id).await?;
            if !can_act(&p) {
                return Err(CoreError::validation(format!(
                    "{name} does not have the {} permission on this document",
                    kind.permission()
                )));
            }
        }
        Assignee::Role(id) => {
            let name = sqlx::query_scalar!("SELECT name FROM roles WHERE id = $1", id)
                .fetch_optional(&core.db)
                .await?
                .ok_or_else(|| CoreError::validation("a chosen role no longer exists"))?;
            let mut conn = core.db.acquire().await?;
            let holders =
                dispatch::role_holders(&mut conn, id, Some(doc.attrs.department_id)).await?;
            drop(conn);
            let mut any = false;
            for h in holders {
                if can_act(&crate::auth::principal::load(&core.db, h).await?) {
                    any = true;
                    break;
                }
            }
            if !any {
                return Err(CoreError::validation(format!(
                    "no one holding the {name} role can act on this document (they need the {} permission, \
                     and cannot be the submitter for an approval)",
                    kind.permission()
                )));
            }
        }
    }
    Ok(())
}

fn dedup(v: &[Assignee]) -> Vec<Assignee> {
    let mut seen = BTreeSet::new();
    v.iter()
        .copied()
        .filter(|a| {
            seen.insert(match a {
                Assignee::User(id) => (0, *id),
                Assignee::Role(id) => (1, *id),
            })
        })
        .collect()
}

fn workflow_event(ctx: &Ctx, doc: &DocRef, action: &str, workflow_id: Uuid) -> AuditEvent {
    doc.event(ctx, Category::Workflow, action)
        .context("workflow_id", workflow_id)
}

async fn record_event(
    conn: &mut PgConnection,
    workflow_id: Uuid,
    step_id: Option<Uuid>,
    actor: Option<Uuid>,
    action: &str,
    comment: &str,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO workflow_events (id, workflow_id, step_id, actor_id, action, comment)
         VALUES ($1, $2, $3, $4, $5, $6)",
        Uuid::now_v7(),
        workflow_id,
        step_id,
        actor,
        action,
        comment
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Submit a document for review and approval (requires `edit`). The document
/// moves to In Review and the first stage's people are told.
pub async fn submit(core: &Core, ctx: &Ctx, document_id: Uuid, s: Submission) -> CoreResult<Uuid> {
    let doc = access::authorize(core, ctx, document_id, Permission::Edit).await?;
    let d = sqlx::query!(
        r#"SELECT d.status AS "status: DocumentStatus", d.current_version, t.requires_legal_review
             FROM documents d JOIN document_types t ON t.id = d.document_type_id WHERE d.id = $1"#,
        document_id
    )
    .fetch_one(&core.db)
    .await?;
    if !submittable(d.status) {
        return Err(CoreError::validation(format!(
            "a {} document cannot be submitted for review",
            d.status.label()
        )));
    }
    if in_progress(&core.db, document_id).await?.is_some() {
        return Err(CoreError::validation("this document is already in review"));
    }
    let reviewers = dedup(&s.reviewers);
    let approvers = dedup(&s.approvers);
    if approvers.is_empty() {
        return Err(CoreError::validation("choose at least one approver"));
    }
    if reviewers.len() > 20 || approvers.len() > 20 {
        return Err(CoreError::validation(
            "choose at most 20 reviewers and 20 approvers",
        ));
    }
    let note: String = s.note.trim().chars().take(2000).collect();
    let submitter = ctx.user_id();
    let legal = d.requires_legal_review || s.legal_review;
    let mut stages: Vec<(StepKind, Vec<Assignee>)> = Vec::new();
    if !reviewers.is_empty() {
        stages.push((StepKind::Review, reviewers));
    }
    if legal {
        stages.push((
            StepKind::LegalReview,
            vec![Assignee::Role(legal_role(&core.db).await?)],
        ));
    }
    stages.push((StepKind::Approval, approvers));
    for (kind, list) in &stages {
        for a in list {
            check_assignee(core, &doc, *a, *kind, submitter).await?;
        }
    }

    let previous = if d.status == DocumentStatus::InReview {
        DocumentStatus::Draft
    } else {
        d.status
    };
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO workflows (id, document_id, version_no, previous_status, submitted_by, note)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        document_id,
        d.current_version,
        previous as DocumentStatus,
        submitter,
        note
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(x) if x.is_unique_violation() => {
            CoreError::Conflict("This document was just submitted by someone else.".into())
        }
        _ => CoreError::Db(e),
    })?;
    for (i, (kind, list)) in stages.iter().enumerate() {
        let stage = i32::try_from(i + 1).unwrap_or(i32::MAX);
        for a in list {
            let (user, role) = match a {
                Assignee::User(u) => (Some(*u), None),
                Assignee::Role(r) => (None, Some(*r)),
            };
            sqlx::query!(
                "INSERT INTO workflow_steps (id, workflow_id, stage, kind, assignee_user_id, assignee_role_id, status)
                 VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $3 = 1 THEN 'pending'::step_status ELSE 'waiting'::step_status END)",
                Uuid::now_v7(),
                id,
                stage,
                *kind as StepKind,
                user,
                role
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    record_event(&mut tx, id, None, submitter, "submitted", &note).await?;
    let mut events = Vec::new();
    if d.status != DocumentStatus::InReview {
        events.push(
            documents::set_status(
                &mut tx,
                ctx,
                &doc,
                d.status,
                DocumentStatus::InReview,
                "submitted for review",
            )
            .await?,
        );
    }
    let route: Vec<String> = stages
        .iter()
        .map(|(k, l)| format!("{} ×{}", k.as_str(), l.len()))
        .collect();
    events.push(
        workflow_event(ctx, &doc, "workflow.submitted", id)
            .context("version", d.current_version)
            .context("route", route.join(", "))
            .context("legal_review", legal)
            .context("note", note.clone()),
    );
    pdms_audit::append(&mut tx, events).await?;
    notify_stage(&mut tx, core, ctx, &doc, id, 1).await?;
    tx.commit().await?;
    Ok(id)
}

/// Tell the people in `stage` that their action is needed.
async fn notify_stage(
    conn: &mut PgConnection,
    core: &Core,
    ctx: &Ctx,
    doc: &DocRef,
    workflow_id: Uuid,
    stage: i32,
) -> CoreResult<()> {
    let steps = sqlx::query!(
        r#"SELECT s.kind AS "kind: StepKind", s.assignee_user_id, s.assignee_role_id, w.submitted_by,
                  u.display_name AS "submitter?", w.note
             FROM workflow_steps s JOIN workflows w ON w.id = s.workflow_id
             LEFT JOIN users u ON u.id = w.submitted_by
            WHERE s.workflow_id = $1 AND s.stage = $2 AND s.status = 'pending'"#,
        workflow_id,
        stage
    )
    .fetch_all(&mut *conn)
    .await?;
    let Some(first) = steps.first() else {
        return Ok(());
    };
    let kind = first.kind;
    let mut users = BTreeSet::new();
    for s in &steps {
        match (s.assignee_user_id, s.assignee_role_id) {
            (Some(u), _) => {
                users.insert(u);
            }
            (None, Some(r)) => {
                users.extend(
                    dispatch::role_holders(&mut *conn, r, Some(doc.attrs.department_id)).await?,
                );
            }
            (None, None) => {}
        }
    }
    if kind.is_sign_off()
        && !core.policy.allow_self_approval
        && let Some(sub) = first.submitted_by
    {
        users.remove(&sub);
    }
    let what = match kind {
        StepKind::Review => "Review requested",
        StepKind::Approval => "Approval requested",
        StepKind::LegalReview => "Legal review requested",
    };
    let by = first
        .submitter
        .clone()
        .unwrap_or_else(|| "Someone".to_owned());
    let mut body = format!(
        "{by} submitted {} \"{}\" for review. Your {} is needed.",
        doc.external_id,
        doc.title,
        kind.label().to_lowercase()
    );
    if !first.note.is_empty() {
        body.push_str(&format!(" Note: {}", first.note));
    }
    let visibility = Visibility::document(core, doc.attrs.clone()).await?;
    actions::send(
        conn,
        core,
        ctx,
        Notice {
            subject: format!("{what}: {}", doc.label()),
            body,
            link: format!("/documents/{}#workflow", doc.id()),
            document_id: Some(doc.id()),
            appointment_id: None,
            dedupe_key: format!("workflow:{workflow_id}:stage:{stage}"),
        },
        users,
        &visibility,
        workflow_event(ctx, doc, "notification.created", workflow_id),
    )
    .await?;
    Ok(())
}

/// Tell the submitter (and the owner) how the round ended.
async fn notify_outcome(
    conn: &mut PgConnection,
    core: &Core,
    ctx: &Ctx,
    doc: &DocRef,
    w: &Workflow,
    subject: String,
    body: String,
) -> CoreResult<()> {
    let (workflow_id, submitter) = (w.id, w.submitted_by_id);
    let mut users: BTreeSet<Uuid> = submitter.into_iter().collect();
    users.insert(doc.attrs.owner_id);
    if let Some(me) = ctx.user_id() {
        users.remove(&me);
    }
    let visibility = Visibility::document(core, doc.attrs.clone()).await?;
    actions::send(
        conn,
        core,
        ctx,
        Notice {
            subject,
            body,
            link: format!("/documents/{}#workflow", doc.id()),
            document_id: Some(doc.id()),
            appointment_id: None,
            dedupe_key: format!("workflow:{workflow_id}:outcome"),
        },
        users,
        &visibility,
        workflow_event(ctx, doc, "notification.created", workflow_id),
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Deciding
// ---------------------------------------------------------------------------

/// Where a document goes when a round ends without approval.
fn return_status(previous: DocumentStatus) -> DocumentStatus {
    if previous == DocumentStatus::Draft {
        DocumentStatus::Draft
    } else {
        // Date-driven statuses are re-applied by the planner right after.
        DocumentStatus::Active
    }
}

/// Approve, reject, or request changes on a step assigned to the caller.
/// Rejections and change requests need a comment.
pub async fn decide_step(
    core: &Core,
    ctx: &Ctx,
    step_id: Uuid,
    decision: StepDecision,
    comment: &str,
) -> CoreResult<()> {
    let principal = ctx.principal().ok_or(CoreError::Unauthenticated)?.clone();
    let step = sqlx::query!(
        r#"SELECT s.workflow_id, w.document_id FROM workflow_steps s JOIN workflows w ON w.id = s.workflow_id
            WHERE s.id = $1"#,
        step_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let doc = access::authorize(core, ctx, step.document_id, Permission::View).await?;
    let comment: String = comment.trim().chars().take(4000).collect();
    if decision != StepDecision::Approve && comment.is_empty() {
        return Err(CoreError::validation(
            "explain what is wrong or what needs to change",
        ));
    }

    let mut tx = core.db.begin().await?;
    // Serialize decisions on one workflow.
    sqlx::query!(
        "SELECT id FROM workflows WHERE id = $1 FOR UPDATE",
        step.workflow_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let w = load(&mut tx, None, Some(step.workflow_id))
        .await?
        .into_iter()
        .next()
        .ok_or(CoreError::NotFound)?;
    let s = w
        .steps
        .iter()
        .find(|s| s.id == step_id)
        .cloned()
        .ok_or(CoreError::NotFound)?;
    if w.status != WorkflowStatus::InProgress || s.status != StepStatus::Pending {
        return Err(CoreError::Conflict(
            "This step is no longer awaiting a decision. Reload to see what changed.".into(),
        ));
    }
    let deny = |reason: &str| {
        workflow_event(ctx, &doc, "workflow.decision_denied", w.id)
            .outcome(Outcome::Denied)
            .context("step_id", step_id)
            .context("reason", reason.to_owned())
    };
    if !is_assignee(&principal, s.assignee, doc.attrs.department_id) {
        drop(tx);
        pdms_audit::record(&core.db, vec![deny("not_assignee")]).await?;
        return Err(CoreError::Forbidden(None));
    }
    if s.kind.is_sign_off()
        && let Some(sub) = w.submitted_by_id
        && let Err(reason) = check_self_approval(&core.policy, principal.user_id, sub)
    {
        drop(tx);
        pdms_audit::record(&core.db, vec![deny("self_approval")]).await?;
        return Err(CoreError::Forbidden(Some(reason)));
    }
    if s.kind.permission() != Permission::View
        && !access::can(core, ctx, doc.id(), s.kind.permission()).await?
    {
        drop(tx);
        pdms_audit::record(&core.db, vec![deny("missing_permission")]).await?;
        return Err(CoreError::Forbidden(Some(DenyReason::NoMatchingGrant)));
    }
    if decision == StepDecision::Approve {
        let newer_file = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM document_versions WHERE document_id = $1 AND version_no > $2
                                AND kind IN ('initial', 'revision', 'amendment', 'restore')) AS "e!""#,
            doc.id(),
            w.version_no
        )
        .fetch_one(&mut *tx)
        .await?;
        if newer_file {
            return Err(CoreError::Conflict(
                "A new file was added after this was submitted. Cancel this review and submit again so \
                 everyone sees the current file."
                    .into(),
            ));
        }
    }

    let (step_status, action) = match decision {
        StepDecision::Approve => (StepStatus::Approved, "approved"),
        StepDecision::Reject => (StepStatus::Rejected, "rejected"),
        StepDecision::RequestChanges => (StepStatus::ChangesRequested, "changes_requested"),
    };
    sqlx::query!(
        "UPDATE workflow_steps SET status = $2, acted_by = $3, acted_at = now(), comment = $4 WHERE id = $1",
        step_id,
        step_status as StepStatus,
        principal.user_id,
        comment
    )
    .execute(&mut *tx)
    .await?;
    record_event(
        &mut tx,
        w.id,
        Some(step_id),
        Some(principal.user_id),
        action,
        &comment,
    )
    .await?;
    let mut events = vec![
        workflow_event(ctx, &doc, &format!("workflow.step_{action}"), w.id)
            .context("step_id", step_id)
            .context("step_kind", s.kind.as_str())
            .context("stage", s.stage)
            .context("comment", comment.clone()),
    ];
    let actor_name = ctx.actor.display.clone().unwrap_or_default();
    let mut refresh = false;

    match decision {
        StepDecision::Approve => {
            let stage_open = w
                .steps
                .iter()
                .any(|o| o.id != step_id && o.stage == s.stage && o.status == StepStatus::Pending);
            let next = w
                .steps
                .iter()
                .filter(|o| o.status == StepStatus::Waiting)
                .map(|o| o.stage)
                .min();
            match (stage_open, next) {
                (true, _) => {}
                (false, Some(next)) => {
                    sqlx::query!(
                        "UPDATE workflow_steps SET status = 'pending' WHERE workflow_id = $1 AND stage = $2",
                        w.id,
                        next
                    )
                    .execute(&mut *tx)
                    .await?;
                    pdms_audit::append(&mut tx, std::mem::take(&mut events)).await?;
                    notify_stage(&mut tx, core, ctx, &doc, w.id, next).await?;
                }
                (false, None) => {
                    sqlx::query!(
                        "UPDATE workflows SET status = 'approved', completed_at = now() WHERE id = $1",
                        w.id
                    )
                    .execute(&mut *tx)
                    .await?;
                    record_event(&mut tx, w.id, None, None, "completed", "").await?;
                    events.push(
                        documents::set_status(
                            &mut tx,
                            ctx,
                            &doc,
                            DocumentStatus::InReview,
                            DocumentStatus::Active,
                            "approved through the review workflow",
                        )
                        .await?,
                    );
                    events.push(workflow_event(ctx, &doc, "workflow.approved", w.id));
                    pdms_audit::append(&mut tx, std::mem::take(&mut events)).await?;
                    notify_outcome(
                        &mut tx,
                        core,
                        ctx,
                        &doc,
                        &w,
                        format!("Approved: {}", doc.label()),
                        format!(
                            "Every reviewer and approver approved {} \"{}\". It is now Active.",
                            doc.external_id, doc.title
                        ),
                    )
                    .await?;
                    refresh = true;
                }
            }
        }
        StepDecision::Reject | StepDecision::RequestChanges => {
            let (wf_status, verb) = if decision == StepDecision::Reject {
                (WorkflowStatus::Rejected, "rejected")
            } else {
                (WorkflowStatus::ChangesRequested, "asked for changes to")
            };
            sqlx::query!(
                "UPDATE workflows SET status = $2, completed_at = now() WHERE id = $1",
                w.id,
                wf_status as WorkflowStatus
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "UPDATE workflow_steps SET status = 'skipped' WHERE workflow_id = $1 AND status IN ('waiting', 'pending')",
                w.id
            )
            .execute(&mut *tx)
            .await?;
            let back = return_status(w.previous_status);
            events.push(
                documents::set_status(
                    &mut tx,
                    ctx,
                    &doc,
                    DocumentStatus::InReview,
                    back,
                    &format!("review ended: {}", wf_status.label().to_lowercase()),
                )
                .await?,
            );
            events.push(workflow_event(
                ctx,
                &doc,
                &format!("workflow.{}", wf_status.as_str()),
                w.id,
            ));
            pdms_audit::append(&mut tx, std::mem::take(&mut events)).await?;
            notify_outcome(
                &mut tx,
                core,
                ctx,
                &doc,
                &w,
                format!("{}: {}", wf_status.label(), doc.label()),
                format!(
                    "{actor_name} {verb} {} \"{}\" ({} step): {comment}",
                    doc.external_id,
                    doc.title,
                    s.kind.label().to_lowercase()
                ),
            )
            .await?;
            refresh = true;
        }
    }
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    if refresh {
        crate::notifications::refresh_document(core, doc.id(), Utc::now()).await?;
    }
    Ok(())
}

/// Add a comment to the in-progress round (participants and editors).
pub async fn comment(core: &Core, ctx: &Ctx, workflow_id: Uuid, text: &str) -> CoreResult<()> {
    let document_id = sqlx::query_scalar!(
        "SELECT document_id FROM workflows WHERE id = $1",
        workflow_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let p = panel(core, ctx, document_id).await?;
    let active = p
        .workflows
        .iter()
        .find(|w| w.id == workflow_id && w.status == WorkflowStatus::InProgress);
    if active.is_none() {
        return Err(CoreError::validation("this review has finished"));
    }
    let doc = access::authorize(core, ctx, document_id, Permission::View).await?;
    if !p.can_comment {
        pdms_audit::record(
            &core.db,
            vec![
                workflow_event(ctx, &doc, "workflow.comment_denied", workflow_id)
                    .outcome(Outcome::Denied),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(None));
    }
    let text: String = text.trim().chars().take(4000).collect();
    if text.is_empty() {
        return Err(CoreError::validation("write a comment"));
    }
    let mut tx = core.db.begin().await?;
    record_event(
        &mut tx,
        workflow_id,
        None,
        ctx.user_id(),
        "commented",
        &text,
    )
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![workflow_event(ctx, &doc, "workflow.commented", workflow_id).context("comment", text)],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Withdraw an in-progress round (the submitter or an editor). The document
/// returns to where it was.
pub async fn cancel(core: &Core, ctx: &Ctx, workflow_id: Uuid, reason: &str) -> CoreResult<()> {
    let document_id = sqlx::query_scalar!(
        "SELECT document_id FROM workflows WHERE id = $1",
        workflow_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let p = panel(core, ctx, document_id).await?;
    let doc = access::authorize(core, ctx, document_id, Permission::View).await?;
    if !p.can_cancel {
        pdms_audit::record(
            &core.db,
            vec![
                workflow_event(ctx, &doc, "workflow.cancel_denied", workflow_id)
                    .outcome(Outcome::Denied),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(None));
    }
    let reason: String = reason.trim().chars().take(2000).collect();
    let mut tx = core.db.begin().await?;
    let previous = sqlx::query_scalar!(
        r#"UPDATE workflows SET status = 'cancelled', completed_at = now()
            WHERE id = $1 AND status = 'in_progress' RETURNING previous_status AS "s: DocumentStatus""#,
        workflow_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| CoreError::Conflict("This review has already finished.".into()))?;
    sqlx::query!(
        "UPDATE workflow_steps SET status = 'skipped' WHERE workflow_id = $1 AND status IN ('waiting', 'pending')",
        workflow_id
    )
    .execute(&mut *tx)
    .await?;
    record_event(
        &mut tx,
        workflow_id,
        None,
        ctx.user_id(),
        "cancelled",
        &reason,
    )
    .await?;
    let status_event = documents::set_status(
        &mut tx,
        ctx,
        &doc,
        DocumentStatus::InReview,
        return_status(previous),
        "review cancelled",
    )
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            status_event,
            workflow_event(ctx, &doc, "workflow.cancelled", workflow_id).context("reason", reason),
        ],
    )
    .await?;
    tx.commit().await?;
    crate::notifications::refresh_document(core, document_id, Utc::now()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_that_fail_return_the_document() {
        assert_eq!(return_status(DocumentStatus::Draft), DocumentStatus::Draft);
        assert_eq!(
            return_status(DocumentStatus::Active),
            DocumentStatus::Active
        );
        assert_eq!(
            return_status(DocumentStatus::Overdue),
            DocumentStatus::Active
        );
    }

    #[test]
    fn which_statuses_can_be_submitted() {
        use DocumentStatus::*;
        for s in [Draft, InReview, Active, RenewalDue, Overdue] {
            assert!(submittable(s), "{s:?}");
        }
        for s in [Expired, Superseded, Archived] {
            assert!(!submittable(s), "{s:?}");
        }
    }

    #[test]
    fn sign_off_steps() {
        assert!(StepKind::Approval.is_sign_off());
        assert!(StepKind::LegalReview.is_sign_off());
        assert!(!StepKind::Review.is_sign_off());
        assert_eq!(StepKind::Approval.permission(), Permission::Approve);
        assert_eq!(StepKind::LegalReview.permission(), Permission::View);
    }
}
