// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Seats needing attention, for the dashboard and the positions page.

mod common;

use chrono::{Duration, Utc};
use common::*;
use pdms_core::appointments::{self, NewAppointment, PositionSpec, SeatState};
use pdms_core::{directory, schedule};

#[tokio::test]
async fn attention_lists_worst_first_and_respects_visibility() {
    let (core, _db) = core().await;
    let (_, dept) = user_in_new_department(&core, "BOARDS", "m@x.org").await;
    let clerk_id = directory::create_user(&core.db, &system(), "Clerk", "clerk@x.org", None)
        .await
        .unwrap();
    let role = directory::role_id_by_key(&core.db, "appointments_clerk")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), clerk_id, role, None)
        .await
        .unwrap();
    let clerk = as_user(&core, clerk_id).await;
    let today = schedule::local_today(Utc::now(), core.config.timezone());

    let make = async |name: &str, seats: i32| {
        appointments::create_position(
            &core,
            &clerk,
            PositionSpec {
                name: name.into(),
                body_name: "Advisory Committee".into(),
                department_id: Some(dept),
                appointing_authority: "Board".into(),
                term_months: Some(24),
                seats,
                manager_user_id: None,
                reappointment_lead_days: 90,
                description: String::new(),
                is_active: true,
            },
        )
        .await
        .unwrap()
    };
    let holdover = make("Seat 1", 2).await;
    let decision = make("Seat 2", 1).await;
    let ending = make("Seat 3", 1).await;

    let appoint = |pos, end, due| {
        (
            pos,
            NewAppointment {
                person_name: "Jane Doe".into(),
                start_date: today - Duration::days(700),
                end_date: Some(end),
                reappointment_due_date: due,
                ..Default::default()
            },
        )
    };
    for (pos, a) in [
        appoint(
            holdover,
            today - Duration::days(10),
            Some(today + Duration::days(1)),
        ),
        appoint(
            decision,
            today + Duration::days(200),
            Some(today - Duration::days(5)),
        ),
        appoint(
            ending,
            today + Duration::days(30),
            Some(today + Duration::days(5)),
        ),
    ] {
        appointments::appoint(&core, &clerk, pos, a).await.unwrap();
    }

    let rows = appointments::attention(&core, &clerk, today, 60)
        .await
        .unwrap();
    let got: Vec<_> = rows.iter().map(|r| (r.label.as_str(), r.state)).collect();
    assert_eq!(
        got,
        [
            ("Seat 2, Advisory Committee", SeatState::DecisionOverdue),
            ("Seat 1, Advisory Committee", SeatState::Holdover),
            ("Seat 1, Advisory Committee", SeatState::Vacant),
            (
                "Seat 3, Advisory Committee",
                SeatState::TermEnding(today + Duration::days(30))
            ),
        ]
    );

    let (outsider, _) = user_in_new_department(&core, "HR", "o@x.org").await;
    let outsider = as_user(&core, outsider).await;
    assert!(
        appointments::attention(&core, &outsider, today, 60)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn attention_boundaries_and_one_classification() {
    let (core, _db) = core().await;
    let (_, dept) = user_in_new_department(&core, "BOARDS", "m@x.org").await;
    let clerk_id = directory::create_user(&core.db, &system(), "Clerk", "clerk@x.org", None)
        .await
        .unwrap();
    let role = directory::role_id_by_key(&core.db, "appointments_clerk")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), clerk_id, role, None)
        .await
        .unwrap();
    let clerk = as_user(&core, clerk_id).await;
    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let d = |n: i64| today + Duration::days(n);

    // (name, term end, decision date)
    let cases = [
        ("Yesterday", d(300), Some(d(-1))),
        ("Today", d(300), Some(d(0))),
        ("Horizon", d(60), Some(d(30))),
        ("Both", d(-10), Some(d(-20))),
        ("Beyond", d(61), Some(d(30))),
    ];
    let mut ids = Vec::new();
    for (name, end, due) in cases {
        let pos = appointments::create_position(
            &core,
            &clerk,
            PositionSpec {
                name: name.into(),
                body_name: "Board".into(),
                department_id: Some(dept),
                appointing_authority: "Board".into(),
                term_months: Some(24),
                seats: 1,
                manager_user_id: None,
                reappointment_lead_days: 90,
                description: String::new(),
                is_active: true,
            },
        )
        .await
        .unwrap();
        appointments::appoint(
            &core,
            &clerk,
            pos,
            NewAppointment {
                person_name: "Jane Doe".into(),
                start_date: d(-700),
                end_date: Some(end),
                reappointment_due_date: due,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        ids.push(pos);
    }

    let rows = appointments::attention(&core, &clerk, today, 60)
        .await
        .unwrap();
    let got: Vec<_> = rows.iter().map(|r| (r.label.as_str(), r.state)).collect();
    assert_eq!(
        got,
        [
            ("Both, Board", SeatState::DecisionOverdue),
            ("Yesterday, Board", SeatState::DecisionOverdue),
            ("Today, Board", SeatState::DecisionDueToday),
            ("Horizon, Board", SeatState::TermEnding(d(60))),
        ],
        "each seat once, worst state; the term ending beyond the horizon is absent"
    );

    // The positions page flag comes from the same classifier.
    let today_pos = appointments::position(&core, &clerk, ids[1]).await.unwrap();
    let current: Vec<_> = today_pos.appointments.iter().collect();
    assert_eq!(
        appointments::list_flag(&today_pos.position, &current, today),
        "Decision due"
    );
}
