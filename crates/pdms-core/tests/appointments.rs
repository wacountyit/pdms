#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 3: positions, appointments, requirement checklists, and appointment
//! reminders on the shared calendar and notification system.

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::*;
use pdms_core::appointments::{
    self, AppointmentEdit, AppointmentStatus, NewAppointment, PositionSpec, Term,
};
use pdms_core::calendar::{self, EventSubject};
use pdms_core::documents::search::DocumentFilter;
use pdms_core::notifications::mailer::MemoryMailer;
use pdms_core::notifications::rules::DueEvent;
use pdms_core::notifications::{self, delivery, inbox};
use pdms_core::{Core, CoreError, directory, schedule};
use uuid::Uuid;

fn today(core: &Core) -> NaiveDate {
    schedule::local_today(Utc::now(), core.config.timezone())
}

fn morning(core: &Core) -> DateTime<Utc> {
    today(core).and_hms_opt(16, 0, 0).unwrap().and_utc()
}

struct World {
    core: Core,
    _db: pdms_testkit::TestDb,
    mail: MemoryMailer,
    dept: Uuid,
    clerk: Uuid,
    manager: Uuid,
    member: Uuid,
    outsider: Uuid,
}

async fn world() -> World {
    let (mut core, db) = core().await;
    let mail = MemoryMailer::default();
    core.mailer = Arc::new(mail.clone());
    let (manager, dept) = user_in_new_department(&core, "BOARDS", "manager@x.org").await;
    let member = directory::create_user(&core.db, &system(), "Member", "member@x.org", Some(dept))
        .await
        .unwrap();
    let clerk = directory::create_user(&core.db, &system(), "Clerk", "clerk@x.org", None)
        .await
        .unwrap();
    let role = directory::role_id_by_key(&core.db, "appointments_clerk")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), clerk, role, None)
        .await
        .unwrap();
    let (outsider, _) = user_in_new_department(&core, "HR", "outsider@x.org").await;
    World {
        core,
        _db: db,
        mail,
        dept,
        clerk,
        manager,
        member,
        outsider,
    }
}

async fn position(w: &World, seats: i32) -> Uuid {
    let clerk = as_user(&w.core, w.clerk).await;
    appointments::create_position(
        &w.core,
        &clerk,
        PositionSpec {
            name: "Seat 3".into(),
            body_name: "Advisory Committee".into(),
            department_id: Some(w.dept),
            appointing_authority: "Board of Directors".into(),
            term_months: Some(24),
            seats,
            manager_user_id: Some(w.manager),
            reappointment_lead_days: 90,
            description: String::new(),
            is_active: true,
        },
    )
    .await
    .unwrap()
}

