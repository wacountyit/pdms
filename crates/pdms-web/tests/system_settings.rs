// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Administration → Email, → Virus scanning and the OCR Settings panel (S1
//! Phase 5). The tests against real services are in the core tests.

mod support;

use axum::http::StatusCode;
use support::*;

async fn admin(app: &App) -> Browser {
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(app);
    b.login("admin@x.org", &secret).await;
    b
}

#[tokio::test]
async fn administrators_set_email_up_on_the_page() {
    let app = app().await;
    let mut b = admin(&app).await;
    let page = b.get("/admin/email").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Email is off."));
    assert!(
        page.body.contains(r#"href="/admin/email""#),
        "in the admin menu"
    );
    assert!(
        page.body.contains(r#"value="admin@x.org""#),
        "test address defaults to mine"
    );
    let csrf = page.csrf();
    let fields = |extra: &[(&'static str, &'static str)]| {
        let mut f = vec![
            ("_csrf", csrf.clone()),
            ("on", "true".to_owned()),
            ("host", "127.0.0.1".to_owned()),
            ("port", "9".to_owned()),
            ("security", "none".to_owned()),
            ("username", "pdms@example.org".to_owned()),
            ("password", "smtp-pass-31".to_owned()),
            ("from", "Records System <pdms@example.org>".to_owned()),
        ];
        f.extend(extra.iter().map(|(k, v)| (*k, (*v).to_owned())));
        f
    };

    let f = fields(&[("test_to", "clerk@example.org")]);
    let f: Vec<(&str, &str)> = f.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = b.post("/admin/email/test", &f).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("wasn"), "{}", r.body);
    assert!(
        !r.body.contains("smtp-pass-31"),
        "the password is never sent back"
    );
    assert!(!app.core.mailer().enabled(), "a test saves nothing");

    let f = fields(&[]);
    let f: Vec<(&str, &str)> = f.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = b.post("/admin/email", &f).await;
    assert!(r.status.is_redirection());
    assert!(app.core.mailer().enabled(), "applied at once");
    let page = b.get("/admin/email").await;
    assert!(page.body.contains("Email is on"), "{}", page.body);
    assert!(page.body.contains("A password is saved"));
    assert!(!page.body.contains("smtp-pass-31"));
}

#[tokio::test]
async fn administrators_turn_virus_scanning_and_ocr_on_and_off() {
    let app = app().await;
    let mut b = admin(&app).await;
    let page = b.get("/admin/virus-scanning").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Uploads are not scanned."));
    let csrf = page.csrf();
    b.post(
        "/admin/virus-scanning",
        &[("_csrf", &csrf), ("on", "true"), ("required", "true")],
    )
    .await;
    assert!(app.core.scanner().is_some());
    let r = b
        .post("/admin/virus-scanning/test", &[("_csrf", &csrf)])
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("--configure clamav"), "{}", r.body);

    let page = b.get("/admin/ocr").await;
    assert!(page.body.contains(r#"id="settings-h""#));
    b.post(
        "/admin/ocr",
        &[
            ("_csrf", &csrf),
            ("text_extraction", "true"),
            ("ocr", "true"),
        ],
    )
    .await;
    assert!(app.core.ocr().is_some());
    assert!(!app.core.config().ocr.suggest_dates);
    let r = b.post("/admin/ocr/test", &[("_csrf", &csrf)]).await;
    assert!(r.body.contains("--configure ocr"), "{}", r.body);
}

#[tokio::test]
async fn settings_from_the_configuration_file_show_locked() {
    let mut c = config();
    c.storage.clamav_addr = "clamav:3310".into();
    c.locked.insert("storage.clamav_addr".into());
    c.email.smtp_host = "mail.example.org".into();
    c.locked.insert("email.smtp_host".into());
    let app = app_with(c).await;
    let mut b = admin(&app).await;
    let page = b.get("/admin/virus-scanning").await;
    assert!(
        page.body
            .contains(r#"name="on" value="true" checked disabled"#),
        "{}",
        page.body
    );
    let page = b.get("/admin/email").await;
    assert!(page.body.contains(r#"aria-describedby="em-host-locked""#));
}
