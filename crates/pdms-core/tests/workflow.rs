#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 3: review and approval workflows, separation of duties, and signatures.

mod common;

use std::sync::Arc;

use chrono::Utc;
use common::*;
use pdms_authz::{Assignee, DenyReason, Permission, Scope, Subject};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, VersionKind, types};
use pdms_core::notifications::mailer::MemoryMailer;
use pdms_core::notifications::{delivery, inbox};
use pdms_core::signatures::{self, NewSignatory, SignatureStatus, SignedRecord};
use pdms_core::workflow::{self, StepDecision, StepKind, StepStatus, Submission, WorkflowStatus};
use pdms_core::{Core, CoreError, acl, directory};
use uuid::Uuid;

struct World {
    core: Core,
    _db: pdms_testkit::TestDb,
    mail: MemoryMailer,
    dept: Uuid,
    owner: Uuid,
    reviewer: Uuid,
    lawyer: Uuid,
    approver: Uuid,
    outsider: Uuid,
}

async fn world() -> World {
    let (mut core, db) = core().await;
    let mail = MemoryMailer::default();
    core.mailer = Arc::new(mail.clone());
    let (owner, dept) = user_in_new_department(&core, "OPS", "owner@x.org").await;
    let mk = |email: &'static str, dept: Option<Uuid>| {
        let core = core.clone();
        async move {
            directory::create_user(&core.db, &system(), email, email, dept)
                .await
                .unwrap()
        }
    };
    let reviewer = mk("reviewer@x.org", Some(dept)).await;
    let lawyer = mk("lawyer@x.org", None).await;
    let approver = mk("approver@x.org", Some(dept)).await;
    let (outsider, _) = user_in_new_department(&core, "HR", "outsider@x.org").await;
    let legal = directory::role_id_by_key(&core.db, "legal_counsel")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), lawyer, legal, None)
        .await
        .unwrap();
    // The owner may edit; the approver is the department head (default approver).
    acl::grant(
        &core.db,
        &system(),
        Subject::User(owner),
        Scope::Department(dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    let head = directory::role_id_by_key(&core.db, "department_head")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), approver, head, Some(dept))
        .await
        .unwrap();
    World {
        core,
        _db: db,
        mail,
        dept,
        owner,
        reviewer,
        lawyer,
        approver,
        outsider,
    }
}

async fn draft(w: &World, type_key: &str) -> Uuid {
    let t = types::type_by_key(&w.core.db, type_key).await.unwrap().id;
    let file = upload(&w.core, "agreement.pdf", &pdf("v1")).await;
    documents::create(
        &w.core,
        &system(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: "Equipment maintenance agreement".into(),
                document_type_id: t,
                department_id: w.dept,
                ..Default::default()
            },
            owner_id: w.owner,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: Some(file),
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

async fn status_of(core: &Core, id: Uuid) -> DocumentStatus {
    documents::view(core, &system(), id)
        .await
        .unwrap()
        .document
        .status
}

async fn pending_step(w: &World, doc: Uuid, kind: StepKind) -> Uuid {
    let p = workflow::panel(&w.core, &system(), doc).await.unwrap();
    p.workflows[0]
        .steps
        .iter()
        .find(|s| s.kind == kind && s.status == StepStatus::Pending)
        .unwrap_or_else(|| panic!("no pending {kind:?} step"))
        .id
}

async fn inbox_subjects(core: &Core, user: Uuid) -> Vec<String> {
    inbox::list(core, &as_user(core, user).await, true)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.headline)
        .collect()
}

async fn send_all(core: &Core) {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM notification_deliveries WHERE channel = 'email' AND status = 'pending' ORDER BY id",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    for id in ids {
        delivery::send_email(core, id, "test").await.unwrap();
    }
}