fn person(name: &str, start: NaiveDate) -> NewAppointment {
    NewAppointment {
        person_name: name.into(),
        start_date: start,
        appointment_reference: "Resolution 2025-14".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn appointments_keep_history_and_checklists() {
    let w = world().await;
    let clerk = as_user(&w.core, w.clerk).await;
    let manager = as_user(&w.core, w.manager).await;
    let pos = position(&w, 1).await;
    let letter =
        appointments::add_requirement(&w.core, &clerk, pos, "Acceptance letter on file", "")
            .await
            .unwrap();
    let approval = appointments::add_requirement(
        &w.core,
        &clerk,
        pos,
        "Appointed by the Board of Directors",
        "Minutes reference required",
    )
    .await
    .unwrap();
    // Only appointment managers change the setup.
    assert!(matches!(
        appointments::add_requirement(&w.core, &manager, pos, "X", "").await,
        Err(CoreError::Forbidden(_))
    ));

    let start: NaiveDate = "2025-01-01".parse().unwrap();
    let jane = appointments::appoint(&w.core, &clerk, pos, person("Jane Doe", start))
        .await
        .unwrap();
    let (_, a) = appointments::appointment(&w.core, &clerk, jane)
        .await
        .unwrap();
    assert_eq!(
        a.end_date,
        Some("2026-12-31".parse().unwrap()),
        "two-year term"
    );
    assert_eq!(
        a.reappointment_due_date,
        Some("2026-10-02".parse().unwrap())
    );
    assert_eq!(
        a.appointed_by, "Board of Directors",
        "defaults to the position's authority"
    );
    assert_eq!(a.requirements.len(), 2);
    assert!(a.requirements.iter().all(|r| !r.satisfied));

    // One seat: a second appointment is refused.
    let r = appointments::appoint(&w.core, &clerk, pos, person("John Roe", start)).await;
    assert!(matches!(&r, Err(CoreError::Validation(m)) if m.contains("every seat is filled")));

    // The position's manager records checklist items; who and when are kept.
    appointments::set_requirement(
        &w.core,
        &manager,
        jane,
        letter,
        true,
        "Filed with the corporate secretary",
    )
    .await
    .unwrap();
    let member = as_user(&w.core, w.member).await;
    assert!(matches!(
        appointments::set_requirement(&w.core, &member, jane, approval, true, "").await,
        Err(CoreError::Forbidden(_))
    ));
    let (_, a) = appointments::appointment(&w.core, &member, jane)
        .await
        .unwrap();
    let o = a
        .requirements
        .iter()
        .find(|r| r.requirement_id == letter)
        .unwrap();
    assert!(o.satisfied);
    assert_eq!(o.satisfied_by.as_deref(), Some("manager@x.org"));
    assert!(o.satisfied_at.is_some());
    assert_eq!(o.note, "Filed with the corporate secretary");
    assert_eq!(a.requirements_met(), 1);

    // A requirement added later applies to the current appointee too.
    let bond = appointments::add_requirement(&w.core, &clerk, pos, "Bond posted", "")
        .await
        .unwrap();
    let (_, a) = appointments::appointment(&w.core, &clerk, jane)
        .await
        .unwrap();
    assert!(
        a.requirements
            .iter()
            .any(|r| r.requirement_id == bond && !r.satisfied)
    );
    appointments::update_requirement(&w.core, &clerk, bond, "Bond posted", "", false)
        .await
        .unwrap();
    let (_, a) = appointments::appointment(&w.core, &clerk, jane)
        .await
        .unwrap();
    assert!(
        !a.requirements.iter().any(|r| r.requirement_id == bond),
        "retired and never satisfied"
    );

    // Reappoint: a new term for the same person; the checklist starts over.
    let jane2 = appointments::reappoint(
        &w.core,
        &clerk,
        jane,
        Term {
            appointment_reference: "Resolution 2026-90".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_, a2) = appointments::appointment(&w.core, &clerk, jane2)
        .await
        .unwrap();
    assert_eq!(a2.start_date, "2027-01-01".parse().unwrap());
    assert_eq!(a2.end_date, Some("2028-12-31".parse().unwrap()));
    assert_eq!(a2.predecessor_id, Some(jane));
    assert_eq!(a2.requirements_met(), 0);

    // Replace with someone new, then the new appointee resigns.
    let john = appointments::replace(
        &w.core,
        &clerk,
        jane2,
        person("John Roe", "2027-06-01".parse().unwrap()),
    )
    .await
    .unwrap();
    assert!(
        appointments::reappoint(&w.core, &clerk, jane2, Term::default())
            .await
            .is_err(),
        "already ended"
    );
    appointments::end(
        &w.core,
        &clerk,
        john,
        "2027-09-30".parse().unwrap(),
        "Resigned",
    )
    .await
    .unwrap();

    let detail = appointments::position(&w.core, &manager, pos)
        .await
        .unwrap();
    assert!(!detail.can_manage && detail.can_track);
    assert_eq!(detail.position.filled, 0);
    assert_eq!(detail.position.vacancies(), 1);
    let history: Vec<(String, AppointmentStatus, Option<NaiveDate>, String)> = detail
        .appointments
        .iter()
        .map(|a| {
            (
                a.person_name.clone(),
                a.status,
                a.ended_on,
                a.end_reason.clone(),
            )
        })
        .collect();
    assert_eq!(
        history,
        vec![
            (
                "John Roe".into(),
                AppointmentStatus::Ended,
                Some("2027-09-30".parse().unwrap()),
                "Resigned".into()
            ),
            (
                "Jane Doe".into(),
                AppointmentStatus::Ended,
                Some("2027-05-31".parse().unwrap()),
                "replaced".into()
            ),
            (
                "Jane Doe".into(),
                AppointmentStatus::Ended,
                Some("2026-12-31".parse().unwrap()),
                "reappointed".into()
            ),
        ],
        "the full history of appointees is kept"
    );
    // The first term keeps its satisfied checklist item.
    assert!(
        detail.appointments[2]
            .requirements
            .iter()
            .any(|r| r.satisfied)
    );

    // Corrections are audited.
    appointments::update_appointment(
        &w.core,
        &clerk,
        john,
        AppointmentEdit {
            person_name: "John Q. Roe".into(),
            start_date: "2027-06-01".parse().unwrap(),
            end_date: Some("2029-05-31".parse().unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let audit = audit_actions(&w.core).await;
    for a in [
        "position.created",
        "position.requirement_added",
        "position.requirement_updated",
        "appointment.created",
        "appointment.requirement_recorded",
        "appointment.ended",
        "appointment.reappointed",
        "appointment.replaced",
        "appointment.updated",
        "access.denied",
    ] {
        assert!(audit.iter().any(|x| x == a), "missing {a}");
    }
}

#[tokio::test]
async fn positions_are_visible_only_to_the_right_people() {
    let w = world().await;
    let pos = position(&w, 3).await;
    let outsider = as_user(&w.core, w.outsider).await;
    assert!(matches!(
        appointments::position(&w.core, &outsider, pos).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        appointments::list_positions(&w.core, &outsider, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!appointments::any_visible(&w.core, &outsider).await.unwrap());
    let member = as_user(&w.core, w.member).await;
    assert_eq!(
        appointments::list_positions(&w.core, &member, false)
            .await
            .unwrap()
            .len(),
        1
    );
    // Auditors read everything but change nothing.
    let auditor = directory::create_user(&w.core.db, &system(), "Auditor", "aud@x.org", None)
        .await
        .unwrap();
    let role = directory::role_id_by_key(&w.core.db, "auditor")
        .await
        .unwrap();
    directory::assign_role(&w.core.db, &system(), auditor, role, None)
        .await
        .unwrap();
    let auditor = as_user(&w.core, auditor).await;
    let d = appointments::position(&w.core, &auditor, pos)
        .await
        .unwrap();
    assert!(!d.can_manage && !d.can_track);
    assert!(
        appointments::appoint(&w.core, &auditor, pos, person("X", today(&w.core)))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn term_ends_and_reappointments_are_reminded_and_on_the_calendar() {
    let w = world().await;
    let clerk = as_user(&w.core, w.clerk).await;
    let pos = position(&w, 2).await;
    let t = today(&w.core);
    // Term ends in 30 days; the reappointment decision was due a week ago.
    let jane = appointments::appoint(
        &w.core,
        &clerk,
        pos,
        NewAppointment {
            person_name: "Jane Doe".into(),
            start_date: t - Duration::days(700),
            end_date: Some(t + Duration::days(30)),
            reappointment_due_date: Some(t - Duration::days(7)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // A holdover: the term ended and no successor has been recorded.
    appointments::appoint(
        &w.core,
        &clerk,
        pos,
        NewAppointment {
            person_name: "Hal Holdover".into(),
            start_date: t - Duration::days(800),
            end_date: Some(t - Duration::days(3)),
            reappointment_due_date: Some(t - Duration::days(60)),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Appointing plans reminders right away (when today's send hour has
    // passed); the scheduled run covers the rest and creates nothing twice.
    notifications::plan(&w.core, morning(&w.core))
        .await
        .unwrap();
    let rows: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT event::text, appointment_id FROM notifications WHERE appointment_id IS NOT NULL ORDER BY event, due_date",
    )
    .fetch_all(&w.core.db)
    .await
    .unwrap();
    let events: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(
        events.contains(&"term_end") && events.contains(&"reappointment"),
        "{events:?}"
    );
    // Idempotent.
    let again = notifications::plan(&w.core, morning(&w.core))
        .await
        .unwrap();
    assert_eq!(again.notifications, 0);

    // The position's manager and the appointments clerk are told; the
    // outsider and plain department members are not (by default rules).
    let manager = as_user(&w.core, w.manager).await;
    let heads: Vec<String> = inbox::list(&w.core, &manager, false)
        .await
        .unwrap()
        .into_iter()
        .map(|i| format!("{} | {} | {}", i.headline, i.title, i.href))
        .collect();
    assert!(
        heads
            .iter()
            .any(|h| h.starts_with("Term ends in 30 days | Jane Doe — Seat 3")),
        "{heads:?}"
    );
    assert!(
        heads
            .iter()
            .any(|h| h.starts_with("Reappointment decision overdue by 7 days")),
        "{heads:?}"
    );
    assert!(
        heads
            .iter()
            .all(|h| h.ends_with(&format!("/positions/{pos}")))
    );
    assert!(
        !inbox::list(&w.core, &clerk, false)
            .await
            .unwrap()
            .is_empty()
    );
    let outsider = as_user(&w.core, w.outsider).await;
    assert!(
        inbox::list(&w.core, &outsider, false)
            .await
            .unwrap()
            .is_empty()
    );

    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT dl.id FROM notification_deliveries dl JOIN notifications n ON n.id = dl.notification_id
          WHERE dl.channel = 'email' AND dl.status = 'pending' AND n.appointment_id IS NOT NULL",
    )
    .fetch_all(&w.core.db)
    .await
    .unwrap();
    assert!(!ids.is_empty());
    for id in ids {
        delivery::send_email(&w.core, id, "t").await.unwrap();
    }
    let m = w
        .mail
        .messages()
        .into_iter()
        .find(|m| m.subject.contains("Term ends in 30 days"))
        .unwrap();
    assert!(m.subject.contains("Jane Doe (Seat 3, Advisory Committee)"));
    assert!(m.text.contains(&format!("/positions/{pos}")));
    let overdue = w
        .mail
        .messages()
        .into_iter()
        .find(|m| m.subject.contains("Reappointment decision overdue"))
        .unwrap();
    assert!(overdue.text.contains("a replacement must be selected"));

    // Same calendar and dashboard as documents.
    let filter = DocumentFilter::default();
    let ev = calendar::events(&w.core, &manager, &filter, t, t + Duration::days(60), false)
        .await
        .unwrap();
    let e = ev.iter().find(|e| e.kind == DueEvent::TermEnd).unwrap();
    assert_eq!(e.title, "Jane Doe — Seat 3");
    assert_eq!(e.href(), format!("/positions/{pos}"));
    assert!(
        matches!(e.subject, EventSubject::Appointment { appointment_id, .. } if appointment_id == jane)
    );
    assert!(
        calendar::events(
            &w.core,
            &outsider,
            &filter,
            t,
            t + Duration::days(60),
            false
        )
        .await
        .unwrap()
        .is_empty()
    );
    // Document-only filters leave appointments out.
    let typed = DocumentFilter {
        document_type_id: Some(Uuid::now_v7()),
        ..Default::default()
    };
    assert!(
        calendar::events(&w.core, &manager, &typed, t, t + Duration::days(60), false)
            .await
            .unwrap()
            .is_empty()
    );
    let dash = calendar::dashboard(&w.core, &manager, &filter, 60)
        .await
        .unwrap();
    assert!(dash.upcoming.iter().any(|e| e.kind == DueEvent::TermEnd));
    let overdue_titles: Vec<(DueEvent, String)> = dash
        .overdue
        .iter()
        .map(|o| (o.event.kind, o.event.title.clone()))
        .collect();
    assert!(overdue_titles.contains(&(DueEvent::Reappointment, "Jane Doe — Seat 3".into())));
    assert!(
        overdue_titles.contains(&(DueEvent::TermEnd, "Hal Holdover — Seat 3".into())),
        "holdovers show as overdue: {overdue_titles:?}"
    );

    // Recording the reappointment stops further reminders for the old term
    // and schedules the new one.
    appointments::reappoint(
        &w.core,
        &clerk,
        jane,
        Term {
            start_date: Some(t + Duration::days(31)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM appointments WHERE position_id = $1 AND status = 'active'",
    )
    .bind(pos)
    .fetch_one(&w.core.db)
    .await
    .unwrap();
    assert_eq!(active, 2);
}

#[tokio::test]
async fn calendar_feeds_include_appointments() {
    let w = world().await;
    let clerk = as_user(&w.core, w.clerk).await;
    let pos = position(&w, 1).await;
    let t = today(&w.core);
    appointments::appoint(
        &w.core,
        &clerk,
        pos,
        NewAppointment {
            person_name: "Jane Doe".into(),
            start_date: t - Duration::days(100),
            end_date: Some(t + Duration::days(200)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let manager = as_user(&w.core, w.manager).await;
    let (_, token) = calendar::create_feed(&w.core, &manager, "Mine", None, false)
        .await
        .unwrap();
    let ics = calendar::render_feed(&w.core, &token, &Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(ics.contains("Term ends: Jane Doe (Seat 3"), "{ics}");
    assert!(ics.contains("Reappointment due: Jane Doe"));
    let outsider = as_user(&w.core, w.outsider).await;
    let (_, token) = calendar::create_feed(&w.core, &outsider, "Mine", None, false)
        .await
        .unwrap();
    let ics = calendar::render_feed(&w.core, &token, &Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(!ics.contains("Jane Doe"));
}
