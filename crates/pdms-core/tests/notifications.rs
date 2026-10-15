#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 2: schedules, statuses, reminders, delivery, tasks, calendar, ICS.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use common::*;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::calendar;
use pdms_core::documents::search::DocumentFilter;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, VersionKind, types};
use pdms_core::notifications::mailer::MemoryMailer;
use pdms_core::notifications::rules::{self, Channel, DueEvent, Recipient, RuleScope, RuleSpec};
use pdms_core::notifications::{self, delivery, inbox};
use pdms_core::reviews::{self, Completion, TaskStatus};
use pdms_core::{Core, CoreError, Ctx, acl, directory, schedule};
use uuid::Uuid;

fn today(core: &Core) -> NaiveDate {
    schedule::local_today(Utc::now(), core.config.timezone())
}

/// Mid-morning today in the organization's zone (inside the send window).
fn morning(core: &Core) -> DateTime<Utc> {
    today(core).and_hms_opt(16, 0, 0).unwrap().and_utc()
}

fn date(s: &str) -> Option<NaiveDate> {
    Some(s.parse().unwrap())
}

fn with_memory_mailer(core: &mut Core) -> MemoryMailer {
    let m = MemoryMailer::default();
    core.mailer = Arc::new(m.clone());
    m
}

async fn type_id(core: &Core, key: &str) -> Uuid {
    types::type_by_key(&core.db, key).await.unwrap().id
}

