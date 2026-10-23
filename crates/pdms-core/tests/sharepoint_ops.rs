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
mod mock_graph;

use common::*;
use mock_graph::{MockGraph, certificate};
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::config::StorageBackend;
use pdms_core::graph::GraphError;
use pdms_core::model::UserStatus;
use pdms_core::sharepoint::{access, status};
use pdms_core::{Core, CoreError, acl, directory};
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

/// A `Core` that stores document files in the mock SharePoint (as in P3's tests).
async fn sp_core(sp: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut c = (*core.config).clone();
    c.graph = sp.config(certificate());
    c.storage.backend = StorageBackend::SharePoint;
    c.sharepoint.site_url = mock_graph::SITE_URL.into();
    c.sharepoint.upload_chunk_bytes = 327_680;
    (Core::new(core.db.clone(), c).unwrap(), db)
}

/// A department whose members may create and edit documents.
async fn department(core: &Core, code: &str, name: &str) -> Uuid {
    let dept = directory::create_department(&core.db, &system(), code, name)
        .await
        .unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(
            &core.db,
            &system(),
            Subject::Department(dept),
            Scope::Department(dept),
            p,
            None,
            "test",
        )
        .await
        .unwrap();
    }
    dept
}

async fn last_error(core: &Core, name: &str) -> Option<String> {
    sqlx::query_scalar("SELECT last_error FROM sync_cursors WHERE name = $1")
        .bind(name)
        .fetch_optional(&core.db)
        .await
        .unwrap()
        .flatten()
}

#[tokio::test]
async fn department_members_get_read_access_on_their_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let bob = entra_user(&core, "bob@county.example", Some(roads)).await;
    // Local break-glass accounts never get SharePoint access.
    directory::create_user(
        &core.db,
        &system(),
        "breakglass@county.example",
        "breakglass@county.example",
        Some(roads),
    )
    .await
    .unwrap();

    let r = access::reconcile(&core).await.unwrap();
    assert_eq!((r.granted, r.revoked, r.errors.len()), (2, 0, 0), "{r:?}");
    let folder = sp.item("Departments/Road and Bridge").unwrap().id;
    assert_eq!(
        sp.readers(&folder),
        vec!["alice@county.example", "bob@county.example"]
    );
    assert!(sp.invites().iter().any(|q| {
        q == r#"invite alice@county.example requireSignIn=true sendInvitation=false roles=["read"]"#
    }));
    assert_eq!(last_error(&core, status::RECONCILE_CURSOR).await, None);

    // A second run changes nothing.
    let again = access::reconcile(&core).await.unwrap();
    assert_eq!((again.granted, again.revoked), (0, 0));

    // Bob leaves the department; Alice leaves the county.
    directory::remove_department_member(&core.db, &system(), bob, roads)
        .await
        .unwrap();
    directory::set_user_status(&core.db, &system(), alice, UserStatus::Departed)
        .await
        .unwrap();
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.revoked, 2);
    assert!(sp.readers(&folder).is_empty());

    // Access someone added by hand is reported, never removed. What the folder
    // got from the site when its inheritance broke is not reported.
    let area = sp.item("Departments").unwrap().id;
    sp.add_group_permission(&area, "PDMS Test Members");
    sp.add_group_permission(&folder, "PDMS Test Members");
    sp.add_group_permission(&folder, "Road and Bridge Editors");
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.foreign, 1, "{r:?}");
    let foreign: Vec<String> =
        sqlx::query_scalar("SELECT granted_to FROM sharepoint_foreign_grants")
            .fetch_all(&core.db)
            .await
            .unwrap();
    assert_eq!(foreign, vec!["Road and Bridge Editors"]);
    let acts = audit_actions(&core).await;
    for a in ["sharepoint.access_granted", "sharepoint.access_revoked"] {
        assert!(acts.contains(&a.to_owned()), "{a}");
    }
}

#[tokio::test]
async fn one_unresolvable_person_does_not_block_the_rest() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    entra_user(&core, "alice@county.example", Some(roads)).await;
    entra_user(&core, "ghost@nobody.example", Some(roads)).await;
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.granted, 1);
    assert_eq!(r.errors.len(), 1);
    assert!(r.errors[0].contains("ghost@nobody.example"), "{r:?}");
    assert!(
        last_error(&core, status::RECONCILE_CURSOR)
            .await
            .unwrap()
            .contains("ghost@nobody.example")
    );
}

#[tokio::test]
async fn folder_access_reports_sign_in_failures() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let boss = admin(&core, "admin@county.example", None).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    entra_user(&core, "alice@county.example", Some(roads)).await;
    sp.deny(true);
    assert!(access::reconcile(&core).await.is_err());
    assert_eq!(
        in_app_notices(&core).await,
        vec![(boss, "PDMS cannot sign in to Microsoft Graph".to_owned())]
    );
    sp.deny(false);
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.granted, 1);
    assert_eq!(last_error(&core, status::RECONCILE_CURSOR).await, None);
}

#[test]
fn plan_adds_and_removes_by_person() {
    let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let desired = [(a, "a@x".to_owned()), (b, "b@x".to_owned())]
        .into_iter()
        .collect();
    let granted = [(b, "perm-b".to_owned()), (c, "perm-c".to_owned())]
        .into_iter()
        .collect();
    let (add, remove) = access::plan(&desired, &granted);
    assert_eq!(add, vec![(a, "a@x".to_owned())]);
    assert_eq!(remove, vec![(c, "perm-c".to_owned())]);
}

#[test]
fn changes_in_one_minute_share_one_run_just_after_it() {
    assert_eq!(
        access::reconcile_slot(120),
        ("sharepoint.reconcile_access:2".to_owned(), 185)
    );
    assert_eq!(
        access::reconcile_slot(179).0,
        "sharepoint.reconcile_access:2"
    );
    assert_eq!(
        access::reconcile_slot(180).0,
        "sharepoint.reconcile_access:3"
    );
}

async fn queued_runs(core: &Core) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'sharepoint.reconcile_access' AND status = 'pending'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn membership_changes_queue_one_folder_access_run() {
    let (core, _db) = core().await;
    let roads = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let parks = directory::create_department(&core.db, &system(), "PK", "Parks")
        .await
        .unwrap();
    assert_eq!(
        queued_runs(&core).await,
        0,
        "a new, empty department needs no run"
    );
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    directory::add_department_member(&core.db, &system(), alice, parks, false)
        .await
        .unwrap();
    directory::remove_department_member(&core.db, &system(), alice, roads)
        .await
        .unwrap();
    directory::set_user_status(&core.db, &system(), alice, UserStatus::Departed)
        .await
        .unwrap();
    let n = queued_runs(&core).await;
    assert!(
        (1..=2).contains(&n),
        "changes in the same minute share one run; got {n}"
    );
    let first: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT min(run_at) FROM jobs WHERE kind = 'sharepoint.reconcile_access'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert!(
        first > chrono::Utc::now(),
        "runs after the minute of the change ends"
    );
    // Without SharePoint storage the run does nothing.
    assert_eq!(
        access::reconcile(&core).await.unwrap(),
        access::ReconcileReport::default()
    );
}
