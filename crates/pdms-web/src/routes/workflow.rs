// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The review/approval workflow and signature panels on the document page.

use axum::Extension;
use axum::extract::{Multipart, Path, State};
use axum::response::Response;
use axum_extra::extract::Form;
use pdms_authz::Assignee;
use pdms_core::directory;
use pdms_core::documents::types;
use pdms_core::signatures::{self, NewSignatory, SignatureStatus, SignedRecord};
use pdms_core::workflow::{self, StepDecision, StepStatus, Submission, WorkflowStatus};
use pdms_core::{CoreError, CoreResult};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::middleware::DeferredCsrf;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::upload;
use crate::views::{done, fmt_date, fmt_time};

pub struct StepRow {
    pub id: Uuid,
    pub stage: i32,
    pub kind_label: &'static str,
    pub assignee: String,
    pub status: &'static str,
    pub status_label: &'static str,
    pub acted: String,
    pub comment: String,
    pub can_act: bool,
    pub self_blocked: bool,
}

pub struct EventRow {
    pub when: String,
    pub who: String,
    pub label: String,
    pub comment: String,
}

pub struct RoundRow {
    pub id: Uuid,
    pub active: bool,
    pub status: &'static str,
    pub status_label: &'static str,
    pub submitted: String,
    pub note: String,
    pub version_no: i32,
    pub steps: Vec<StepRow>,
    pub events: Vec<EventRow>,
}

pub struct SignatoryRow {
    pub id: Uuid,
    pub who: String,
    pub email: String,
    pub status: &'static str,
    pub status_label: &'static str,
    pub detail: String,
    pub note: String,
    pub signed_version: Option<i32>,
    pub can_request: bool,
    pub can_record: bool,
    pub can_remove: bool,
}

/// Everything the workflow and signature sections of the document page need.
pub struct WorkflowView {
    pub rounds: Vec<RoundRow>,
    pub active: Option<Uuid>,
    pub can_submit: bool,
    pub can_cancel: bool,
    pub can_comment: bool,
    pub legal_required: bool,
    /// Picker options: ("user:<id>" | "role:<id>", label).
    pub people: Vec<(String, String)>,
    pub roles: Vec<(String, String)>,
    pub signatories: Vec<SignatoryRow>,
    pub can_manage_signatures: bool,
    pub requires_signatures: bool,
    pub signed: usize,
    pub signers: Vec<(Uuid, String)>,
    pub today: String,
}

impl WorkflowView {
    pub fn signatures_summary(&self) -> String {
        format!("{} of {} signed", self.signed, self.signatories.len())
    }
}