#[tokio::test]
async fn ila_goes_through_review_legal_and_approval() {
    let w = world().await;
    let doc = draft(&w, "partnership_agreement").await;
    let owner = as_user(&w.core, w.owner).await;
    let id = workflow::submit(
        &w.core,
        &owner,
        doc,
        Submission {
            reviewers: vec![Assignee::User(w.reviewer)],
            approvers: vec![Assignee::User(w.approver)],
            legal_review: false, // required by the agreement type anyway
            note: "Please review before the board meets".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::InReview);
    let panel = workflow::panel(&w.core, &owner, doc).await.unwrap();
    let kinds: Vec<(i32, StepKind, StepStatus)> = panel.workflows[0]
        .steps
        .iter()
        .map(|s| (s.stage, s.kind, s.status))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (1, StepKind::Review, StepStatus::Pending),
            (2, StepKind::LegalReview, StepStatus::Waiting),
            (3, StepKind::Approval, StepStatus::Waiting),
        ]
    );
    assert!(panel.legal_required);
    // Only the first stage was told.
    assert_eq!(
        inbox_subjects(&w.core, w.reviewer).await,
        vec!["Review requested: PAG-2026-000001 Equipment maintenance agreement".to_owned()]
            .into_iter()
            .map(|s| s.replace("2026", &Utc::now().format("%Y").to_string()))
            .collect::<Vec<_>>()
    );
    assert!(inbox_subjects(&w.core, w.lawyer).await.is_empty());

    // Someone who is not the assignee cannot decide.
    let step = pending_step(&w, doc, StepKind::Review).await;
    let r = workflow::decide_step(
        &w.core,
        &as_user(&w.core, w.approver).await,
        step,
        StepDecision::Approve,
        "",
    )
    .await;
    assert!(matches!(r, Err(CoreError::Forbidden(_))), "{r:?}");

    let reviewer = as_user(&w.core, w.reviewer).await;
    let mine = workflow::my_pending(&w.core, &reviewer).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, StepKind::Review);
    workflow::decide_step(
        &w.core,
        &reviewer,
        step,
        StepDecision::Approve,
        "Looks right",
    )
    .await
    .unwrap();

    // The legal counsel (any holder of the role) is next.
    assert!(inbox_subjects(&w.core, w.lawyer).await[0].starts_with("Legal review requested"));
    let lawyer = as_user(&w.core, w.lawyer).await;
    let legal_step = pending_step(&w, doc, StepKind::LegalReview).await;
    assert_eq!(
        workflow::my_pending(&w.core, &lawyer).await.unwrap().len(),
        1
    );
    workflow::decide_step(
        &w.core,
        &lawyer,
        legal_step,
        StepDecision::Approve,
        "Approved as to form",
    )
    .await
    .unwrap();

    let approver = as_user(&w.core, w.approver).await;
    let approval = pending_step(&w, doc, StepKind::Approval).await;
    workflow::comment(&w.core, &approver, id, "Will sign after the meeting")
        .await
        .unwrap();
    workflow::decide_step(&w.core, &approver, approval, StepDecision::Approve, "")
        .await
        .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::Active);
    let panel = workflow::panel(&w.core, &owner, doc).await.unwrap();
    assert_eq!(panel.workflows[0].status, WorkflowStatus::Approved);
    assert!(panel.workflows[0].completed_at.is_some());
    let actions: Vec<&str> = panel.workflows[0]
        .events
        .iter()
        .map(|e| e.action.as_str())
        .collect();
    assert_eq!(
        actions,
        vec![
            "submitted",
            "approved",
            "approved",
            "commented",
            "approved",
            "completed"
        ]
    );
    assert!(inbox_subjects(&w.core, w.owner).await[0].starts_with("Approved: PAG-"));

    // Every action notice also went out by email.
    send_all(&w.core).await;
    let subjects: Vec<String> = w.mail.messages().into_iter().map(|m| m.subject).collect();
    assert!(subjects.iter().any(|s| s.contains("Review requested")));
    assert!(
        subjects
            .iter()
            .any(|s| s.contains("Legal review requested"))
    );
    assert!(subjects.iter().any(|s| s.contains("Approval requested")));
    assert!(subjects.iter().any(|s| s.contains("Approved: PAG-")));
    let msg = w
        .mail
        .messages()
        .into_iter()
        .find(|m| m.subject.contains("Approval requested"))
        .unwrap();
    assert!(msg.text.contains(&format!("/documents/{doc}#workflow")));
    assert!(msg.text.contains("Please review before the board meets"));

    let audit = audit_actions(&w.core).await;
    for a in [
        "workflow.submitted",
        "workflow.step_approved",
        "workflow.commented",
        "workflow.approved",
        "document.status_changed",
        "notification.created",
        "notification.sent",
    ] {
        assert!(audit.iter().any(|x| x == a), "missing {a}");
    }
}

