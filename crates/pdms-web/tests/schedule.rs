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
            visibility: Default::default(),
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
    assert!(
        home.body.contains("Notifications <span class=\"count\">"),
        "a space keeps screen readers from running the label into the count"
    );
    assert!(
        home.body
            .contains("<span class=\"visually-hidden\"> unread</span>"),
        "unread badge has screen-reader text"
    );
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

#[tokio::test]
async fn dashboard_greets_and_shows_tiles() {
    let app = app().await;
    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("ada@example.org", &secret).await;
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("Good "), "greeting");
    assert!(home.body.contains("ada@example.org"), "first name");
    for tile in [
        "Overdue",
        "Due in 30 days",
        "Awaiting your approval",
        "Your reviews and renewals",
    ] {
        assert!(home.body.contains(tile), "tile {tile}");
    }
    assert!(home.body.contains("Nothing needs your attention."));
    assert!(home.body.contains("No recent changes."));
    // "Your access" now lives on the account page.
    assert!(!home.body.contains("Your access"));
    assert!(b.get("/account/tokens").await.body.contains("Your access"));
}

#[tokio::test]
async fn dashboard_panels_respect_visibility() {
    let app = app().await;
    let a = directory::create_department(&app.core.db, &system(), "AAA", "Dept Alpha")
        .await
        .unwrap();
    let bdept = directory::create_department(&app.core.db, &system(), "BBB", "Dept Bravo")
        .await
        .unwrap();
    let (_, secret) = user(&app.core, "alice@x.org", Some(a), None).await;
    let (bob, _) = user(&app.core, "bob@x.org", Some(bdept), None).await;
    let soon = Some(today(&app) + chrono::Duration::days(10));
    let mut ids = Vec::new();
    for (title, vis) in [
        ("Bravo dept doc", pdms_authz::Visibility::Department),
        ("Bravo restricted doc", pdms_authz::Visibility::Restricted),
        ("Bravo countywide doc", pdms_authz::Visibility::CountyWide),
    ] {
        let id = doc(
            &app,
            bdept,
            bob,
            title,
            "policy",
            MetadataInput {
                next_review_date: soon,
                ..Default::default()
            },
        )
        .await;
        documents::set_visibility(&app.core, &system(), id, vis)
            .await
            .unwrap();
        ids.push(id);
    }
    plan(&app).await;

    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("Bravo countywide doc"));
    assert!(!home.body.contains("Bravo dept doc"));
    assert!(!home.body.contains("Bravo restricted doc"));
    // One visible item is due in the next 30 days.
    let tile = regex::Regex::new(r#"(?s)Due in 30 days.*?class="value">(\d+)<"#)
        .unwrap()
        .captures(&home.body)
        .unwrap()[1]
        .to_owned();
    assert_eq!(tile, "1");
}

#[tokio::test]
async fn boards_panel_hidden_without_appointment_access() {
    let app = app().await;
    let (_, secret) = user(&app.core, "plain@x.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("plain@x.org", &secret).await;
    assert!(!b.get("/").await.body.contains("Boards and commissions"));

    let (_, s2) = user(&app.core, "clerk@x.org", None, Some("appointments_clerk")).await;
    let mut c = Browser::new(&app);
    c.login("clerk@x.org", &s2).await;
    let home = c.get("/").await;
    assert!(home.body.contains("Boards and commissions"));
    assert!(home.body.contains("All seats are filled and current."));
}

/// The markup of the panel whose heading id is `heading_id`.
fn panel<'a>(body: &'a str, heading_id: &str) -> &'a str {
    let start = body
        .find(&format!("aria-labelledby=\"{heading_id}\""))
        .unwrap_or_else(|| panic!("no panel {heading_id}"));
    let rest = &body[start..];
    &rest[..rest.find("</section>").unwrap()]
}

async fn spooled_pdf(app: &App) -> pdms_core::documents::files::Upload {
    let mut w =
        pdms_storage::spool::SpoolWriter::create(&app.core.config.storage.spool_path, 1 << 20)
            .await
            .unwrap();
    w.write(b"%PDF-1.4\n% hello\n%%EOF\n").await.unwrap();
    pdms_core::documents::files::Upload {
        file: w.finish().await.unwrap(),
        file_name: "policy.pdf".into(),
    }
}