async fn doc(core: &Core, dept: Uuid, owner: Uuid, m: MetadataInput) -> Uuid {
    documents::create(
        core,
        &system(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                department_id: dept,
                ..m
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

async fn status_of(core: &Core, id: Uuid) -> DocumentStatus {
    sqlx::query_scalar::<_, String>("SELECT status::text FROM documents WHERE id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .map(|s| DocumentStatus::parse(&s).unwrap())
        .unwrap()
}

async fn pending_email_deliveries(core: &Core) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM notification_deliveries WHERE channel = 'email' AND status IN ('pending', 'failed', 'sending') ORDER BY id")
        .fetch_all(&core.db)
        .await
        .unwrap()
}

async fn send_all(core: &Core) {
    for id in pending_email_deliveries(core).await {
        delivery::send_email(core, id, "test").await.unwrap();
    }
}

/// Acceptance test 4: a partnership agreement from 2013, amended 2014, on a five-year review
/// cycle and not reviewed since 2014.
#[tokio::test]
async fn ila_overdue_acceptance() {
    let (mut core, _db) = core().await;
    let mail = with_memory_mailer(&mut core);
    let (owner, ops) = user_in_new_department(&core, "OPS", "owner@x.org").await;
    let colleague = directory::create_user(
        &core.db,
        &system(),
        "Colleague",
        "colleague@x.org",
        Some(ops),
    )
    .await
    .unwrap();
    let (outsider, _) = user_in_new_department(&core, "HR", "hr@x.org").await;
    let agreement = type_id(&core, "partnership_agreement").await;

    // Created with the original 2013 file; "next review" left empty to be computed.
    let id = documents::create(
        &core,
        &system(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: "Facilities Maintenance agreement".into(),
                document_type_id: agreement,
                department_id: ops,
                execution_date: date("2013-02-12"),
                effective_date: date("2013-03-01"),
                last_review_date: date("2014-06-01"),
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file: Some(upload(&core, "agreement-2013.pdf", &pdf("2013")).await),
            note: "Original agreement".into(),
        },
    )
    .await
    .unwrap();
    // The 2014 amendment is a new version with its milestone date.
    documents::add_version(
        &core,
        &system(),
        id,
        upload(&core, "amendment-2014.pdf", &pdf("2014")).await,
        VersionKind::Amendment,
        "Amendment 1",
        date("2014-06-01"),
    )
    .await
    .unwrap();
    let view = documents::view(&core, &system(), id).await.unwrap();
    assert_eq!(
        view.document.next_review_date,
        date("2019-06-01"),
        "five years after the last review"
    );

    // Notify the owner and the department.
    rules::create(
        &core,
        &system(),
        RuleScope::Document(id),
        RuleSpec {
            event: DueEvent::Review,
            offsets_days: vec![90, 30, 7, 0],
            overdue_repeat_days: Some(7),
            channels: vec![Channel::Email, Channel::InApp],
            recipients: vec![Recipient::Owner, Recipient::DocumentDepartment],
            is_active: true,
        },
    )
    .await
    .unwrap();

    let report = notifications::plan(&core, morning(&core)).await.unwrap();
    assert_eq!(status_of(&core, id).await, DocumentStatus::Overdue);
    assert_eq!(
        report.notifications, 1,
        "one overdue reminder, not a burst of every missed one"
    );

    // Missed review cycles: 2019-06-01, 2024-06-01, ... up to today.
    let dash = calendar::dashboard(
        &core,
        &as_user(&core, owner).await,
        &DocumentFilter::default(),
        30,
    )
    .await
    .unwrap();
    let item = dash
        .overdue
        .iter()
        .find(|o| o.event.document_id() == Some(id))
        .unwrap();
    let expected = schedule::missed_cycles(date("2019-06-01").unwrap(), Some(60), today(&core));
    assert!(
        expected.len() >= 2
            && expected[0] == date("2019-06-01").unwrap()
            && expected[1] == date("2024-06-01").unwrap()
    );
    assert_eq!(item.missed_cycles, expected);

    // The review task is overdue.
    let tasks = reviews::for_document(&core, &system(), id).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Overdue);
    assert_eq!(tasks[0].due_date, date("2019-06-01").unwrap());

    // Owner and department colleague are notified by email and in the app; the outsider is not.
    send_all(&core).await;
    let mut to: Vec<String> = mail.messages().into_iter().map(|m| m.to_address).collect();
    to.sort();
    assert_eq!(to, vec!["colleague@x.org", "owner@x.org"]);
    assert!(
        mail.messages()[0]
            .subject
            .contains("Review overdue since June 1, 2019")
    );
    for u in [owner, colleague] {
        let items = inbox::list(&core, &as_user(&core, u).await, false)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].headline, "Review overdue since June 1, 2019");
    }
    assert!(
        inbox::list(&core, &as_user(&core, outsider).await, false)
            .await
            .unwrap()
            .is_empty()
    );

    // Planning again creates and sends nothing new.
    let again = notifications::plan(&core, morning(&core)).await.unwrap();
    assert_eq!(
        (
            again.notifications,
            again.status_changes,
            again.tasks_created
        ),
        (0, 0, 0)
    );
    send_all(&core).await;
    assert_eq!(mail.messages().len(), 2);

    // Completing the review reschedules five years out and clears Overdue.
    let ctx_owner = as_user(&core, owner).await;
    reviews::start(&core, &ctx_owner, tasks[0].id)
        .await
        .unwrap();
    reviews::complete(
        &core,
        &ctx_owner,
        tasks[0].id,
        Completion {
            note: "Reviewed with the partner".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let after = documents::view(&core, &ctx_owner, id)
        .await
        .unwrap()
        .document;
    assert_eq!(after.last_review_date, Some(today(&core)));
    assert_eq!(
        after.next_review_date,
        Some(schedule::add_months(today(&core), 60))
    );
    assert_eq!(after.status, DocumentStatus::Active);

    // History: both files, the review recorded as a version, and the completed review.
    let versions = documents::versions(&core, &ctx_owner, id).await.unwrap();
    assert_eq!(versions.iter().filter(|v| v.file_changed).count(), 2);
    assert!(versions.iter().any(|v| v.note == "Review completed"));
    let history = reviews::for_document(&core, &ctx_owner, id).await.unwrap();
    assert_eq!(history[0].status, TaskStatus::Completed);
    assert_eq!(history[0].outcome_note, "Reviewed with the partner");
    assert!(pdms_audit::verify(&core.db).await.unwrap().is_intact());
}

async fn due_docs(core: &Core, n: usize) -> (Uuid, Vec<Uuid>) {
    let (owner, dept) = user_in_new_department(core, "OPS", "owner@x.org").await;
    let other = type_id(core, "other").await;
    let mut ids = Vec::new();
    for i in 0..n {
        let m = MetadataInput {
            title: format!("Doc {i}"),
            document_type_id: other,
            next_review_date: Some(today(core) + chrono::Duration::days(30)),
            ..Default::default()
        };
        ids.push(doc(core, dept, owner, m).await);
    }
    (owner, ids)
}

/// Acceptance test 5: kill the worker mid-run and restart it. Every email is
/// sent exactly once.
#[tokio::test]
async fn notification_delivery_survives_a_worker_crash() {
    let (mut core, _db) = core().await;
    let mut cfg = (*core.config).clone();
    cfg.worker.lease_seconds = 1;
    cfg.worker.poll_interval_ms = 20;
    core.config = Arc::new(cfg);
    // The first worker's mailer hangs, so it dies with sends in flight.
    let shared = MemoryMailer::default();
    let hanging = MemoryMailer {
        delay: Some(Duration::from_secs(30)),
        ..shared.clone()
    };
    core.mailer = Arc::new(hanging);
    let (_owner, ids) = due_docs(&core, 12).await;

    // Two planners at once must not duplicate anything.
    let (a, b) = tokio::join!(
        notifications::plan(&core, morning(&core)),
        notifications::plan(&core, morning(&core))
    );
    assert_eq!(
        a.unwrap().notifications + b.unwrap().notifications,
        ids.len()
    );
    let expected = pending_email_deliveries(&core).await.len();
    assert_eq!(expected, ids.len());

    let (tx1, rx1) = tokio::sync::watch::channel(false);
    let w1 = tokio::spawn(pdms_core::maintenance::worker(&core).run(rx1));
    tokio::time::sleep(Duration::from_millis(800)).await;
    w1.abort(); // crash: leases are left behind, some deliveries are 'sending'
    let _ = w1.await;
    drop(tx1);
    assert!(
        shared.messages().is_empty(),
        "nothing completed before the crash"
    );

    // Restart with a working mailer (sharing the record of sent mail).
    core.mailer = Arc::new(shared.clone());
    tokio::time::sleep(Duration::from_millis(1100)).await; // leases expire
    let (tx2, rx2) = tokio::sync::watch::channel(false);
    let w2 = tokio::spawn(pdms_core::maintenance::worker(&core).run(rx2));
    for _ in 0..200 {
        let sent: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_deliveries WHERE channel = 'email' AND status = 'sent'")
            .fetch_one(&core.db)
            .await
            .unwrap();
        if sent as usize == expected {
            break;
        }
        // Retried jobs back off; pull them forward so the test is quick.
        sqlx::query("UPDATE jobs SET run_at = now() WHERE status = 'pending'")
            .execute(&core.db)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tx2.send(true).unwrap();
    w2.await.unwrap();

    let msgs = shared.messages();
    let mut ids_sent: Vec<String> = msgs.iter().map(|m| m.message_id.clone()).collect();
    ids_sent.sort();
    ids_sent.dedup();
    assert_eq!(msgs.len(), expected, "no duplicates and none missed");
    assert_eq!(ids_sent.len(), expected);
    // Attempts were recorded, including the interrupted ones.
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_delivery_attempts")
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert!(attempts as usize >= expected);
}

#[tokio::test]
async fn failed_sends_are_retried_and_recorded() {
    let (mut core, _db) = core().await;
    let mail = with_memory_mailer(&mut core);
    *mail.fail_next.lock().unwrap() = 1;
    due_docs(&core, 1).await;
    notifications::plan(&core, morning(&core)).await.unwrap();
    let id = pending_email_deliveries(&core).await[0];
    assert!(delivery::send_email(&core, id, "w").await.is_err());
    let (status, err): (String, Option<String>) = sqlx::query_as(
        "SELECT status::text, last_error FROM notification_deliveries WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(status, "failed");
    assert!(err.unwrap().contains("simulated failure"));
    delivery::send_email(&core, id, "w").await.unwrap();
    delivery::send_email(&core, id, "w").await.unwrap(); // already sent: no-op
    assert_eq!(mail.messages().len(), 1);
    let outcomes: Vec<String> = sqlx::query_scalar("SELECT outcome FROM notification_delivery_attempts WHERE delivery_id = $1 ORDER BY attempt_no")
        .bind(id)
        .fetch_all(&core.db)
        .await
        .unwrap();
    assert_eq!(outcomes, vec!["failed", "sent"]);
}

/// Acceptance test 6: no expiration date means Active forever and no expiration alerts.
#[tokio::test]
async fn documents_without_expiration_never_expire_or_alert() {
    let (mut core, _db) = core().await;
    with_memory_mailer(&mut core);
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let policy = type_id(&core, "policy").await;
    let forever = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "Forever".into(),
            document_type_id: policy,
            ..Default::default()
        },
    )
    .await;
    let soon = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "Soon".into(),
            document_type_id: policy,
            expiration_date: Some(today(&core) + chrono::Duration::days(7)),
            ..Default::default()
        },
    )
    .await;
    notifications::plan(&core, morning(&core)).await.unwrap();
    let events: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT document_id, event::text FROM notifications")
            .fetch_all(&core.db)
            .await
            .unwrap();
    assert_eq!(events, vec![(soon, "expiration".to_owned())]);
    assert_eq!(status_of(&core, forever).await, DocumentStatus::Active);
    let cal = calendar::events(
        &core,
        &system(),
        &DocumentFilter::default(),
        NaiveDate::MIN,
        NaiveDate::MAX,
        true,
    )
    .await
    .unwrap();
    assert!(cal.iter().all(|e| e.document_id() != Some(forever)));
}

