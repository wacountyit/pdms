#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 2 pages: dashboard, calendar, ICS, inbox, tasks, and rules.

mod support;

use axum::http::StatusCode;
use chrono::{NaiveDate, Utc};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::{directory, notifications, schedule};
use support::*;
use uuid::Uuid;

fn today(app: &App) -> NaiveDate {
    schedule::local_today(Utc::now(), app.core.config.timezone())
}

async fn plan(app: &App) {
    let morning = today(app).and_hms_opt(16, 0, 0).unwrap().and_utc();
    notifications::plan(&app.core, morning).await.unwrap();
}

async fn doc(
    app: &App,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    type_key: &str,
    m: MetadataInput,
) -> Uuid {
    let t = types::type_by_key(&app.core.db, type_key).await.unwrap().id;
    documents::create(
        &app.core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
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

#[tokio::test]
async fn calendar_dashboard_and_ics_respect_permissions() {
    let app = app().await;
    let ops = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let hr = directory::create_department(&app.core.db, &system(), "HR", "Human Resources")
        .await
        .unwrap();
    let (alice, secret) = user(&app.core, "alice@x.org", Some(ops), None).await;
    let (bob, _) = user(&app.core, "bob@x.org", Some(hr), None).await;
    let soon = Some(today(&app) + chrono::Duration::days(10));
    doc(
        &app,
        ops,
        alice,
        "Ops review",
        "policy",
        MetadataInput {
            next_review_date: soon,
            ..Default::default()
        },
    )
    .await;
    doc(
        &app,
        hr,
        bob,
        "HR secret review",
        "policy",
        MetadataInput {
            next_review_date: soon,
            ..Default::default()
        },
    )
    .await;
    doc(
        &app,
        ops,
        alice,
        "Old agreement",
        "partnership_agreement",
        MetadataInput {
            last_review_date: Some("2014-06-01".parse().unwrap()),
            ..Default::default()
        },
    )
    .await;
    plan(&app).await;

    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    for path in ["/", "/calendar", "/calendar?view=agenda&days=30"] {
        let r = b.get(path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert!(
            !r.body.contains("HR secret review"),
            "{path} leaks another department"
        );
    }
    let dash = b.get("/").await;
    assert!(dash.body.contains("Ops review"), "upcoming");
    assert!(dash.body.contains("Old agreement"), "overdue");
    assert!(
        dash.body.contains("Operations") || dash.body.contains("Operations"),
        "your access card"
    );
    let agenda = b.get("/calendar?view=agenda&days=30").await;
    assert!(agenda.body.contains("Ops review"));

    // Subscribe, fetch the ICS anonymously, revoke.
    let csrf = b.get("/calendar/feeds").await.csrf();
    let r = b
        .post(
            "/calendar/feeds",
            &[("_csrf", csrf.as_str()), ("name", "Mine")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = b.get("/calendar/feeds").await;
    let url = regex::Regex::new(r"/ics/([A-Za-z0-9_-]+)\.ics")
        .unwrap()
        .captures(&page.body)
        .unwrap()[0]
        .to_owned();
    let mut anon = Browser::new(&app);
    let ics = anon.get(&url).await;
    assert_eq!(ics.status, StatusCode::OK);
    assert!(
        ics.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/calendar")
    );
    assert!(ics.body.contains("Ops review") && !ics.body.contains("HR secret review"));
    assert!(
        !b.get("/calendar/feeds").await.body.contains(&url),
        "the address is shown only once"
    );
    let feed_id = regex::Regex::new(r"/calendar/feeds/([0-9a-f-]{36})/revoke")
        .unwrap()
        .captures(&page.body)
        .unwrap()[1]
        .to_owned();
    b.post(
        &format!("/calendar/feeds/{feed_id}/revoke"),
        &[("_csrf", csrf.as_str())],
    )
    .await;
    assert_eq!(anon.get(&url).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        anon.get("/ics/not-a-real-token.ics").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn inbox_tasks_and_rules_through_the_web() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "IT", "IT")
        .await
        .unwrap();
    let (owner, secret) = user(&app.core, "owner@x.org", Some(dept), None).await;
    for p in [pdms_authz::Permission::Edit] {
        pdms_core::acl::grant(
            &app.core.db,
            &system(),
            pdms_authz::Subject::User(owner),
            pdms_authz::Scope::Department(dept),
            p,
            None,
            "t",
        )
        .await
        .unwrap();
    }
    let id = doc(
        &app,
        dept,
        owner,
        "Annual policy",
        "policy",
        MetadataInput {
            next_review_date: Some(today(&app)),
            ..Default::default()
        },
    )
    .await;
    plan(&app).await;

    let mut b = Browser::new(&app);
    b.login("owner@x.org", &secret).await;
    let home = b.get("/").await;
    assert!(home.body.contains("class=\"count\""), "unread badge");
    let inbox = b.get("/notifications").await;
    assert!(inbox.body.contains("Review due today"));
    let csrf = inbox.csrf();
    let ack = regex::Regex::new(r"/notifications/([0-9a-f-]{36})/ack")
        .unwrap()
        .captures(&inbox.body)
        .unwrap()[0]
        .to_owned();
    assert_eq!(
        b.post(&ack, &[("_csrf", csrf.as_str())]).await.status,
        StatusCode::SEE_OTHER
    );
    assert!(
        b.get("/notifications")
            .await
            .body
            .contains("No notifications.")
    );

    // Document page shows the open task; complete it from the form.
    let page = b.get(&format!("/documents/{id}")).await;
    assert!(page.body.contains("Review due") && page.body.contains("Record the completed review"));
    assert!(
        page.body.contains("organization default"),
        "reminder source shown"
    );
    let complete = regex::Regex::new(r"/documents/[0-9a-f-]{36}/tasks/([0-9a-f-]{36})/complete")
        .unwrap()
        .captures(&page.body)
        .unwrap()[0]
        .to_owned();
    let r = b
        .post(&complete, &[("_csrf", csrf.as_str()), ("note", "All good")])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = b.get(&format!("/documents/{id}")).await;
    assert!(
        page.body
            .contains("Completed. The schedule has been updated.")
    );
    assert!(page.body.contains("All good"));
    let next = documents::view(&app.core, &system(), id)
        .await
        .unwrap()
        .document
        .next_review_date;
    assert_eq!(next, Some(schedule::add_months(today(&app), 12)));

    // Override reminders on this document.
    let r = b
        .post(
            &format!("/documents/{id}/rules"),
            &[
                ("_csrf", csrf.as_str()),
                ("event", "review"),
                ("offsets", "60, 14"),
                ("overdue_repeat_days", ""),
                ("channel", "in_app"),
                ("recipient", "owner"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = b.get(&format!("/documents/{id}")).await;
    assert!(
        page.body.contains("60 days before, 14 days before")
            && page.body.contains("set on this document")
    );

    // Admin pages are admin-only.
    assert_eq!(
        b.get("/admin/notification-rules").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        b.get("/admin/notifications").await.status,
        StatusCode::FORBIDDEN
    );
    let (_, admin_secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut a = Browser::new(&app);
    a.login("admin@x.org", &admin_secret).await;
    let rules_page = a.get("/admin/notification-rules").await;
    assert_eq!(rules_page.status, StatusCode::OK);
    assert!(rules_page.body.contains("Organization default: Review"));
    let log = a.get("/admin/notifications").await;
    assert_eq!(log.status, StatusCode::OK);
    assert!(log.body.contains("owner@x.org"), "{}", log.body);
}