#[tokio::test]
async fn recently_updated_labels_order_and_cap() {
    use chrono::TimeZone;
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let (uploader, secret) = user(&app.core, "uploader@x.org", Some(dept), None).await;
    let principal = pdms_core::auth::principal::load(&app.core.db, uploader)
        .await
        .unwrap();
    let ctx = pdms_core::Ctx::user(
        principal,
        "uploader@x.org",
        pdms_audit::AuthMethod::Local,
        &pdms_core::ClientInfo::default(),
    );
    let mut ids = Vec::new();
    for n in 1..=6 {
        ids.push(
            doc(
                &app,
                dept,
                uploader,
                &format!("Recent doc {n}"),
                "policy",
                MetadataInput::default(),
            )
            .await,
        );
    }
    // Doc 1: a file uploaded just now by a named person.
    documents::add_version(
        &app.core,
        &ctx,
        ids[0],
        spooled_pdf(&app).await,
        documents::VersionKind::Revision,
        "",
        None,
    )
    .await
    .unwrap();
    // Doc 2: has a file, but its details were edited well after the upload.
    documents::add_version(
        &app.core,
        &ctx,
        ids[1],
        spooled_pdf(&app).await,
        documents::VersionKind::Revision,
        "",
        None,
    )
    .await
    .unwrap();
    let tz = app.core.config.timezone();
    let noon = |days_ago: i64| {
        tz.from_local_datetime(
            &(today(&app) - chrono::Duration::days(days_ago))
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        )
        .unwrap()
        .with_timezone(&Utc)
    };
    // Doc 2 two minutes ago, doc 3 yesterday, 4 to 6 progressively older.
    let stamps = [
        (ids[1], Utc::now() - chrono::Duration::minutes(2)),
        (ids[2], noon(1)),
        (ids[3], noon(3)),
        (ids[4], noon(4)),
        (ids[5], noon(5)),
    ];
    for (id, at) in stamps {
        sqlx::query("UPDATE documents SET updated_at = $2 WHERE id = $1")
            .bind(id)
            .bind(at)
            .execute(&app._db.superuser)
            .await
            .unwrap();
    }

    let mut b = Browser::new(&app);
    b.login("uploader@x.org", &secret).await;
    let home = b.get("/").await;
    let recent = panel(&home.body, "recent-h");
    let pos = |t: &str| recent.find(t).unwrap_or_else(|| panic!("{t} missing"));
    assert!(recent.contains("New version by uploader@x.org"));
    assert!(recent.contains("Details updated"));
    assert!(recent.contains("Today"));
    assert!(recent.contains("Yesterday"));
    assert!(
        pos("Recent doc 1") < pos("Recent doc 2")
            && pos("Recent doc 2") < pos("Recent doc 3")
            && pos("Recent doc 3") < pos("Recent doc 4")
            && pos("Recent doc 4") < pos("Recent doc 5"),
        "newest first"
    );
    assert!(!recent.contains("Recent doc 6"), "only five are shown");
    // Doc 1 is the new-version row, doc 2 (older than 60 s) is not.
    let row2 = &recent[pos("Recent doc 2")..pos("Recent doc 3")];
    assert!(row2.contains("Details updated") && !row2.contains("New version"));
    let row1 = &recent[pos("Recent doc 1")..pos("Recent doc 2")];
    assert!(row1.contains("New version by uploader@x.org"));
}

#[tokio::test]
async fn overdue_review_task_row_has_primary_action() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let (owner, secret) = user(&app.core, "owner@x.org", Some(dept), None).await;
    let id = doc(
        &app,
        dept,
        owner,
        "Late policy",
        "policy",
        MetadataInput {
            next_review_date: Some(today(&app) - chrono::Duration::days(1)),
            ..Default::default()
        },
    )
    .await;
    plan(&app).await;
    let mut b = Browser::new(&app);
    b.login("owner@x.org", &secret).await;
    let home = b.get("/").await;
    let attn = panel(&home.body, "attention-h");
    assert!(attn.contains("Overdue"), "{attn}");
    assert!(attn.contains("1 day late"));
    assert!(attn.contains("Review: Late policy"));
    assert!(
        attn.contains(&format!(
            "class=\"button small primary\" href=\"/documents/{id}#reviews\""
        )),
        "primary Start review link"
    );
    assert!(attn.contains("Start review"));
}