pub async fn panel_view(
    state: &AppState,
    user: &CurrentUser,
    document_id: Uuid,
    type_id: Uuid,
    can_edit: bool,
) -> AppResult<WorkflowView> {
    let core = &state.core;
    let p = workflow::panel(core, &user.ctx, document_id).await?;
    let who = |s: &Option<String>| s.clone().unwrap_or_else(|| "System".into());
    let rounds = p
        .workflows
        .iter()
        .map(|w| RoundRow {
            id: w.id,
            active: w.status == WorkflowStatus::InProgress,
            status: w.status.as_str(),
            status_label: w.status.label(),
            submitted: format!(
                "Submitted by {} on {}",
                who(&w.submitted_by),
                fmt_time(state.tz, w.submitted_at)
            ),
            note: w.note.clone(),
            version_no: w.version_no,
            steps: w
                .steps
                .iter()
                .map(|s| StepRow {
                    id: s.id,
                    stage: s.stage,
                    kind_label: s.kind.label(),
                    assignee: s.assignee_label.clone(),
                    status: s.status.as_str(),
                    status_label: s.status.label(),
                    acted: match (&s.acted_by, s.acted_at) {
                        (Some(by), Some(at)) => format!("{by}, {}", fmt_time(state.tz, at)),
                        _ => String::new(),
                    },
                    comment: s.comment.clone(),
                    can_act: s.status == StepStatus::Pending && p.actionable.contains(&s.id),
                    self_blocked: p.self_blocked.contains(&s.id),
                })
                .collect(),
            events: w
                .events
                .iter()
                .map(|e| EventRow {
                    when: fmt_time(state.tz, e.created_at),
                    who: e.actor.clone().unwrap_or_default(),
                    label: e.label().to_owned(),
                    comment: e.comment.clone(),
                })
                .collect(),
        })
        .collect();
    let (people, roles) = if p.can_submit {
        let users = directory::list_users(&core.db, "", 5000).await?;
        let people = users
            .into_iter()
            .filter(|u| u.status == pdms_core::model::UserStatus::Active)
            .map(|u| {
                (
                    format!("user:{}", u.id),
                    format!("{} ({})", u.display_name, u.email),
                )
            })
            .collect();
        let roles = directory::list_roles(&core.db)
            .await?
            .into_iter()
            .map(|r| (format!("role:{}", r.id), format!("Any {}", r.name)))
            .collect();
        (people, roles)
    } else {
        (Vec::new(), Vec::new())
    };
    let list = signatures::list(core, &user.ctx, document_id).await?;
    let signatories = list
        .iter()
        .map(|s| SignatoryRow {
            id: s.id,
            who: s.describe(),
            email: s.email.clone(),
            status: s.status.as_str(),
            status_label: s.status.label(),
            detail: match s.status {
                SignatureStatus::Signed => format!(
                    "Signed {}{}",
                    fmt_date(s.signed_on),
                    s.recorded_by
                        .as_ref()
                        .map(|b| format!(" · recorded by {b}"))
                        .unwrap_or_default()
                ),
                SignatureStatus::Requested => format!(
                    "Requested {}{}",
                    s.requested_at
                        .map(|t| fmt_time(state.tz, t))
                        .unwrap_or_default(),
                    s.requested_by
                        .as_ref()
                        .map(|b| format!(" by {b}"))
                        .unwrap_or_default()
                ),
                SignatureStatus::Declined => format!(
                    "Declined{}",
                    s.recorded_at
                        .map(|t| format!(" (recorded {})", fmt_time(state.tz, t)))
                        .unwrap_or_default()
                ),
                SignatureStatus::Pending => String::new(),
            },
            note: s.note.clone(),
            signed_version: s.signed_version_no,
            can_request: can_edit
                && matches!(
                    s.status,
                    SignatureStatus::Pending | SignatureStatus::Requested
                ),
            can_record: can_edit && s.status != SignatureStatus::Signed,
            can_remove: can_edit && s.status == SignatureStatus::Pending,
        })
        .collect();
    let signers = if can_edit {
        directory::list_users(&core.db, "", 5000)
            .await?
            .into_iter()
            .filter(|u| u.status == pdms_core::model::UserStatus::Active)
            .map(|u| (u.id, u.display_name))
            .collect()
    } else {
        Vec::new()
    };
    Ok(WorkflowView {
        rounds,
        active: p
            .workflows
            .iter()
            .find(|w| w.status == WorkflowStatus::InProgress)
            .map(|w| w.id),
        can_submit: p.can_submit,
        can_cancel: p.can_cancel,
        can_comment: p.can_comment,
        legal_required: p.legal_required,
        people,
        roles,
        signed: list
            .iter()
            .filter(|s| s.status == SignatureStatus::Signed)
            .count(),
        signatories,
        can_manage_signatures: can_edit,
        requires_signatures: types::get_type(&core.db, type_id)
            .await?
            .requires_signatures,
        signers,
        today: pdms_core::schedule::local_today(chrono::Utc::now(), state.tz).to_string(),
    })
}

fn back(id: Uuid, anchor: &str) -> String {
    format!("/documents/{id}#{anchor}")
}

fn assignees(values: &[String]) -> Vec<Assignee> {
    values
        .iter()
        .filter_map(|v| {
            let (kind, id) = v.split_once(':')?;
            let id: Uuid = id.parse().ok()?;
            match kind {
                "user" => Some(Assignee::User(id)),
                "role" => Some(Assignee::Role(id)),
                _ => None,
            }
        })
        .collect()
}

#[derive(Deserialize)]
pub struct SubmitForm {
    #[serde(default)]
    reviewer: Vec<String>,
    #[serde(default)]
    approver: Vec<String>,
    legal: Option<String>,
    #[serde(default)]
    note: String,
}

pub async fn submit(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<SubmitForm>,
) -> AppResult<Response> {
    let r = workflow::submit(
        &state.core,
        &user.ctx,
        id,
        Submission {
            reviewers: assignees(&f.reviewer),
            approvers: assignees(&f.approver),
            legal_review: f.legal.as_deref() == Some("1"),
            note: f.note,
        },
    )
    .await;
    done(
        &state,
        &back(id, "workflow"),
        r,
        "Submitted for review. The first reviewers have been notified.",
    )
}

