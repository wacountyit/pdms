// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Administration → Entra ID sync.

mod support;

use pdms_core::config::Secret;
use pdms_core::directory::{self, entra_values};
use support::*;

#[tokio::test]
async fn admins_map_department_values_and_see_sync_status() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let rb = directory::create_department(&app.core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;

    let page = b.get("/admin/entra").await;
    assert_eq!(page.status, 200);
    assert!(
        page.body
            .contains("The Microsoft 365 connection is not set up"),
        "{}",
        page.body
    );
    assert!(page.body.contains("No sync has completed yet."));
    assert!(!page.body.contains("Certificate thumbprint"));

    let r = b
        .post(
            "/admin/entra/values",
            &[
                ("_csrf", &page.csrf()),
                ("value", "Road & Bridge"),
                ("department_id", &rb.to_string()),
            ],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    let page = b.get("/admin/entra").await;
    assert!(page.body.contains("Road &#38; Bridge"), "{}", page.body);

    // Sync now is refused while Graph is not configured.
    let r = b
        .post("/admin/entra/sync", &[("_csrf", &page.csrf())])
        .await;
    assert!(r.status.is_redirection());
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'directory.entra_sync'")
            .fetch_one(&app.core.db)
            .await
            .unwrap();
    assert_eq!(jobs, 0);

    let r = b
        .post(
            "/admin/entra/values/remove",
            &[("_csrf", &page.csrf()), ("value", "road and bridge")],
        )
        .await;
    assert!(r.status.is_redirection());
    assert!(entra_values::list(&app.core.db).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_status_panel_shows_the_certificate_thumbprint_and_last_run() {
    let mut cfg = config();
    cfg.graph.tenant_id = "tenant-1".into();
    cfg.graph.client_id = "client-1".into();
    cfg.graph.certificate = Secret::new(
        pdms_core::graph::cert::generate("PDMS test", 30)
            .unwrap()
            .pem,
    );
    let app = app_with(cfg).await;
    let thumbprint = app.core.graph().as_ref().unwrap().thumbprint().to_owned();
    let summary = serde_json::json!({ "created": 3, "updated": 1, "departed": 0, "unmapped": 2 });
    sqlx::query(
        "INSERT INTO sync_cursors (name, last_run, last_summary) VALUES ('entra.users', now(), $1)",
    )
    .bind(summary)
    .execute(&app.core.db)
    .await
    .unwrap();
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;

    let page = b.get("/admin/entra").await;
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("Certificate thumbprint"),
        "{}",
        page.body
    );
    assert!(page.body.contains(&thumbprint), "{}", page.body);
    assert!(
        page.body.contains(
            "3 created, 1 updated, 0 departed, 0 reactivated; of the people it read, 2 had no mapped department"
        ),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("changes made in Entra since the last run"),
        "{}",
        page.body
    );
}

#[tokio::test]
async fn people_without_directory_rights_cannot_open_it() {
    let app = app().await;
    let (_, secret) = user(&app.core, "staff@x.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("staff@x.org", &secret).await;
    assert_eq!(b.get("/admin/entra").await.status, 403);
    let page = b.get("/").await;
    let r = b
        .post(
            "/admin/entra/sync",
            &[("_csrf", &page.csrf()), ("full", "true")],
        )
        .await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn administrators_change_the_sync_settings_on_the_page() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/entra").await;
    assert!(page.body.contains(r#"id="settings-h""#));
    assert!(
        !page.body.contains("/admin/entra/preview"),
        "no preview without the connection"
    );
    let csrf = page.csrf();

    b.post(
        "/admin/entra/settings",
        &[
            ("_csrf", &csrf),
            ("run_at", "03:15"),
            ("attribute", "officeLocation"),
            ("include_guests", "true"),
        ],
    )
    .await;
    let e = &app.core.config().entra_sync;
    assert_eq!(e.run_at, "03:15");
    assert_eq!(e.attribute, "officeLocation");
    assert!(e.include_guests);
    assert!(!e.enabled);

    // Turning it on needs the Microsoft 365 connection: refused, with the reason.
    b.post(
        "/admin/entra/settings",
        &[
            ("_csrf", &csrf),
            ("enabled", "true"),
            ("run_at", "03:15"),
            ("attribute", "officeLocation"),
        ],
    )
    .await;
    let page = b.get("/admin/entra").await;
    assert!(
        page.body.contains("entra_sync needs [graph]"),
        "{}",
        page.body
    );
    assert!(!app.core.config().entra_sync.enabled);
    assert!(app.core.config().entra_sync.include_guests, "nothing saved");

    // A bad time is refused too.
    b.post(
        "/admin/entra/settings",
        &[
            ("_csrf", &csrf),
            ("run_at", "25:00"),
            ("attribute", "department"),
        ],
    )
    .await;
    assert_eq!(app.core.config().entra_sync.run_at, "03:15");
}

const GROUP: &str = "6f1b2c3d-0000-4000-8000-00000000abcd";

#[tokio::test]
async fn the_group_field_saves_and_shows_read_only_from_the_configuration_file() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/entra").await;
    assert!(page.body.contains(r#"name="group_id""#), "{}", page.body);
    assert!(page.body.contains("Only members of this group"));
    let csrf = page.csrf();
    let form = |group: &'static str| {
        [
            ("_csrf", csrf.clone()),
            ("run_at", "03:15".to_owned()),
            ("attribute", "department".to_owned()),
            ("group_id", group.to_owned()),
        ]
    };
    let f = form(GROUP);
    let pairs: Vec<(&str, &str)> = f.iter().map(|(k, v)| (*k, v.as_str())).collect();
    b.post("/admin/entra/settings", &pairs).await;
    let page = b.get("/admin/entra").await;
    assert!(
        page.body.contains(&format!(r#"value="{GROUP}""#)),
        "{}",
        page.body
    );

    let f = form("PDMS");
    let pairs: Vec<(&str, &str)> = f.iter().map(|(k, v)| (*k, v.as_str())).collect();
    b.post("/admin/entra/settings", &pairs).await;
    let page = b.get("/admin/entra").await;
    assert!(
        page.body.contains("must be the group&#39;s object ID"),
        "{}",
        page.body
    );
    assert_eq!(app.core.config().entra_sync.group_id, GROUP);

    let mut c = config();
    c.entra_sync.group_id = GROUP.into();
    c.locked.insert("entra_sync.group_id".into());
    let app = app_with(c).await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/entra").await;
    let at = page.body.find(r#"name="group_id""#).unwrap();
    let tag = &page.body[at..page.body[at..].find('>').unwrap() + at];
    assert!(tag.contains("disabled"), "{tag}");
}

#[tokio::test]
async fn test_group_without_the_connection_says_so() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/entra").await;
    let r = b
        .post(
            "/admin/entra/test-group",
            &[("_csrf", &page.csrf()), ("group_id", GROUP)],
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.body
            .contains("Set up the Microsoft 365 connection first."),
        "{}",
        r.body
    );
    assert!(r.body.contains("Test result"));
}

#[tokio::test]
async fn test_group_uses_the_value_from_the_configuration_file() {
    let mut c = config();
    c.entra_sync.group_id = GROUP.into();
    c.locked.insert("entra_sync.group_id".into());
    let app = app_with(c).await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/entra").await;
    // A disabled field is not posted.
    let r = b
        .post("/admin/entra/test-group", &[("_csrf", &page.csrf())])
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains("Enter the group"), "{}", r.body);
    assert!(
        r.body
            .contains("Set up the Microsoft 365 connection first.")
    );
    // The field still shows the file's value, read-only.
    let group = field(&r.body, "group_id");
    assert!(group.contains(&format!(r#"value="{GROUP}""#)), "{group}");
    assert!(group.contains("disabled"), "{group}");
}

/// The rest of the first tag with this `name`, from the name on.
fn field<'a>(body: &'a str, name: &str) -> &'a str {
    let at = body
        .find(&format!(r#"name="{name}""#))
        .unwrap_or_else(|| panic!("no field {name}: {body}"));
    &body[at..at + body[at..].find('>').unwrap()]
}

#[tokio::test]
async fn test_group_keeps_what_was_typed() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let saved = app.core.config().entra_sync.clone();
    assert!(saved.group_id.is_empty() && !saved.enabled && !saved.include_guests);
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/entra").await;
    let r = b
        .post(
            "/admin/entra/test-group",
            &[
                ("_csrf", &page.csrf()),
                ("enabled", "true"),
                ("run_at", "04:45"),
                ("attribute", "officeLocation"),
                ("group_id", GROUP),
                ("include_guests", "true"),
            ],
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("Test result"), "{}", r.body);
    // The form shows what was typed, so Save after it saves that.
    let group = field(&r.body, "group_id");
    assert!(group.contains(&format!(r#"value="{GROUP}""#)), "{group}");
    let run_at = field(&r.body, "run_at");
    assert!(run_at.contains(r#"value="04:45""#), "{run_at}");
    let attribute = field(&r.body, "attribute");
    assert!(
        attribute.contains(r#"value="officeLocation""#),
        "{attribute}"
    );
    assert!(field(&r.body, "enabled").contains("checked"));
    assert!(field(&r.body, "include_guests").contains("checked"));
    assert_eq!(r.body.matches("<h1").count(), 1);

    // Nothing was saved: a plain visit shows the saved values.
    let e = app.core.config().entra_sync.clone();
    assert_eq!(
        (
            e.group_id,
            e.run_at,
            e.attribute,
            e.enabled,
            e.include_guests
        ),
        (
            saved.group_id,
            saved.run_at.clone(),
            saved.attribute.clone(),
            false,
            false
        )
    );
    let page = b.get("/admin/entra").await;
    let group = field(&page.body, "group_id");
    assert!(group.contains(r#"value="""#), "{group}");
    let run_at = field(&page.body, "run_at");
    assert!(
        run_at.contains(&format!(r#"value="{}""#, saved.run_at)),
        "{run_at}"
    );
    assert!(!field(&page.body, "include_guests").contains("checked"));
}

/// The sync on and limited to [`GROUP`] (the Microsoft 365 connection is
/// not set up, so nothing reaches Graph).
fn grouped() -> pdms_core::config::Config {
    let mut c = config();
    c.entra_sync.enabled = true;
    c.entra_sync.group_id = GROUP.into();
    c
}

/// An administrator signed in, and a last run that held back six people and
/// reactivated one administrator. Returns the held-back IDs.
async fn held_back_page(app: &App) -> (Browser, Vec<uuid::Uuid>) {
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut held_back = Vec::new();
    for i in 1..=6 {
        let id = directory::create_user(
            &app.core.db,
            &system(),
            &format!("Held Person {i}"),
            &format!("held{i}@x.org"),
            None,
        )
        .await
        .unwrap();
        held_back.push(id);
    }
    let (back, _) = user(&app.core, "back@x.org", None, Some("admin")).await;
    let summary = serde_json::json!({
        "created": 0, "updated": 0, "departed": 0, "unmapped": 0,
        "held_back": held_back, "reactivated": [back],
    });
    sqlx::query(
        "INSERT INTO sync_cursors (name, last_run, last_summary) VALUES ('entra.users', now(), $1)",
    )
    .bind(summary)
    .execute(&app.core.db)
    .await
    .unwrap();
    let mut b = Browser::new(app);
    b.login("admin@x.org", &secret).await;
    (b, held_back)
}

#[tokio::test]
async fn the_page_lists_held_back_and_reactivated_people() {
    let app = app_with(grouped()).await;
    let (mut b, held_back) = held_back_page(&app).await;

    let page = b.get("/admin/entra").await;
    assert_eq!(page.status, 200);
    let body = &page.body;
    assert!(body.contains("Who is synced"), "{body}");
    assert!(body.contains("Members of the group"), "{body}");
    assert!(body.contains("0 departed, 1 reactivated"), "{body}");
    assert!(body.contains("Waiting for your confirmation"), "{body}");
    assert!(body.contains("These 6 people are outside the PDMS group"));
    assert!(body.contains("Depart these 6 people"), "{body}");
    assert!(body.contains(r#"action="/admin/entra/depart-held-back""#));
    for i in 1..=6 {
        assert!(body.contains(&format!("Held Person {i}")), "{body}");
        assert!(body.contains(&format!("held{i}@x.org")), "{body}");
    }
    assert!(
        body.contains("Reactivated since the last run started"),
        "{body}"
    );
    assert!(body.contains("back@x.org"), "{body}");
    assert!(body.contains("Has roles: Administrator"), "{body}");
    assert_eq!(body.matches("<h1").count(), 1);

    // Without the Microsoft 365 connection, nobody is departed.
    let r = b
        .post("/admin/entra/depart-held-back", &[("_csrf", &page.csrf())])
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    assert_eq!(r.location(), "/admin/entra");
    let page = b.get("/admin/entra").await;
    assert!(page.body.contains("flash-error"), "{}", page.body);
    assert!(page.body.contains("Depart these 6 people"), "still waiting");
    for id in held_back {
        let status: String = sqlx::query_scalar("SELECT status::text FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&app.core.db)
            .await
            .unwrap();
        assert_eq!(status, "active");
    }
}

#[tokio::test]
async fn held_back_people_show_only_with_a_group_and_the_sync_on() {
    let mut no_group = grouped();
    no_group.entra_sync.group_id.clear();
    let mut sync_off = grouped();
    sync_off.entra_sync.enabled = false;
    for (what, cfg) in [("no group", no_group), ("sync off", sync_off)] {
        let app = app_with(cfg).await;
        let (mut b, _) = held_back_page(&app).await;
        let body = b.get("/admin/entra").await.body;
        assert!(
            !body.contains("Waiting for your confirmation"),
            "{what}: {body}"
        );
        assert!(!body.contains("/admin/entra/depart-held-back"), "{what}");
        assert!(!body.contains("held1@x.org"), "{what}");
        // The reactivated list still shows: those people are active again.
        assert!(body.contains("back@x.org"), "{what}");
    }
}

#[tokio::test]
async fn departed_people_are_not_offered_for_departure() {
    let app = app_with(grouped()).await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let (gone, _) = user(&app.core, "gone@x.org", None, None).await;
    let (left, _) = user(&app.core, "left@x.org", None, None).await;
    directory::set_user_status(
        &app.core.db,
        &system(),
        gone,
        pdms_core::model::UserStatus::Departed,
    )
    .await
    .unwrap();
    let summary = serde_json::json!({ "held_back": [gone, left] });
    sqlx::query(
        "INSERT INTO sync_cursors (name, last_run, last_summary) VALUES ('entra.users', now(), $1)",
    )
    .bind(summary)
    .execute(&app.core.db)
    .await
    .unwrap();
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;

    // One person still waits.
    let page = b.get("/admin/entra").await;
    assert!(page.body.contains("Waiting for your confirmation"));
    assert!(
        page.body.contains("This person is outside the PDMS group"),
        "{}",
        page.body
    );
    assert!(page.body.contains("Depart this person"), "{}", page.body);
    assert!(!page.body.contains("Depart these"), "{}", page.body);
    assert!(page.body.contains("left@x.org"));
    assert!(!page.body.contains("gone@x.org"), "{}", page.body);

    // Nobody does.
    directory::set_user_status(
        &app.core.db,
        &system(),
        left,
        pdms_core::model::UserStatus::Departed,
    )
    .await
    .unwrap();
    let page = b.get("/admin/entra").await;
    assert!(!page.body.contains("Waiting for your confirmation"));
    assert!(!page.body.contains("/admin/entra/depart-held-back"));
    assert!(!page.body.contains("Reactivated since the last run started"));
}