#[tokio::test]
async fn date_driven_statuses_move_both_ways() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let contract = type_id(&core, "contract").await;
    let t = today(&core);
    let id = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "Contract".into(),
            document_type_id: contract,
            renewal_date: Some(t + chrono::Duration::days(45)),
            expiration_date: Some(t - chrono::Duration::days(1)),
            ..Default::default()
        },
    )
    .await;
    notifications::plan(&core, morning(&core)).await.unwrap();
    assert_eq!(status_of(&core, id).await, DocumentStatus::Expired);
    // Extending the expiration (an edit) brings it back to Renewal Due on the next run.
    sqlx::query("UPDATE documents SET expiration_date = $2 WHERE id = $1")
        .bind(id)
        .bind(t + chrono::Duration::days(400))
        .execute(&core.db)
        .await
        .unwrap();
    notifications::plan(&core, morning(&core)).await.unwrap();
    assert_eq!(status_of(&core, id).await, DocumentStatus::RenewalDue);
    let changes: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT changes FROM audit_log WHERE action = 'document.status_changed' ORDER BY seq",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1][0]["after"], "renewal_due");
}

#[tokio::test]
async fn reminders_wait_for_the_send_hour_and_skip_people_without_access() {
    let (mut core, _db) = core().await;
    with_memory_mailer(&mut core);
    let (owner, dept) = user_in_new_department(&core, "HR", "o@x.org").await;
    let (outsider, _) = user_in_new_department(&core, "OPS", "out@x.org").await;
    let other = type_id(&core, "other").await;
    let id = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "Secret".into(),
            document_type_id: other,
            next_review_date: Some(today(&core)),
            ..Default::default()
        },
    )
    .await;
    documents::set_visibility(&core, &system(), id, pdms_authz::Visibility::Restricted)
        .await
        .unwrap();
    rules::create(
        &core,
        &system(),
        RuleScope::Document(id),
        RuleSpec {
            event: DueEvent::Review,
            offsets_days: vec![0],
            overdue_repeat_days: None,
            channels: vec![Channel::InApp],
            recipients: vec![Recipient::Owner, Recipient::User(outsider)],
            is_active: true,
        },
    )
    .await
    .unwrap();
    let early = today(&core).and_hms_opt(9, 0, 0).unwrap().and_utc(); // 04:00–05:00 Eastern
    assert_eq!(
        notifications::plan(&core, early)
            .await
            .unwrap()
            .notifications,
        0
    );
    notifications::plan(&core, morning(&core)).await.unwrap();
    let rows: Vec<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT user_id, status::text, skip_reason FROM notification_deliveries")
            .fetch_all(&core.db)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    for (u, status, reason) in rows {
        if u == owner {
            assert_eq!(status, "sent");
        } else {
            assert_eq!(status, "skipped");
            assert!(reason.unwrap().contains("cannot view"));
        }
    }
}