/// Department A with one document, and department B with a Department-visible,
/// a Restricted and a County-wide document; alice belongs to A.
async fn overview_fixture(app: &App) -> (Uuid, Uuid, Vec<u8>) {
    let a = directory::create_department(&app.core.db, &system(), "AAA", "Dept Alpha")
        .await
        .unwrap();
    let bdept = directory::create_department(&app.core.db, &system(), "BBB", "Dept Bravo")
        .await
        .unwrap();
    let (alice, secret) = user(&app.core, "alice@x.org", Some(a), None).await;
    let (bob, _) = user(&app.core, "bob@x.org", Some(bdept), None).await;
    let soon = Some(today(app) + chrono::Duration::days(10));
    let m = || MetadataInput {
        next_review_date: soon,
        ..Default::default()
    };
    doc(app, a, alice, "Alpha doc", "policy", m()).await;
    for (title, vis) in [
        ("Bravo dept doc", pdms_authz::Visibility::Department),
        ("Bravo restricted doc", pdms_authz::Visibility::Restricted),
        ("Bravo countywide doc", pdms_authz::Visibility::CountyWide),
    ] {
        let id = doc(app, bdept, bob, title, "policy", m()).await;
        documents::set_visibility(&app.core, &system(), id, vis)
            .await
            .unwrap();
    }
    (a, bdept, secret)
}

fn tile_value(body: &str, label: &str) -> String {
    regex::Regex::new(&format!(r#"(?s){label}</span><span class="value">(\d+)<"#))
        .unwrap()
        .captures(body)
        .unwrap_or_else(|| panic!("no {label} tile"))[1]
        .to_owned()
}

#[tokio::test]
async fn overview_counts_respect_visibility() {
    let app = app().await;
    let (_, _, secret) = overview_fixture(&app).await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = b.get("/reports").await;
    assert_eq!(r.status, StatusCode::OK);
    // Alpha's own document plus Bravo's County-wide one.
    assert_eq!(tile_value(&r.body, "Documents"), "2");
    assert!(r.body.contains("2 documents"));
    assert!(r.body.contains("Documents by status: Active 2"));
    // Only visible departments are offered.
    assert!(r.body.contains("Dept Alpha") && r.body.contains("Dept Bravo"));
    assert!(!r.body.contains("Bravo dept doc"));
}

#[tokio::test]
async fn overview_charts_render_as_svg_with_labels() {
    let app = app().await;
    let (_, _, secret) = overview_fixture(&app).await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = b.get("/reports").await;
    assert!(r.body.contains(r#"role="img""#));
    assert!(r.body.contains(r#"aria-label="Items due per month:"#));
    assert!(r.body.matches("View as table").count() >= 3);
    assert!(r.body.contains("Nothing is overdue."));
    assert!(r.body.contains("All reports"));
    assert!(!r.body.contains("style=\""));
}

#[tokio::test]
async fn overview_period_and_department_filters() {
    let app = app().await;
    let (a, _, secret) = overview_fixture(&app).await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let labels = |body: &str| body.matches(r#"class="chart-label""#).count();
    assert_eq!(labels(&b.get("/reports").await.body), 6);
    assert_eq!(labels(&b.get("/reports?months=12").await.body), 12);
    assert_eq!(labels(&b.get("/reports?months=7").await.body), 6);
    let f = b.get(&format!("/reports?department={a}")).await;
    assert_eq!(tile_value(&f.body, "Documents"), "1");
    let bogus = b
        .get(&format!("/reports?department={}", Uuid::new_v4()))
        .await;
    assert_eq!(tile_value(&bogus.body, "Documents"), "2");
}