#[tokio::test]
async fn separation_of_duties_holds() {
    let w = world().await;
    let doc = draft(&w, "partnership_agreement").await;
    // The approver (department head) submits, so they cannot approve their own submission.
    let approver = as_user(&w.core, w.approver).await;
    acl::grant(
        &w.core.db,
        &system(),
        Subject::User(w.approver),
        Scope::Department(w.dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    let r = workflow::submit(
        &w.core,
        &approver,
        doc,
        Submission {
            approvers: vec![Assignee::User(w.approver)],
            ..Default::default()
        },
    )
    .await;
    assert!(
        matches!(&r, Err(CoreError::Validation(m)) if m.contains("separation of duties")),
        "{r:?}"
    );
    // Approvers need the approve permission.
    let r = workflow::submit(
        &w.core,
        &approver,
        doc,
        Submission {
            approvers: vec![Assignee::User(w.reviewer)],
            ..Default::default()
        },
    )
    .await;
    assert!(
        matches!(&r, Err(CoreError::Validation(m)) if m.contains("approve permission")),
        "{r:?}"
    );
    assert_eq!(
        status_of(&w.core, doc).await,
        DocumentStatus::Draft,
        "nothing changed"
    );

    // The lawyer submits a document they also hold the legal role for: the
    // legal step needs another holder.
    acl::grant(
        &w.core.db,
        &system(),
        Subject::User(w.lawyer),
        Scope::Department(w.dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    let lawyer = as_user(&w.core, w.lawyer).await;
    let r = workflow::submit(
        &w.core,
        &lawyer,
        doc,
        Submission {
            approvers: vec![Assignee::User(w.approver)],
            ..Default::default()
        },
    )
    .await;
    assert!(
        matches!(&r, Err(CoreError::Validation(m)) if m.contains("Legal Counsel")),
        "{r:?}"
    );
    let second = directory::create_user(
        &w.core.db,
        &system(),
        "Second lawyer",
        "lawyer2@x.org",
        None,
    )
    .await
    .unwrap();
    let legal = directory::role_id_by_key(&w.core.db, "legal_counsel")
        .await
        .unwrap();
    directory::assign_role(&w.core.db, &system(), second, legal, None)
        .await
        .unwrap();
    workflow::submit(
        &w.core,
        &lawyer,
        doc,
        Submission {
            approvers: vec![Assignee::User(w.approver)],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // The submitting lawyer sees the step but is blocked from it, and was not notified.
    let step = pending_step(&w, doc, StepKind::LegalReview).await;
    let panel = workflow::panel(&w.core, &lawyer, doc).await.unwrap();
    assert_eq!(panel.self_blocked, vec![step]);
    assert!(panel.actionable.is_empty());
    assert!(
        workflow::my_pending(&w.core, &lawyer)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !inbox_subjects(&w.core, w.lawyer)
            .await
            .iter()
            .any(|s| s.starts_with("Legal review requested"))
    );
    let r = workflow::decide_step(&w.core, &lawyer, step, StepDecision::Approve, "").await;
    assert!(
        matches!(r, Err(CoreError::Forbidden(Some(DenyReason::SelfApproval)))),
        "{r:?}"
    );
    assert!(
        audit_actions(&w.core)
            .await
            .contains(&"workflow.decision_denied".to_owned())
    );
    let other = as_user(&w.core, second).await;
    workflow::decide_step(&w.core, &other, step, StepDecision::Approve, "")
        .await
        .unwrap();

    // Manual status changes cannot bypass the workflow.
    let r = documents::change_status(&w.core, &approver, doc, DocumentStatus::Active, "").await;
    assert!(
        matches!(&r, Err(CoreError::Validation(m)) if m.contains("approval workflow")),
        "{r:?}"
    );
}

#[tokio::test]
async fn self_approval_can_be_allowed_by_policy() {
    let mut w = world().await;
    w.core.policy.allow_self_approval = true;
    let doc = draft(&w, "policy").await;
    acl::grant(
        &w.core.db,
        &system(),
        Subject::User(w.approver),
        Scope::Department(w.dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    let approver = as_user(&w.core, w.approver).await;
    workflow::submit(
        &w.core,
        &approver,
        doc,
        Submission {
            approvers: vec![Assignee::User(w.approver)],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let step = pending_step(&w, doc, StepKind::Approval).await;
    workflow::decide_step(&w.core, &approver, step, StepDecision::Approve, "")
        .await
        .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::Active);
}

#[tokio::test]
async fn rejection_and_change_requests_return_the_document() {
    let w = world().await;
    let doc = draft(&w, "policy").await;
    let owner = as_user(&w.core, w.owner).await;
    let head = directory::role_id_by_key(&w.core.db, "department_head")
        .await
        .unwrap();
    let sub = Submission {
        reviewers: vec![Assignee::User(w.reviewer)],
        approvers: vec![Assignee::Role(head)],
        ..Default::default()
    };
    workflow::submit(&w.core, &owner, doc, sub.clone())
        .await
        .unwrap();
    assert!(
        workflow::submit(&w.core, &owner, doc, sub.clone())
            .await
            .is_err(),
        "one round at a time"
    );
    let reviewer = as_user(&w.core, w.reviewer).await;
    let step = pending_step(&w, doc, StepKind::Review).await;
    let r =
        workflow::decide_step(&w.core, &reviewer, step, StepDecision::RequestChanges, " ").await;
    assert!(
        matches!(r, Err(CoreError::Validation(_))),
        "a comment is required"
    );
    workflow::decide_step(
        &w.core,
        &reviewer,
        step,
        StepDecision::RequestChanges,
        "Section 4 cites the old policy",
    )
    .await
    .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::Draft);
    let p = workflow::panel(&w.core, &owner, doc).await.unwrap();
    assert_eq!(p.workflows[0].status, WorkflowStatus::ChangesRequested);
    assert_eq!(p.workflows[0].steps[1].status, StepStatus::Skipped);
    assert!(p.can_submit);
    let notice = &inbox::list(&w.core, &owner, false).await.unwrap()[0];
    assert!(notice.headline.starts_with("Changes requested: POL-"));
    assert!(notice.href.ends_with("#workflow"));

    // Second round: approved by any department head, after a new file is
    // added the approval is refused until resubmitted.
    workflow::submit(&w.core, &owner, doc, sub.clone())
        .await
        .unwrap();
    let step = pending_step(&w, doc, StepKind::Review).await;
    workflow::decide_step(&w.core, &reviewer, step, StepDecision::Approve, "")
        .await
        .unwrap();
    let file = upload(&w.core, "v2.pdf", &pdf("v2")).await;
    documents::add_version(
        &w.core,
        &owner,
        doc,
        file,
        VersionKind::Revision,
        "fix",
        None,
    )
    .await
    .unwrap();
    let approver = as_user(&w.core, w.approver).await;
    let approval = pending_step(&w, doc, StepKind::Approval).await;
    let r = workflow::decide_step(&w.core, &approver, approval, StepDecision::Approve, "").await;
    assert!(matches!(r, Err(CoreError::Conflict(_))), "{r:?}");
    // Rejection needs no permission beyond the step, and ends the round.
    workflow::decide_step(
        &w.core,
        &approver,
        approval,
        StepDecision::Reject,
        "Not needed",
    )
    .await
    .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::Draft);

    // Third round, cancelled by the submitter.
    let id = workflow::submit(&w.core, &owner, doc, sub).await.unwrap();
    let outsider = as_user(&w.core, w.outsider).await;
    assert!(workflow::cancel(&w.core, &outsider, id, "").await.is_err());
    workflow::cancel(&w.core, &owner, id, "Wrong version")
        .await
        .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::Draft);
    let p = workflow::panel(&w.core, &owner, doc).await.unwrap();
    let statuses: Vec<WorkflowStatus> = p.workflows.iter().map(|w| w.status).collect();
    assert_eq!(
        statuses,
        vec![
            WorkflowStatus::Cancelled,
            WorkflowStatus::Rejected,
            WorkflowStatus::ChangesRequested
        ],
        "every round is kept"
    );
    // The outsider cannot see the workflow at all.
    assert!(workflow::panel(&w.core, &outsider, doc).await.is_err());
}

#[tokio::test]
async fn revising_an_active_document_keeps_it_active_on_rejection() {
    let w = world().await;
    let doc = draft(&w, "policy").await;
    documents::change_status(&w.core, &system(), doc, DocumentStatus::Active, "")
        .await
        .unwrap();
    let owner = as_user(&w.core, w.owner).await;
    workflow::submit(
        &w.core,
        &owner,
        doc,
        Submission {
            approvers: vec![Assignee::User(w.approver)],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::InReview);
    let step = pending_step(&w, doc, StepKind::Approval).await;
    workflow::decide_step(
        &w.core,
        &as_user(&w.core, w.approver).await,
        step,
        StepDecision::Reject,
        "Keep the current version",
    )
    .await
    .unwrap();
    assert_eq!(status_of(&w.core, doc).await, DocumentStatus::Active);
}

#[tokio::test]
async fn signatures_are_requested_recorded_and_stored() {
    let w = world().await;
    let doc = draft(&w, "partnership_agreement").await;
    let owner = as_user(&w.core, w.owner).await;
    let chair = signatures::add(
        &w.core,
        &owner,
        doc,
        NewSignatory {
            name: "Pat Chair".into(),
            title: "Board Chair".into(),
            user_id: Some(w.approver),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let partner = signatures::add(
        &w.core,
        &owner,
        doc,
        NewSignatory {
            name: "Sam Director".into(),
            title: "Managing Director".into(),
            organization: "Example Partner Inc.".into(),
            email: "director@partner.example".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let typo = signatures::add(
        &w.core,
        &owner,
        doc,
        NewSignatory {
            name: "Wrong person".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    signatures::remove(&w.core, &owner, typo).await.unwrap();
    // Viewers cannot manage signatures.
    let reviewer = as_user(&w.core, w.reviewer).await;
    assert!(
        signatures::add(
            &w.core,
            &reviewer,
            doc,
            NewSignatory {
                name: "X".into(),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );

    signatures::request(&w.core, &owner, chair).await.unwrap();
    signatures::request(&w.core, &owner, partner).await.unwrap();
    assert!(
        signatures::remove(&w.core, &owner, partner).await.is_err(),
        "already asked"
    );
    assert!(
        inbox_subjects(&w.core, w.approver)
            .await
            .iter()
            .any(|s| s.starts_with("Signature requested: PAG-"))
    );
    let file = upload(&w.core, "signed.pdf", &pdf("signed")).await;
    signatures::record_signed(
        &w.core,
        &owner,
        chair,
        SignedRecord {
            signed_on: None,
            note: "Signed at the meeting".into(),
            file: Some(file),
        },
    )
    .await
    .unwrap();
    signatures::record_declined(&w.core, &owner, partner, "Partner board tabled it")
        .await
        .unwrap();
    let list = signatures::list(&w.core, &reviewer, doc).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].status, SignatureStatus::Signed);
    assert_eq!(list[0].provider, "manual");
    assert!(list[0].signed_on.is_some() && list[0].recorded_at.is_some());
    let v = list[0].signed_version_no.unwrap();
    let versions = documents::versions(&w.core, &owner, doc).await.unwrap();
    let signed = versions.iter().find(|x| x.version_no == v).unwrap();
    assert_eq!(signed.kind, VersionKind::SignedCopy);
    assert_eq!(list[1].status, SignatureStatus::Declined);
    assert!(
        signatures::record_signed(&w.core, &owner, chair, SignedRecord::default())
            .await
            .is_err(),
        "recorded once"
    );
    let audit = audit_actions(&w.core).await;
    for a in [
        "signatory.added",
        "signatory.removed",
        "signature.requested",
        "signature.recorded",
        "signature.declined",
    ] {
        assert!(audit.iter().any(|x| x == a), "missing {a}");
    }
}
