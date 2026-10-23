// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! SharePoint folder access, change detection, resolution, and integrity (P4),
//! against P3's in-process mock of Microsoft Graph.

mod common;

use common::*;
use pdms_core::graph::GraphError;
use pdms_core::sharepoint::status;
use pdms_core::{Core, CoreError, directory};
use uuid::Uuid;

/// A person synced from Entra (only they get SharePoint folder access).
async fn entra_user(core: &Core, email: &str, dept: Option<Uuid>) -> Uuid {
    let id = directory::create_user(&core.db, &system(), email, email, dept)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET source = 'entra' WHERE id = $1")
        .bind(id)
        .execute(&core.db)
        .await
        .unwrap();
    id
}

/// An Entra person with the org-wide administrator role.
async fn admin(core: &Core, email: &str, dept: Option<Uuid>) -> Uuid {
    let id = entra_user(core, email, dept).await;
    let role = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), id, role, None)
        .await
        .unwrap();
    id
}

async fn in_app_notices(core: &Core) -> Vec<(Uuid, String)> {
    sqlx::query_as(
        "SELECT dl.user_id, n.subject FROM notification_deliveries dl
           JOIN notifications n ON n.id = dl.notification_id
          WHERE dl.channel = 'in_app' ORDER BY dl.created_at",
    )
    .fetch_all(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn graph_sign_in_failures_reach_admins_once_an_hour() {
    let (core, _db) = core().await;
    let boss = admin(&core, "admin@county.example", None).await;
    entra_user(&core, "clerk@county.example", None).await;
    let denied = status::RunError::Graph(GraphError::Auth(
        "InvalidAuthenticationToken: AADSTS700027 certificate expired".into(),
    ));
    status::report_failure(&core, status::DELTA_CURSOR, &denied)
        .await
        .unwrap();
    status::report_failure(&core, status::RECONCILE_CURSOR, &denied)
        .await
        .unwrap();
    assert_eq!(
        in_app_notices(&core).await,
        vec![(boss, "PDMS cannot sign in to Microsoft Graph".to_owned())],
        "one notice an hour, only to administrators"
    );
    let err: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM sync_cursors WHERE name = 'sharepoint.delta'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert!(err.unwrap().contains("AADSTS700027"));
    // Other failures are recorded but tell nobody.
    let other = status::RunError::Core(CoreError::internal("disk full"));
    status::report_failure(&core, status::DELTA_CURSOR, &other)
        .await
        .unwrap();
    assert_eq!(in_app_notices(&core).await.len(), 1);
}
