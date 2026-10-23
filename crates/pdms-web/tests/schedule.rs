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
    schedule::local_today(Utc::now(), app.core.config().timezone())
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
        "interlocal_agreement",
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
    // Unread notices are marked with a word, not only a color.
    assert!(inbox.body.contains(r#"<li class="notice unread">"#));
    assert!(
        inbox
            .body
            .contains(r#"<span class="visually-hidden">Unread: </span>"#)
    );
    assert!(inbox.body.contains(r#"action="/notifications/ack-all""#));
    assert!(
        inbox
            .body
            .contains(r#"<a href="/notifications" aria-current="page">Unread</a>"#)
    );
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
    let empty = b.get("/notifications").await;
    assert!(
        empty.body.contains("You’re all caught up."),
        "{}",
        empty.body
    );
    assert!(!empty.body.contains(r#"action="/notifications/ack-all""#));

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
    assert!(
        home.body
            .contains("<h2 id=\"recent-h\">Recent activity</h2>")
    );
    assert!(
        home.body
            .contains("No recent activity on documents you can see.")
    );
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
    // The activity feed shows the County-wide document's creation, by name
    // (the document page shows who created it), and nothing of the others.
    let feed = panel(&home.body, "recent-h");
    assert!(feed.contains("Bravo countywide doc"), "{feed}");
    assert!(feed.contains("Created"), "{feed}");
    assert!(!feed.contains("Bravo dept doc") && !feed.contains("Bravo restricted doc"));
    // "Coming up": the visual date block is hidden from assistive technology,
    // and the full date is in the row's text for screen readers.
    let due = today(&app) + chrono::Duration::days(10);
    let coming = panel(&home.body, "upcoming-h");
    assert!(coming.contains(r#"<span class="date-block" aria-hidden="true">"#));
    assert!(
        coming.contains(&format!(
            r#"<time class="visually-hidden" datetime="{}">{}</time>"#,
            due.format("%Y-%m-%d"),
            due.format("%B %-d, %Y")
        )),
        "{coming}"
    );
    // One visible item is due in the next 30 days.
    let tile = regex::Regex::new(r#"(?s)Due in 30 days.*?class="value">(\d+)<"#)
        .unwrap()
        .captures(&home.body)
        .unwrap()[1]
        .to_owned();
    assert_eq!(tile, "1");
}

#[tokio::test]
async fn coming_up_says_how_many_more_there_are() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "AAA", "Dept Alpha")
        .await
        .unwrap();
    let (owner, secret) = user(&app.core, "alice@x.org", Some(dept), None).await;
    for n in 1..=10 {
        doc(
            &app,
            dept,
            owner,
            &format!("Policy {n}"),
            "policy",
            MetadataInput {
                next_review_date: Some(today(&app) + chrono::Duration::days(n)),
                ..Default::default()
            },
        )
        .await;
    }
    plan(&app).await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let home = b.get("/").await;
    let coming = panel(&home.body, "upcoming-h");
    assert_eq!(
        coming.matches("class=\"date-block\"").count(),
        8,
        "{coming}"
    );
    assert!(
        coming.contains(
            r#"<a href="/calendar?view=agenda&amp;days=30">2 more in the next 30 days</a>"#
        ),
        "{coming}"
    );
    // The link follows the chosen range.
    let year = b.get("/?days=365").await;
    assert!(
        panel(&year.body, "upcoming-h").contains("2 more in the next 365 days"),
        "{}",
        year.body
    );
}

#[tokio::test]
async fn calendar_month_caps_days_and_lists_them_for_phones() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "AAA", "Dept Alpha")
        .await
        .unwrap();
    let (owner, secret) = user(&app.core, "alice@x.org", Some(dept), None).await;
    let busy = today(&app) + chrono::Duration::days(10);
    for n in 1..=5 {
        doc(
            &app,
            dept,
            owner,
            &format!("Busy day policy {n}"),
            "policy",
            MetadataInput {
                next_review_date: Some(busy),
                ..Default::default()
            },
        )
        .await;
    }
    doc(
        &app,
        dept,
        owner,
        "Late policy",
        "policy",
        MetadataInput {
            next_review_date: Some(today(&app) - chrono::Duration::days(3)),
            ..Default::default()
        },
    )
    .await;
    plan(&app).await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;

    let month = b
        .get(&format!(
            "/calendar?view=month&month={}",
            busy.format("%Y-%m")
        ))
        .await;
    assert_eq!(month.status, StatusCode::OK);
    let grid = &month.body[month.body.find("class=\"month-grid\"").unwrap()..];
    let grid = &grid[..grid.find("</table>").unwrap()];
    assert_eq!(
        grid.matches(">Review: Busy day policy").count(),
        3,
        "three per day"
    );
    assert!(
        grid.contains(&format!(
            r#"href="/calendar?view=agenda&amp;from={busy}&amp;days=7&amp;">+2 more"#
        )),
        "{grid}"
    );
    // The phone list has every event of the day.
    let list = &month.body[month.body.find("class=\"month-list\"").unwrap()..];
    assert_eq!(list.matches("Busy day policy").count(), 5);

    // The day's agenda shows them all, starting that day.
    let agenda = b
        .get(&format!("/calendar?view=agenda&from={busy}&days=7"))
        .await;
    assert_eq!(agenda.body.matches("Busy day policy").count(), 5);
    assert!(
        agenda
            .body
            .contains(&format!("from {}", busy.format("%b %-d, %Y")))
    );

    // Overdue items are one link on today, in the month that holds today.
    let now = b.get("/calendar").await;
    assert!(
        now.body
            .contains(r#"<a class="cal-ev overdue" href="/reports/overdue">1 overdue</a>"#),
        "{}",
        now.body
    );

    // Range tabs mark the current range.
    let agenda = b.get("/calendar?view=agenda&days=365").await;
    assert!(
        agenda
            .body
            .contains(r#"days=365&amp;" aria-current="page">365 days</a>"#)
    );

    // A filter with no results offers to clear it.
    let other = directory::create_department(&app.core.db, &system(), "BBB", "Dept Bravo")
        .await
        .unwrap();
    let empty = b
        .get(&format!("/calendar?view=agenda&department={other}"))
        .await;
    assert!(empty.body.contains("Nothing scheduled in this period."));
    assert!(empty.body.contains("Clear the filters"));

    // The feeds page keeps its form.
    let feeds = b.get("/calendar/feeds").await;
    assert!(
        feeds
            .body
            .contains(r#"action="/calendar/feeds" class="grid-form""#)
    );
    for name in ["name", "department_id", "only_owned"] {
        assert!(feeds.body.contains(&format!(r#"name="{name}""#)), "{name}");
    }
    assert!(feeds.body.contains("No subscriptions yet."));
    assert_eq!(feeds.body.matches("<h1>").count(), 1);
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
        pdms_storage::spool::SpoolWriter::create(&app.core.config().storage.spool_path, 1 << 20)
            .await
            .unwrap();
    w.write(b"%PDF-1.4\n% hello\n%%EOF\n").await.unwrap();
    pdms_core::documents::files::Upload {
        file: w.finish().await.unwrap(),
        file_name: "policy.pdf".into(),
    }
}

#[tokio::test]
async fn activity_feed_order_names_and_cap() {
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
    for n in 1..=11 {
        ids.push(
            doc(
                &app,
                dept,
                uploader,
                &format!("Recent doc {n:02}"),
                "policy",
                MetadataInput::default(),
            )
            .await,
        );
    }
    // Two new files, the newest events of all: doc 01, then doc 02.
    for id in [ids[0], ids[1]] {
        documents::add_version(
            &app.core,
            &ctx,
            id,
            spooled_pdf(&app).await,
            documents::VersionKind::Revision,
            "",
            None,
        )
        .await
        .unwrap();
    }

    let mut b = Browser::new(&app);
    b.login("uploader@x.org", &secret).await;
    let home = b.get("/").await;
    let feed = panel(&home.body, "recent-h");
    // Newest first, ten at most (13 events happened).
    assert_eq!(feed.matches("<li class=\"split\">").count(), 10, "{feed}");
    let first = feed.find("<li class=\"split\">").unwrap();
    let first = &feed[first..feed[first..].find("</li>").unwrap() + first];
    assert!(
        first.contains("Recent doc 02") && first.contains("New version · uploader@x.org"),
        "{first}"
    );
    assert!(feed.contains("Today"));
    assert!(feed.contains("Created"));
    // Both uploads are named; the oldest creations fell off the end.
    assert_eq!(feed.matches("New version · uploader@x.org").count(), 2);
    assert!(!feed.contains("Recent doc 03"), "{feed}");
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
async fn overview_status_chart_total_matches_the_documents_tile() {
    let app = app().await;
    let (a, _, secret) = overview_fixture(&app).await;
    let alice: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = 'alice@x.org'")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    for (title, status) in [
        ("Old policy", "superseded"),
        ("Shelved policy", "archived"),
        ("Lapsed policy", "expired"),
        ("Draft policy", "draft"),
    ] {
        doc(&app, a, alice, title, "policy", MetadataInput::default()).await;
        sqlx::query("UPDATE documents SET status = $1::document_status WHERE title = $2")
            .bind(status)
            .bind(title)
            .execute(&app.core.db)
            .await
            .unwrap();
    }
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = b.get("/reports").await;
    let tile: i64 = tile_value(&r.body, "Documents").parse().unwrap();
    let aria = regex::Regex::new(r#"aria-label="Documents by status: ([^"]*)""#)
        .unwrap()
        .captures(&r.body)
        .unwrap()[1]
        .to_owned();
    let chart: i64 = regex::Regex::new(r"(\d+)")
        .unwrap()
        .captures_iter(&aria)
        .map(|c| c[1].parse::<i64>().unwrap())
        .sum();
    // Alpha doc, the County-wide doc, expired and draft; not superseded or
    // archived.
    assert_eq!(tile, 4, "{aria}");
    assert_eq!(chart, tile, "{aria}");
    assert!(aria.contains("Other 2"), "{aria}");
    // The overdue tile says what it counts.
    assert!(
        r.body
            .contains(r#"<span class="label">Overdue dates</span>"#)
    );
    assert!(r.body.contains("Documents and board terms · "));
}

#[tokio::test]
async fn report_pages_long_results_but_the_csv_has_every_row() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "AAA", "Dept Alpha")
        .await
        .unwrap();
    let (owner, secret) = user(&app.core, "alice@x.org", Some(dept), None).await;
    for n in 0..105 {
        doc(
            &app,
            dept,
            owner,
            &format!("Paged policy {n:03}"),
            "policy",
            MetadataInput {
                next_review_date: Some(today(&app) + chrono::Duration::days(5)),
                ..Default::default()
            },
        )
        .await;
    }
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let first = b.get("/reports/upcoming?days=30").await;
    assert_eq!(first.status, StatusCode::OK);
    assert!(
        first.body.contains(r#"<h2 id="result-h">105 rows</h2>"#),
        "{}",
        first.body
    );
    assert_eq!(first.body.matches("Paged policy").count(), 100);
    assert!(first.body.contains("Page 1 of 2"));
    let second = b.get("/reports/upcoming?days=30&page=2").await;
    assert_eq!(second.body.matches("Paged policy").count(), 5);
    assert!(second.body.contains("← Previous"));
    let csv = b.get("/reports/upcoming.csv?days=30").await;
    assert_eq!(csv.body.matches("Paged policy").count(), 105);
    assert_eq!(first.body.matches("<h1>").count(), 1);
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

/// The numbers in an "Items due per month: Oct 2, Nov 0, …." aria label.
fn month_bars(body: &str) -> Vec<i64> {
    let label = regex::Regex::new(r#"aria-label="Items due per month: ([^"]*)\.""#)
        .unwrap()
        .captures(body)
        .expect("due-by-month chart")[1]
        .to_owned();
    label
        .split(", ")
        .map(|p| p.rsplit(' ').next().unwrap().parse().unwrap())
        .collect()
}

#[tokio::test]
async fn overview_month_total_is_the_sum_of_its_bars() {
    use chrono::{Datelike, Months};
    let app = app().await;
    let (a, _, secret) = overview_fixture(&app).await;
    let (owner, _) = user(&app.core, "carol@x.org", Some(a), None).await;
    let today = today(&app);
    let first = today.with_day(1).unwrap();
    // Six bars: this month through the fifth month from now.
    let after = first.checked_add_months(Months::new(6)).unwrap();
    let last = after.pred_opt().unwrap();
    for (title, date) in [("Last day shown", last), ("Month after the chart", after)] {
        doc(
            &app,
            a,
            owner,
            title,
            "policy",
            MetadataInput {
                next_review_date: Some(date),
                ..Default::default()
            },
        )
        .await;
    }
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = b.get("/reports").await;
    let bars = month_bars(&r.body);
    assert_eq!(bars.len(), 6);
    // The fixture's two visible reviews (in ten days) and the last day shown;
    // the month after the chart has no bar and is not in the total.
    let sum: i64 = bars.iter().sum();
    assert_eq!(sum, 3, "{bars:?}");
    assert_eq!(*bars.last().unwrap(), 1, "{bars:?}");
    let due = panel(&r.body, "ch-due");
    assert!(due.contains("Next 6 months · 3 items"), "{due}");
    // "View as table" covers exactly the same days.
    let days = (last - today).num_days();
    let link = format!("/reports/upcoming?days={days}");
    assert!(due.contains(&format!(r#"href="{link}""#)), "{due}");
    let table = b.get(&link).await;
    assert!(table.body.contains("Last day shown"));
    assert!(!table.body.contains("Month after the chart"));
}

/// The count on a "Boards and commissions" row.
fn board_count(body: &str, label: &str) -> String {
    let boards = panel(body, "ch-boards");
    regex::Regex::new(&format!(
        r#"<li><span>{label}</span><strong>(\d+)</strong></li>"#
    ))
    .unwrap()
    .captures(boards)
    .unwrap_or_else(|| panic!("no {label} row in {boards}"))[1]
        .to_owned()
}

#[tokio::test]
async fn overview_boards_follow_the_department_filter() {
    use pdms_core::appointments::{self, NewAppointment, PositionSpec};
    let app = app().await;
    let (a, bdept, secret) = overview_fixture(&app).await;
    let alice = directory::list_users(&app.core.db, "alice@x.org", 1)
        .await
        .unwrap()[0]
        .id;
    // A third department alice can see a document of, with no boards.
    let c = directory::create_department(&app.core.db, &system(), "CCC", "Dept Charlie")
        .await
        .unwrap();
    let (carl, _) = user(&app.core, "carl@x.org", Some(c), None).await;
    let cw = doc(
        &app,
        c,
        carl,
        "Charlie countywide doc",
        "policy",
        MetadataInput::default(),
    )
    .await;
    documents::set_visibility(&app.core, &system(), cw, pdms_authz::Visibility::CountyWide)
        .await
        .unwrap();

    let position = async |name: &str, dept: Uuid, seats: i32, manager: Option<Uuid>| {
        appointments::create_position(
            &app.core,
            &system(),
            PositionSpec {
                name: name.into(),
                body_name: "Advisory Committee".into(),
                department_id: Some(dept),
                appointing_authority: "Board".into(),
                term_months: Some(24),
                seats,
                manager_user_id: manager,
                reappointment_lead_days: 90,
                description: String::new(),
                is_active: true,
            },
        )
        .await
        .unwrap()
    };
    // Alpha (alice's department): two seats, one filled and current.
    let alpha_seat = position("Alpha seat", a, 2, None).await;
    let t = today(&app);
    appointments::appoint(
        &app.core,
        &system(),
        alpha_seat,
        NewAppointment {
            person_name: "Pat Example".into(),
            start_date: t - chrono::Duration::days(100),
            end_date: Some(t + chrono::Duration::days(400)),
            reappointment_due_date: Some(t + chrono::Duration::days(300)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Bravo: one vacant seat alice sees because she manages it.
    position("Bravo seat", bdept, 1, Some(alice)).await;

    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let all = b.get("/reports").await;
    assert!(panel(&all.body, "ch-boards").contains("Seats you can see"));
    assert_eq!(board_count(&all.body, "Filled and current"), "1");
    assert_eq!(board_count(&all.body, "Vacant"), "2");

    let alpha = b.get(&format!("/reports?department={a}")).await;
    assert!(panel(&alpha.body, "ch-boards").contains("Seats you can see in this department"));
    assert_eq!(board_count(&alpha.body, "Filled and current"), "1");
    assert_eq!(board_count(&alpha.body, "Vacant"), "1");

    let bravo = b.get(&format!("/reports?department={bdept}")).await;
    assert_eq!(board_count(&bravo.body, "Filled and current"), "0");
    assert_eq!(board_count(&bravo.body, "Vacant"), "1");
    // The tiles and tables open the same department.
    assert!(
        bravo
            .body
            .contains(&format!(r#"href="/reports/overdue?department={bdept}""#))
    );

    let charlie = b.get(&format!("/reports?department={c}")).await;
    let boards = panel(&charlie.body, "ch-boards");
    assert!(boards.contains("No boards or commissions you can see belong to this department."));
    assert!(!boards.contains("<strong>"));
}

#[tokio::test]
async fn overview_overdue_by_department_names_are_text_rows() {
    let app = app().await;
    let (a, _, secret) = overview_fixture(&app).await;
    let long = directory::create_department(
        &app.core.db,
        &system(),
        "RBM",
        "Road and Bridge Maintenance and Operations",
    )
    .await
    .unwrap();
    let (owner, _) = user(&app.core, "carol@x.org", Some(a), None).await;
    let (dave, _) = user(&app.core, "dave@x.org", Some(long), None).await;
    let past = Some(today(&app) - chrono::Duration::days(20));
    let late = || MetadataInput {
        next_review_date: past,
        ..Default::default()
    };
    for t in ["Late one", "Late two"] {
        doc(&app, a, owner, t, "policy", late()).await;
    }
    let id = doc(&app, long, dave, "Late road policy", "policy", late()).await;
    documents::set_visibility(&app.core, &system(), id, pdms_authz::Visibility::CountyWide)
        .await
        .unwrap();

    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = b.get("/reports").await;
    let over = panel(&r.body, "ch-over");
    // One summary for assistive technology, most overdue first.
    assert!(
        over.contains(
            r#"<div class="hbars" role="img" aria-label="Overdue items by department: Dept Alpha 2, Road and Bridge Maintenance and Operations 1.">"#
        ),
        "{over}"
    );
    // Each name is a whole line of HTML text above its bar, never cut short.
    assert!(
        over.contains(
            r#"<span class="hbar-label">Road and Bridge Maintenance and Operations</span>"#
        )
    );
    // The bars: critical color, a native tooltip, the count after the bar.
    assert_eq!(over.matches(r#"<g class="chart-seg-critical">"#).count(), 2);
    assert!(over.contains("<title>Dept Alpha: 2</title>"));
    assert!(over.contains(r#"<svg class="hbar" width="80%" height="16">"#));
    assert!(over.contains(r#"<svg class="hbar" width="40%" height="16">"#));
    assert!(over.contains(r#"<span class="hbar-value">2</span>"#));
    assert!(!over.contains("style="));
}