#[tokio::test]
async fn rule_management_is_authorized_and_audited() {
    let (core, _db) = core().await;
    let (dept, editor) = {
        let (u, d) = user_in_new_department(&core, "OPS", "e@x.org").await;
        acl::grant(
            &core.db,
            &system(),
            Subject::User(u),
            Scope::Department(d),
            Permission::Edit,
            None,
            "t",
        )
        .await
        .unwrap();
        (d, u)
    };
    let viewer = directory::create_user(&core.db, &system(), "V", "v@x.org", Some(dept))
        .await
        .unwrap();
    let other = type_id(&core, "other").await;
    let id = doc(
        &core,
        dept,
        editor,
        MetadataInput {
            title: "D".into(),
            document_type_id: other,
            ..Default::default()
        },
    )
    .await;
    let spec = RuleSpec {
        event: DueEvent::Review,
        offsets_days: vec![30, 0],
        overdue_repeat_days: Some(14),
        channels: vec![Channel::Email],
        recipients: vec![Recipient::Owner],
        is_active: true,
    };
    assert!(matches!(
        rules::create(
            &core,
            &as_user(&core, viewer).await,
            RuleScope::Document(id),
            spec.clone()
        )
        .await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        rules::create(
            &core,
            &as_user(&core, editor).await,
            RuleScope::Organization,
            spec.clone()
        )
        .await,
        Err(CoreError::Forbidden(_))
    ));
    let rid = rules::create(
        &core,
        &as_user(&core, editor).await,
        RuleScope::Document(id),
        spec.clone(),
    )
    .await
    .unwrap();
    let all = rules::load_all(&core.db).await.unwrap();
    let (eff, source) = rules::effective(&all, id, other, DueEvent::Review);
    assert_eq!((eff.len(), source), (1, rules::RuleSource::Document));
    assert_eq!(
        eff[0].summary(),
        "30 days before, on the due date, then every 14 days while overdue"
    );
    rules::delete(&core, &as_user(&core, editor).await, rid)
        .await
        .unwrap();
    let all = rules::load_all(&core.db).await.unwrap();
    assert_eq!(
        rules::effective(&all, id, other, DueEvent::Review).1,
        rules::RuleSource::Organization
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE 'notification_rule.%' AND document_id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn review_approval_enforces_separation_of_duties() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let approver = directory::create_user(&core.db, &system(), "A", "a@x.org", Some(dept))
        .await
        .unwrap();
    for (u, p) in [
        (owner, Permission::Edit),
        (owner, Permission::Approve),
        (approver, Permission::Approve),
    ] {
        acl::grant(
            &core.db,
            &system(),
            Subject::User(u),
            Scope::Department(dept),
            p,
            None,
            "t",
        )
        .await
        .unwrap();
    }
    let policy = type_id(&core, "policy").await;
    let id = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "P".into(),
            document_type_id: policy,
            next_review_date: Some(today(&core)),
            ..Default::default()
        },
    )
    .await;
    notifications::plan(&core, morning(&core)).await.unwrap();
    let task = reviews::for_document(&core, &system(), id)
        .await
        .unwrap()
        .remove(0);
    let ctx = as_user(&core, owner).await;
    reviews::complete(&core, &ctx, task.id, Completion::default())
        .await
        .unwrap();
    assert!(
        matches!(
            reviews::approve(&core, &ctx, task.id).await,
            Err(CoreError::Forbidden(_))
        ),
        "own review"
    );
    reviews::approve(&core, &as_user(&core, approver).await, task.id)
        .await
        .unwrap();
    let t = reviews::for_document(&core, &system(), id).await.unwrap();
    assert_eq!(t[0].status, TaskStatus::Approved);
    // Policy: annual review, so the next one is a year out.
    let next = documents::view(&core, &system(), id)
        .await
        .unwrap()
        .document
        .next_review_date;
    assert_eq!(next, Some(schedule::add_months(today(&core), 12)));
}

