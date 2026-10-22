// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;
use axum::http::StatusCode;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::{Core, acl, branding, directory};
use support::*;

/// Minimal PNG header bytes: signature + IHDR chunk with the given size.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    v
}

/// Like `app()`, but also hands back the state so a test can reload branding.
async fn app_and_state() -> (App, pdms_web::AppState) {
    let db = pdms_testkit::test_db().await;
    let core = Core::new(db.app.clone(), config()).unwrap();
    let state = pdms_web::AppState::new(core.clone(), None, None);
    let router = pdms_web::router(state.clone());
    (
        App {
            core,
            router,
            _db: db,
        },
        state,
    )
}

fn header<'a>(r: &'a Resp, name: &str) -> &'a str {
    r.headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

#[tokio::test]
async fn theme_css_is_public_and_carries_the_accent() {
    let app = app().await;
    let mut b = Browser::new(&app);
    let r = b.get("/theme.css").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
    assert!(r.body.contains("--accent: #B45309;"), "{}", r.body);
    assert!(r.body.contains("--accent-link:"));
    assert!(
        r.headers["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
}

#[tokio::test]
async fn seal_is_404_until_set() {
    let app = app().await;
    let mut b = Browser::new(&app);
    assert_eq!(b.get("/branding/seal").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shell_has_sidebar_menu_and_permissions() {
    let app = app().await;
    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("ada@example.org", &secret).await;
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    for item in [
        "Dashboard",
        "Documents",
        "Calendar",
        "Appointments",
        "Reports",
        "Notifications",
    ] {
        assert!(home.body.contains(item), "missing {item}");
    }
    assert!(home.body.contains("href=\"/theme.css?v="));
    assert!(home.body.contains("class=\"menu-toggle\""));
    assert!(
        !home.body.contains(">Administration<"),
        "plain user must not see Administration"
    );
    assert!(!home.body.contains(">Audit log<"));
    assert!(home.body.contains("name=\"q\""), "top bar search");
    // No seal set: the generic mark, and no link to the seal.
    assert!(
        home.body
            .contains("<span class=\"mark\" aria-hidden=\"true\">PADM</span>")
    );
    assert!(!home.body.contains("/branding/seal"));
}

#[tokio::test]
async fn admin_sees_manage_group_and_admin_tabs() {
    let app = app().await;
    let (_, secret) = user(&app.core, "root@example.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("root@example.org", &secret).await;
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains(">Manage<"), "{}", home.body);
    assert!(home.body.contains(">Administration<"));
    assert!(home.body.contains(">Audit log<"));
    // Not on an admin page: no admin tab row.
    assert!(!home.body.contains("class=\"tabs admin-tabs\""));

    let users = b.get("/admin/users").await;
    assert_eq!(users.status, StatusCode::OK);
    assert!(
        users.body.contains("class=\"tabs admin-tabs\""),
        "{}",
        users.body
    );
    assert!(users.body.contains("href=\"/admin/departments\""));
    // The current admin tab is marked, and only that one.
    assert!(
        users
            .body
            .contains("<a href=\"/admin/users\" aria-current=\"page\">Users</a>")
    );
    assert!(
        users
            .body
            .contains("<a href=\"/admin/departments\" >Departments</a>")
    );
    let entra = b.get("/admin/entra").await;
    assert_eq!(entra.status, StatusCode::OK, "{}", entra.location());
    assert!(
        entra
            .body
            .contains("<a href=\"/admin/entra\" aria-current=\"page\">")
    );
    let tabs = &entra.body[entra.body.find("admin-tabs").unwrap()..];
    let tabs = &tabs[..tabs.find("</nav>").unwrap()];
    assert_eq!(tabs.matches("aria-current").count(), 1, "{tabs}");
}

#[tokio::test]
async fn nav_is_rendered_for_sidebar_and_menu_with_one_current_item_each() {
    let app = app().await;
    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("ada@example.org", &secret).await;
    let docs = b.get("/documents").await;
    assert_eq!(docs.status, StatusCode::OK);
    assert!(
        docs.body
            .contains("<a class=\"skip-link\" href=\"#main\">Skip to main content</a>")
    );
    assert!(docs.body.contains("id=\"main\""));
    assert!(docs.body.contains("<div class=\"sidebar-nav\">"));
    assert!(docs.body.contains("<nav aria-label=\"Main\">"));
    assert!(docs.body.contains("<nav aria-label=\"Main (menu)\">"));
    // The menu copy starts closed.
    assert!(!docs.body.contains("<details class=\"menu-toggle\" open"));
    // Documents is the current page in both copies, and nothing else is
    // except the list's own "All" status tab.
    assert_eq!(docs.body.matches("aria-current=\"page\"").count(), 3);
    assert_eq!(
        docs.body
            .matches("<a href=\"/documents\" aria-current=\"page\">")
            .count(),
        2
    );
    // Sign out stays a POST form with the CSRF token.
    assert!(
        docs.body
            .contains("<form method=\"post\" action=\"/logout\">")
    );
    assert!(!docs.csrf().is_empty());
}

#[tokio::test]
async fn user_block_shows_department_and_new_document_follows_create_access() {
    let app = app().await;
    let db = &app.core.db;
    let ops = directory::create_department(db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let hr = directory::create_department(db, &system(), "HR", "Human Resources")
        .await
        .unwrap();
    let parks = directory::create_department(db, &system(), "PARKS", "Parks")
        .await
        .unwrap();
    acl::grant(
        db,
        &system(),
        Subject::Department(ops),
        Scope::Department(ops),
        Permission::Create,
        None,
        "t",
    )
    .await
    .unwrap();

    // One department where they may create: its name, and the button.
    let (_, s1) = user(&app.core, "one@example.org", Some(ops), None).await;
    let mut one = Browser::new(&app);
    one.login("one@example.org", &s1).await;
    let home = one.get("/").await;
    assert!(
        home.body.contains("<span class=\"dept\">Operations</span>"),
        "{}",
        home.body
    );
    assert!(
        home.body
            .contains("<span class=\"initials\" aria-hidden=\"true\">ON</span>")
    );
    assert!(home.body.contains("href=\"/documents/new\""));

    // Two departments, neither allowing creation: a count, and no button.
    let (two, s2) = user(&app.core, "two@example.org", Some(hr), None).await;
    directory::add_department_member(db, &system(), two, parks, false)
        .await
        .unwrap();
    let mut b2 = Browser::new(&app);
    b2.login("two@example.org", &s2).await;
    let home = b2.get("/").await;
    assert!(
        home.body
            .contains("<span class=\"dept\">2 departments</span>")
    );
    assert!(!home.body.contains("href=\"/documents/new\""));

    // No department: no department line.
    let (_, s3) = user(&app.core, "none@example.org", None, None).await;
    let mut b3 = Browser::new(&app);
    b3.login("none@example.org", &s3).await;
    let home = b3.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(!home.body.contains("class=\"dept\""));
    assert!(!home.body.contains("href=\"/documents/new\""));
}

#[tokio::test]
async fn branding_is_cached_until_reloaded_and_reaches_the_shell() {
    let (app, state) = app_and_state().await;
    let mut b = Browser::new(&app);

    // Load at startup, as the server does, so no background refresh runs
    // during this test: configured name, default accent.
    state.reload_branding().await;
    let css = b.get("/theme.css").await;
    assert!(css.body.contains("--accent: #B45309;"), "{}", css.body);
    let before = state.branding();
    assert_eq!(before.org_name, "Test Org PDMS");
    assert_eq!(before.seal_version, None);
    assert_eq!(before.version.len(), 12);

    branding::update(&app.core, &system(), "Example County", "#1F3A5F")
        .await
        .unwrap();
    branding::set_seal(&app.core, &system(), &png(64, 64))
        .await
        .unwrap();

    // Inside the refresh interval the cached values are still served.
    assert!(
        b.get("/theme.css")
            .await
            .body
            .contains("--accent: #B45309;")
    );

    state.reload_branding().await;
    let after = state.branding();
    assert_eq!(after.org_name, "Example County");
    assert_eq!(after.theme.accent, "#1F3A5F");
    assert_ne!(after.version, before.version);
    let seal_v = after.seal_version.clone().expect("seal version");
    assert_eq!(seal_v.len(), 12);
    assert!(
        b.get("/theme.css")
            .await
            .body
            .contains("--accent: #1F3A5F;")
    );

    let seal = b.get(&format!("/branding/seal?v={seal_v}")).await;
    assert_eq!(seal.status, StatusCode::OK);
    assert_eq!(header(&seal, "content-type"), "image/png");
    assert_eq!(header(&seal, "x-content-type-options"), "nosniff");
    assert_eq!(
        header(&seal, "content-disposition"),
        "inline; filename=\"seal.png\""
    );
    assert!(header(&seal, "cache-control").contains("immutable"));

    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    b.login("ada@example.org", &secret).await;
    let home = b.get("/").await;
    assert!(
        home.body
            .contains(&format!("<img src=\"/branding/seal?v={seal_v}\"")),
        "{}",
        home.body
    );
    assert!(!home.body.contains("class=\"mark\""));
    assert!(
        home.body
            .contains(&format!("href=\"/theme.css?v={}\"", after.version))
    );
    assert!(home.body.contains("– Example County</title>"));
    assert!(
        home.body
            .contains("<span class=\"org\">Example County</span>")
    );
}

#[tokio::test]
async fn first_page_request_starts_a_background_refresh_but_probes_do_not() {
    let (app, state) = app_and_state().await;
    branding::update(&app.core, &system(), "Example County", "#1F3A5F")
        .await
        .unwrap();
    let mut b = Browser::new(&app);
    assert!(state.branding().loaded_at.is_none());

    for path in ["/healthz", "/readyz", "/metrics", "/static/app.css"] {
        b.get(path).await;
        assert!(
            state.branding().loaded_at.is_none(),
            "{path} must not refresh branding"
        );
    }

    // The middleware claims the refresh on the first page request...
    assert_eq!(b.get("/theme.css").await.status, StatusCode::OK);
    assert!(state.branding().loaded_at.is_some());

    // ...and the background reload swaps in the stored values.
    let mut waited = 0;
    while state.branding().org_name != "Example County" {
        assert!(waited < 100, "background refresh never finished");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited += 1;
    }
    assert_eq!(state.branding().theme.accent, "#1F3A5F");
    assert!(
        b.get("/theme.css")
            .await
            .body
            .contains("--accent: #1F3A5F;")
    );
}