#[derive(Deserialize)]
pub struct DecisionForm {
    decision: String,
    #[serde(default)]
    comment: String,
}

pub async fn decide(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, step)): Path<(Uuid, Uuid)>,
    Form(f): Form<DecisionForm>,
) -> AppResult<Response> {
    let Some(decision) = StepDecision::parse(&f.decision) else {
        return done::<()>(
            &state,
            &back(id, "workflow"),
            Err(CoreError::validation("choose a decision")),
            "",
        );
    };
    let r = workflow::decide_step(&state.core, &user.ctx, step, decision, &f.comment).await;
    let message = match decision {
        StepDecision::Approve => "Approved.",
        StepDecision::Reject => "Rejected. The submitter has been notified.",
        StepDecision::RequestChanges => "Changes requested. The submitter has been notified.",
    };
    match r {
        Err(CoreError::Forbidden(Some(pdms_authz::DenyReason::SelfApproval))) => {
            Ok(crate::views::redirect_with(
                &state,
                &back(id, "workflow"),
                false,
                "You submitted this document, so someone else must approve it.",
            ))
        }
        other => done(&state, &back(id, "workflow"), other, message),
    }
}

#[derive(Deserialize)]
pub struct TextForm {
    #[serde(default)]
    text: String,
}

pub async fn comment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, wf)): Path<(Uuid, Uuid)>,
    Form(f): Form<TextForm>,
) -> AppResult<Response> {
    let r = workflow::comment(&state.core, &user.ctx, wf, &f.text).await;
    done(&state, &back(id, "workflow"), r, "Comment added.")
}

pub async fn cancel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, wf)): Path<(Uuid, Uuid)>,
    Form(f): Form<TextForm>,
) -> AppResult<Response> {
    let r = workflow::cancel(&state.core, &user.ctx, wf, &f.text).await;
    done(&state, &back(id, "workflow"), r, "Review cancelled.")
}

#[derive(Deserialize)]
pub struct SignatoryForm {
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    organization: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    user: String,
}

pub async fn add_signatory(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<SignatoryForm>,
) -> AppResult<Response> {
    let r = async {
        let user_id = if f.user.trim().is_empty() {
            None
        } else {
            Some(
                f.user
                    .trim()
                    .parse()
                    .map_err(|_| CoreError::validation("choose a user"))?,
            )
        };
        // Picking a user fills in the name when it was left blank.
        let name = match (f.name.trim().is_empty(), user_id) {
            (true, Some(u)) => {
                directory::get_user(&state.core.db, u)
                    .await?
                    .user
                    .display_name
            }
            _ => f.name.clone(),
        };
        signatures::add(
            &state.core,
            &user.ctx,
            id,
            NewSignatory {
                name,
                title: f.title,
                organization: f.organization,
                email: f.email,
                user_id,
            },
        )
        .await
    }
    .await;
    done(&state, &back(id, "signatures"), r, "Signer added.")
}

pub async fn request_signature(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, s)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = signatures::request(&state.core, &user.ctx, s).await;
    done(&state, &back(id, "signatures"), r, "Signature requested.")
}

pub async fn remove_signatory(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, s)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = signatures::remove(&state.core, &user.ctx, s).await;
    done(&state, &back(id, "signatures"), r, "Signer removed.")
}

pub async fn decline_signature(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, s)): Path<(Uuid, Uuid)>,
    Form(f): Form<TextForm>,
) -> AppResult<Response> {
    let r = signatures::record_declined(&state.core, &user.ctx, s, &f.text).await;
    done(&state, &back(id, "signatures"), r, "Recorded as declined.")
}

/// Record a signature, optionally with the signed copy (multipart).
pub async fn record_signed(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, s)): Path<(Uuid, Uuid)>,
    deferred: Option<Extension<DeferredCsrf>>,
    multipart: Multipart,
) -> AppResult<Response> {
    let up = upload::read(&state, deferred.map(|e| e.0), multipart).await?;
    let r: CoreResult<()> = async {
        let signed_on = crate::routes::documents::parse_date("Signing date", up.get("signed_on"))?;
        signatures::record_signed(
            &state.core,
            &user.ctx,
            s,
            SignedRecord {
                signed_on,
                note: up.get("note").to_owned(),
                file: up.file,
            },
        )
        .await
    }
    .await;
    done(&state, &back(id, "signatures"), r, "Signature recorded.")
}