#[tokio::test]
async fn ics_feeds_follow_permissions_and_revocation() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let (other_user, other_dept) = user_in_new_department(&core, "HR", "h@x.org").await;
    let policy = type_id(&core, "policy").await;
    let mine = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "Visible policy".into(),
            document_type_id: policy,
            next_review_date: Some(today(&core) + chrono::Duration::days(10)),
            ..Default::default()
        },
    )
    .await;
    let secret = doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "Secret policy".into(),
            document_type_id: policy,
            next_review_date: Some(today(&core) + chrono::Duration::days(10)),
            ..Default::default()
        },
    )
    .await;
    documents::set_visibility(&core, &system(), secret, pdms_authz::Visibility::Restricted)
        .await
        .unwrap();
    doc(
        &core,
        other_dept,
        other_user,
        MetadataInput {
            title: "HR only".into(),
            document_type_id: policy,
            next_review_date: Some(today(&core)),
            ..Default::default()
        },
    )
    .await;

    let ctx = as_user(&core, owner).await;
    let (feed, token) = calendar::create_feed(&core, &ctx, "My reviews", None, false)
        .await
        .unwrap();
    let ics = calendar::render_feed(&core, &token, &Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(ics.contains("BEGIN:VCALENDAR") && ics.contains("Visible policy"));
    assert!(ics.contains("RRULE:FREQ=MONTHLY;INTERVAL=12"), "{ics}");
    assert!(
        !ics.contains("Secret policy"),
        "confidential documents never go into feeds"
    );
    assert!(!ics.contains("HR only"), "only documents the owner can see");
    assert!(ics.contains(&mine.to_string()));
    assert!(
        calendar::render_feed(&core, "wrong-token", &Default::default())
            .await
            .unwrap()
            .is_none()
    );

    calendar::revoke_feed(&core, &ctx, feed).await.unwrap();
    assert!(
        calendar::render_feed(&core, &token, &Default::default())
            .await
            .unwrap()
            .is_none()
    );
    let (_, token2) = calendar::create_feed(&core, &ctx, "Again", None, false)
        .await
        .unwrap();
    directory::set_user_status(
        &core.db,
        &system(),
        owner,
        pdms_core::model::UserStatus::Disabled,
    )
    .await
    .unwrap();
    assert!(
        calendar::render_feed(&core, &token2, &Default::default())
            .await
            .unwrap()
            .is_none(),
        "disabled owner"
    );
    let acts = audit_actions(&core).await;
    for a in [
        "calendar_feed.created",
        "calendar_feed.accessed",
        "calendar_feed.revoked",
    ] {
        assert!(acts.contains(&a.to_owned()), "missing {a}");
    }
}

