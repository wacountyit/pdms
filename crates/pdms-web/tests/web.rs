#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! End-to-end HTTP tests through the full middleware stack.

mod support;

use axum::http::StatusCode;
use pdms_core::directory;
use pdms_core::model::UserStatus;
use support::*;

#[tokio::test]
async fn login_sets_session_and_lands_on_home() {
    let app = app().await;
    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    let mut b = Browser::new(&app);
    let r = b.login("ada@example.org", &secret).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location(), "/");
    assert!(b.has_session());
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("Test Org PDMS"));
    assert!(home.body.contains("Welcome, ada@example.org"));
}

#[tokio::test]
async fn unauthenticated_requests_redirect_to_login() {
    let app = app().await;
    let mut b = Browser::new(&app);
    for path in ["/", "/admin/users", "/audit", "/audit/export.csv"] {
        let r = b.get(path).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{path}");
        assert!(
            r.location().starts_with("/login?next="),
            "{path}: {}",
            r.location()
        );
    }
    let r = b.get("/admin/departments").await;
    assert_eq!(r.location(), "/login?next=%2Fadmin%2Fdepartments");
}

#[tokio::test]
async fn read_only_default_user_is_denied_everything_else() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "HR", "Human Resources")
        .await
        .unwrap();
    let (_, secret) = user(&app.core, "new@example.org", Some(dept), None).await;
    let mut b = Browser::new(&app);
    b.login("new@example.org", &secret).await;
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("Human Resources"));
    let csrf = home.csrf();

    for path in [
        "/admin/users".to_owned(),
        "/admin/departments".to_owned(),
        format!("/admin/departments/{dept}"),
        "/admin/grants".to_owned(),
        "/audit".to_owned(),
        "/audit/export.csv".to_owned(),
        "/audit/export.json".to_owned(),
    ] {
        assert_eq!(
            b.get(&path).await.status,
            StatusCode::FORBIDDEN,
            "GET {path}"
        );
    }
    for (path, form) in [
        ("/admin/departments", vec![("code", "X"), ("name", "X")]),
        (
            "/admin/grants",
            vec![
                ("subject", "user:00000000-0000-0000-0000-000000000000"),
                ("scope", "global"),
                ("permission", "view"),
                ("reason", "x"),
            ],
        ),
        ("/audit/verify", vec![]),
    ] {
        let mut f = form.clone();
        f.push(("_csrf", &csrf));
        assert_eq!(
            b.post(path, &f).await.status,
            StatusCode::FORBIDDEN,
            "POST {path}"
        );
    }
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'access.denied' AND outcome = 'denied'",
    )
    .fetch_one(&app.core.db)
    .await
    .unwrap();
    assert_eq!(denied, 10, "every denial is audited");
}

