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
        page.body.contains("Microsoft Graph is not configured"),
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
    let thumbprint = app.core.graph.as_ref().unwrap().thumbprint().to_owned();
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
            "3 created, 1 updated, 0 departed; of the people it read, 2 had no mapped department"
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
