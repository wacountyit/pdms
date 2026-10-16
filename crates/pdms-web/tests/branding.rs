// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use axum::http::StatusCode;
use support::*;

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    v
}

async fn admin(app: &App) -> Browser {
    let (_, secret) = user(&app.core, "admin@example.test", None, Some("admin")).await;
    let mut b = Browser::new(app);
    b.login("admin@example.test", &secret).await;
    b
}

fn version(body: &str) -> String {
    let re = regex::Regex::new(r#"/theme\.css\?v=([^"]+)"#).unwrap();
    re.captures(body)
        .map(|c| c[1].to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn branding_page_requires_settings_capability() {
    let app = app().await;
    let (_, secret) = user(&app.core, "plain@example.test", None, None).await;
    let mut p = Browser::new(&app);
    p.login("plain@example.test", &secret).await;
    let r = p.get("/admin/branding").await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.body.to_lowercase().contains("access"),
        "{}",
        r.status
    );
    assert!(!r.body.contains("Accent color"));

    let mut b = admin(&app).await;
    let r = b.get("/admin/branding").await;
    assert_eq!(r.status, StatusCode::OK);
    for s in ["Branding", "County name", "Accent color", "Seal"] {
        assert!(r.body.contains(s), "missing {s}");
    }
    assert!(!r.body.contains("Remove seal"));
}

#[tokio::test]
async fn branding_page_refuses_light_accent() {
    let app = app().await;
    let mut b = admin(&app).await;
    let csrf = b.get("/admin/branding").await.csrf();
    let r = b
        .post(
            "/admin/branding",
            &[
                ("org_name", "[County Name]"),
                ("accent_text", "#F5D76E"),
                ("_csrf", &csrf),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = b.get(r.location()).await;
    assert!(page.body.contains("4.5:1"), "{}", page.body);
    let css = b.get("/theme.css").await;
    assert!(css.body.contains("#B45309"));
}

#[tokio::test]
async fn saving_branding_changes_name_theme_and_audit() {
    let app = app().await;
    let mut b = admin(&app).await;
    let page = b.get("/admin/branding").await;
    let csrf = page.csrf();
    let v0 = version(&page.body);
    // Only the color input (no accent_text) must also work.
    let r = b
        .post(
            "/admin/branding",
            &[
                ("org_name", "[County Name]"),
                ("accent", "#1F3A5F"),
                ("accent_text", ""),
                ("_csrf", &csrf),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let home = b.get("/").await;
    assert!(home.body.contains("– [County Name]"), "{}", home.body);
    assert_ne!(version(&home.body), v0);
    assert!(
        b.get("/theme.css")
            .await
            .body
            .contains("--accent: #1F3A5F;")
    );
    assert!(
        actions(&app.core)
            .await
            .contains(&"branding.updated".to_owned())
    );

    let csrf = b.get("/admin/branding").await.csrf();
    let r = b
        .post(
            "/admin/branding",
            &[
                ("org_name", ""),
                ("accent", "#000000"),
                ("accent_text", "#2A4B7C"),
                ("_csrf", &csrf),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        b.get("/theme.css")
            .await
            .body
            .contains("--accent: #2A4B7C;")
    );
}

#[tokio::test]
async fn seal_upload_round_trip_and_headers() {
    let app = app().await;
    let mut b = admin(&app).await;
    let csrf = b.get("/admin/branding").await.csrf();
    let data = png(128, 128);
    let r = b
        .multipart(
            "/admin/branding/seal",
            &[("_csrf", &csrf)],
            Some(("seal.png", &data)),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let s = b.get("/branding/seal").await;
    assert_eq!(s.status, StatusCode::OK);
    assert_eq!(s.headers["content-type"], "image/png");
    assert_eq!(s.headers["x-content-type-options"], "nosniff");
    assert_eq!(
        s.headers["content-disposition"],
        "inline; filename=\"seal.png\""
    );
    let page = b.get("/admin/branding").await;
    assert!(page.body.contains("/branding/seal?v="));
    assert!(page.body.contains("Remove seal"));
    assert!(b.get("/").await.body.contains("/branding/seal?v="));

    let csrf = page.csrf();
    let r = b
        .multipart(
            "/admin/branding/seal",
            &[("_csrf", &csrf)],
            Some((
                "seal.png",
                b"<svg xmlns='http://www.w3.org/2000/svg'></svg>",
            )),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        b.get(r.location())
            .await
            .body
            .contains("must be a PNG or JPEG")
    );
    assert_eq!(b.get("/branding/seal").await.status, StatusCode::OK);

    // No file at all.
    let r = b
        .multipart("/admin/branding/seal", &[("_csrf", &csrf)], None)
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(b.get(r.location()).await.body.contains("Choose an image"));

    // Remove.
    let csrf = b.get("/admin/branding").await.csrf();
    let r = b
        .post("/admin/branding/seal/remove", &[("_csrf", &csrf)])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(b.get("/branding/seal").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reset_restores_defaults() {
    let app = app().await;
    let mut b = admin(&app).await;
    let csrf = b.get("/admin/branding").await.csrf();
    b.post(
        "/admin/branding",
        &[
            ("org_name", "[County Name]"),
            ("accent_text", "#1F3A5F"),
            ("_csrf", &csrf),
        ],
    )
    .await;
    let data = png(128, 128);
    b.multipart(
        "/admin/branding/seal",
        &[("_csrf", &csrf)],
        Some(("seal.png", &data)),
    )
    .await;
    assert_eq!(b.get("/branding/seal").await.status, StatusCode::OK);

    let confirm = b.get("/admin/branding/reset").await;
    assert_eq!(confirm.status, StatusCode::OK);
    assert!(confirm.body.contains("Reset to defaults"));
    let r = b
        .post("/admin/branding/reset", &[("_csrf", &confirm.csrf())])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(b.get("/theme.css").await.body.contains("#B45309"));
    assert_eq!(b.get("/branding/seal").await.status, StatusCode::NOT_FOUND);
    assert!(b.get("/").await.body.contains("Test Org PDMS"));
    assert!(
        actions(&app.core)
            .await
            .contains(&"branding.reset".to_owned())
    );
}