#[tokio::test]
async fn role_revocation_applies_to_the_next_request() {
    let app = app().await;
    let (uid, secret) = user(&app.core, "aud@example.org", None, Some("auditor")).await;
    let mut b = Browser::new(&app);
    b.login("aud@example.org", &secret).await;
    assert_eq!(b.get("/audit").await.status, StatusCode::OK);
    assert_eq!(b.get("/audit/export.csv").await.status, StatusCode::OK);

    let assignment = directory::get_user(&app.core.db, uid).await.unwrap().roles[0].id;
    directory::revoke_role(&app.core.db, &system(), assignment)
        .await
        .unwrap();
    assert_eq!(b.get("/audit").await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        b.get("/audit/export.csv").await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn disabling_a_user_ends_their_session() {
    let app = app().await;
    let (uid, secret) = user(&app.core, "gone@example.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("gone@example.org", &secret).await;
    assert_eq!(b.get("/").await.status, StatusCode::OK);
    directory::set_user_status(&app.core.db, &system(), uid, UserStatus::Departed)
        .await
        .unwrap();
    assert_eq!(b.get("/").await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn state_changes_require_csrf_token_and_same_origin() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@example.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@example.org", &secret).await;
    let csrf = b.get("/admin/departments").await.csrf();
    let form = [("code", "OPS"), ("name", "Operations")];

    assert_eq!(
        b.post("/admin/departments", &form).await.status,
        StatusCode::FORBIDDEN,
        "missing token"
    );
    let wrong = [("code", "OPS"), ("name", "Operations"), ("_csrf", "AAAA")];
    assert_eq!(
        b.post("/admin/departments", &wrong).await.status,
        StatusCode::FORBIDDEN,
        "wrong token"
    );
    let good = [
        ("code", "OPS"),
        ("name", "Operations"),
        ("_csrf", csrf.as_str()),
    ];
    let r = b
        .post_with(
            "/admin/departments",
            &good,
            &[("origin", "https://evil.example")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "cross-origin");
    let r = b
        .post_with(
            "/admin/departments",
            &[("code", "OPS"), ("name", "Operations")],
            &[("x-csrf-token", &csrf)],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "header token (HTMX) accepted"
    );
    let r = b
        .post(
            "/admin/departments",
            &[("code", "HR"), ("name", "HR"), ("_csrf", &csrf)],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "form token accepted");
    assert_eq!(
        directory::list_departments(&app.core.db)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn login_form_requires_its_own_csrf_token() {
    let app = app().await;
    let (_, secret) = user(&app.core, "x@example.org", None, None).await;
    let mut b = Browser::new(&app);
    let code = pdms_core::auth::totp::current_code(&secret, chrono::Utc::now().timestamp() as u64)
        .unwrap();
    let r = b
        .post(
            "/login/local",
            &[
                ("_csrf", "forged"),
                ("email", "x@example.org"),
                ("password", PW),
                ("code", &code),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(!b.has_session());
}

#[tokio::test]
async fn login_attempts_are_rate_limited() {
    let mut cfg = config();
    cfg.auth.login_attempts_per_minute = 2;
    let app = app_with(cfg).await;
    let mut b = Browser::new(&app);
    let csrf = b.get("/login").await.csrf();
    let form = [
        ("_csrf", csrf.as_str()),
        ("email", "nobody@example.org"),
        ("password", "whatever whatever"),
        ("code", "000000"),
    ];
    assert_eq!(
        b.post("/login/local", &form).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        b.post("/login/local", &form).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        b.post("/login/local", &form).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn pages_carry_security_headers() {
    let app = app().await;
    let mut b = Browser::new(&app);
    let r = b.get("/login").await;
    let h = |n: &str| {
        r.headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    assert!(h("content-security-policy").contains("script-src 'self'"));
    assert!(h("content-security-policy").contains("frame-ancestors 'none'"));
    assert_eq!(h("x-content-type-options"), "nosniff");
    assert_eq!(h("cache-control"), "no-store");
    assert!(!h("x-request-id").is_empty());
    assert!(!r.body.contains("<script>"), "no inline scripts");
}

#[tokio::test]
async fn admin_workflow_is_audited_with_diffs() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@example.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@example.org", &secret).await;
    let csrf = b.get("/admin/departments").await.csrf();
    let r = b
        .post(
            "/admin/departments",
            &[("code", "OPS"), ("name", "Operations"), ("_csrf", &csrf)],
        )
        .await;
    let dept_page = r.location().to_owned();
    let r = b
        .post(
            &dept_page,
            &[
                ("name", "Operations and Maintenance"),
                ("is_active", "true"),
                ("_csrf", &csrf),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = b.get(&dept_page).await;
    assert!(
        page.body.contains("Department saved."),
        "flash shown after redirect"
    );
    assert!(
        !b.get(&dept_page).await.body.contains("Department saved."),
        "flash shown once"
    );

    let acts = actions(&app.core).await;
    for a in [
        "auth.login",
        "department.created",
        "acl.granted",
        "department.updated",
    ] {
        assert!(acts.contains(&a.to_owned()), "missing {a}: {acts:?}");
    }
    let changes: serde_json::Value =
        sqlx::query_scalar("SELECT changes FROM audit_log WHERE action = 'department.updated'")
            .fetch_one(&app.core.db)
            .await
            .unwrap();
    assert_eq!(
        changes,
        serde_json::json!([{"field": "name", "before": "Operations", "after": "Operations and Maintenance"}])
    );
    assert!(pdms_audit::verify(&app.core.db).await.unwrap().is_intact());
}

#[tokio::test]
async fn audit_rows_cannot_be_changed_through_the_app() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@example.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@example.org", &secret).await;
    let csrf = b.get("/").await.csrf();
    for path in [
        "/audit/1",
        "/audit/1/delete",
        "/audit/delete",
        "/admin/audit",
    ] {
        let r = b.post(path, &[("_csrf", &csrf)]).await;
        assert!(
            matches!(
                r.status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{path}: {}",
            r.status
        );
    }
    let r = b
        .send(
            axum::http::Request::delete("/audit")
                .header("x-csrf-token", &csrf)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn health_endpoints() {
    let app = app().await;
    let mut b = Browser::new(&app);
    assert_eq!(b.get("/healthz").await.status, StatusCode::OK);
    assert_eq!(b.get("/readyz").await.status, StatusCode::OK);
    assert_eq!(b.get("/static/app.css").await.status, StatusCode::OK);
    assert_eq!(b.get("/static/nope.js").await.status, StatusCode::NOT_FOUND);
}

/// AGPL-3.0 section 13: every page offers the source and the license.
#[tokio::test]
async fn source_and_license_are_offered_to_every_user() {
    let app = app().await;
    let mut b = Browser::new(&app);
    let login = b.get("/login").await;
    assert!(login.body.contains(r#"href="/source""#) && login.body.contains(r#"href="/license""#));
    let lic = b.get("/license").await;
    assert_eq!(lic.status, StatusCode::OK);
    assert!(lic.body.contains("GNU AFFERO GENERAL PUBLIC LICENSE"));
    let src = b.get("/source").await;
    assert_eq!(src.status, StatusCode::OK);
    assert!(src.body.contains("have not published a download location"));

    let mut cfg = config();
    cfg.org.source_url = "https://github.com/example/pdms".into();
    let app = app_with(cfg).await;
    let r = Browser::new(&app).get("/source").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location(), "https://github.com/example/pdms");
}