#[tokio::test]
async fn inbox_acknowledgement_is_per_user_and_audited() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let other_user = directory::create_user(&core.db, &system(), "x", "x@x.org", Some(dept))
        .await
        .unwrap();
    let policy = type_id(&core, "policy").await;
    doc(
        &core,
        dept,
        owner,
        MetadataInput {
            title: "P".into(),
            document_type_id: policy,
            next_review_date: Some(today(&core)),
            ..Default::default()
        },
    )
    .await;
    notifications::plan(&core, morning(&core)).await.unwrap();
    let ctx = as_user(&core, owner).await;
    let items = inbox::list(&core, &ctx, false).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(inbox::unread_count(&core.db, owner).await.unwrap(), 1);
    assert!(matches!(
        inbox::acknowledge(
            &core,
            &as_user(&core, other_user).await,
            items[0].delivery_id
        )
        .await,
        Err(CoreError::NotFound)
    ));
    inbox::acknowledge(&core, &ctx, items[0].delivery_id)
        .await
        .unwrap();
    assert_eq!(inbox::unread_count(&core.db, owner).await.unwrap(), 0);
    assert_eq!(inbox::list(&core, &ctx, true).await.unwrap().len(), 1);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"notification.acknowledged".to_owned())
    );
}

#[allow(dead_code)]
fn _ctx(_: Ctx) {}
