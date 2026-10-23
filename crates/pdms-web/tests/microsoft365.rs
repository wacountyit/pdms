// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Administration → Microsoft 365 → Connection (S1 Phase 2).

mod support;

use axum::http::StatusCode;
use support::*;

const TENANT: &str = "01234567-89ab-cdef-0123-456789abcdef";
const CLIENT: &str = "11234567-89ab-cdef-0123-456789abcdef";

#[tokio::test]
async fn administrators_set_up_the_connection_in_the_browser() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/microsoft-365").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Not connected yet"));
    assert!(
        page.body.contains(r#"href="/admin/microsoft-365""#),
        "in the admin menu"
    );

    // The IDs alone are refused, with the reason.
    let csrf = page.csrf();
    let r = b
        .post(
            "/admin/microsoft-365",
            &[
                ("_csrf", &csrf),
                ("tenant_id", TENANT),
                ("client_id", CLIENT),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        b.get("/admin/microsoft-365")
            .await
            .body
            .contains("Generate a certificate first")
    );

    // A badly formed ID is refused too.
    b.post(
        "/admin/microsoft-365",
        &[
            ("_csrf", &csrf),
            ("tenant_id", "not-a-guid"),
            ("client_id", CLIENT),
        ],
    )
    .await;
    assert!(
        b.get("/admin/microsoft-365")
            .await
            .body
            .contains("look like 01234567")
    );

    b.post("/admin/microsoft-365/certificates", &[("_csrf", &csrf)])
        .await;
    let page = b.get("/admin/microsoft-365").await;
    assert!(page.body.contains("Certificate made"));
    assert!(!page.body.contains("PRIVATE KEY"));
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM graph_certificates")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    let crt = b
        .get(&format!("/admin/microsoft-365/certificates/{id}.crt"))
        .await;
    assert_eq!(crt.status, StatusCode::OK);
    assert!(crt.body.contains("BEGIN CERTIFICATE"));
    assert!(
        !crt.body.contains("PRIVATE KEY"),
        "only the public certificate"
    );

    let r = b
        .post(
            "/admin/microsoft-365",
            &[
                ("_csrf", &csrf),
                ("tenant_id", TENANT),
                ("client_id", CLIENT),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(app.core.graph().is_some(), "connected without a restart");
    let page = b.get("/admin/microsoft-365").await;
    assert!(
        page.body.contains("Connected: PDMS signs in"),
        "{}",
        page.body
    );
    assert_eq!(page.body.matches("<h1").count(), 1);
}

#[tokio::test]
async fn the_connection_page_is_for_settings_managers() {
    let app = app().await;
    let (_, secret) = user(&app.core, "clerk@x.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("clerk@x.org", &secret).await;
    assert_eq!(
        b.get("/admin/microsoft-365").await.status,
        StatusCode::FORBIDDEN
    );
    let id = uuid::Uuid::now_v7();
    assert_eq!(
        b.get(&format!("/admin/microsoft-365/certificates/{id}.crt"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}
