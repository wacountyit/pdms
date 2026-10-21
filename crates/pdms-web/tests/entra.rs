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
