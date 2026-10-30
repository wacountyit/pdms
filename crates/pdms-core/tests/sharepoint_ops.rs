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
use pdms_authz::{Permission, Scope, Subject, Visibility};
use pdms_core::config::StorageBackend;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::graph::GraphError;
use pdms_core::model::UserStatus;
use pdms_core::sharepoint::{access, changes, resolve, status};
use pdms_core::{Core, CoreError, acl, directory, integrity};
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
    let mut c = (*core.config()).clone();
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

/// A document with one file in the mock SharePoint: (document, file version id, SharePoint item id).
async fn stored_document(
    core: &Core,
    owner: Uuid,
    dept: Uuid,
    title: &str,
    bytes: &[u8],
) -> (Uuid, Uuid, String) {
    let t = types::type_by_key(&core.db, "contract").await.unwrap().id;
    let id = documents::create(
        core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file: Some(upload(core, "agreement.pdf", bytes).await),
            note: String::new(),
            visibility: Visibility::Department,
        },
    )
    .await
    .unwrap();
    let (version, item): (Uuid, String) = sqlx::query_as(
        "SELECT document_version_id, item_id FROM sharepoint_files WHERE document_id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await
    .unwrap();
    (id, version, item)
}

async fn file_state(core: &Core, version: Uuid) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, changed_by FROM sharepoint_files WHERE document_version_id = $1")
        .bind(version)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn outside_changes_are_flagged_and_admins_told() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let boss = admin(&core, "admin@county.example", None).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (fuel, fuel_v1, fuel_item) =
        stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    let (_lease, lease_v1, lease_item) =
        stored_document(&core, alice, roads, "Office Lease", &pdf("lease")).await;

    // The first run reads everything; PDMS's own files and folders are not changes.
    let first = changes::detect_changes(&core).await.unwrap();
    assert_eq!(
        (first.modified, first.missing, first.unexpected),
        (0, 0, 0),
        "{first:?}"
    );

    sp.human_edit(&fuel_item, &pdf("fuel, edited by hand"), "Pat Clerk");
    sp.human_delete(&lease_item, "Pat Clerk");
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!((r.modified, r.missing), (1, 1), "{r:?}");
    assert_eq!(
        file_state(&core, fuel_v1).await,
        (
            "changed_outside".to_owned(),
            Some("Pat Clerk <pat.clerk@county.example>".to_owned())
        )
    );
    assert_eq!(file_state(&core, lease_v1).await.0, "missing");
    assert!(
        audit_actions(&core)
            .await
            .contains(&"sharepoint.changed_outside".to_owned())
    );
    assert_eq!(
        in_app_notices(&core).await,
        vec![(
            boss,
            "SharePoint: 2 file(s) changed outside PDMS".to_owned()
        )]
    );
    assert_eq!(
        status::document_flags(&core.db, fuel).await.unwrap(),
        vec![(1, "changed_outside".to_owned())]
    );

    // Nothing new happened: no new flags, no new notice.
    let quiet = changes::detect_changes(&core).await.unwrap();
    assert_eq!((quiet.modified, quiet.missing), (0, 0));
    assert_eq!(in_app_notices(&core).await.len(), 1);
    assert_eq!(last_error(&core, status::DELTA_CURSOR).await, None);
}

#[tokio::test]
async fn untracked_files_are_listed_after_ten_minutes_and_never_deleted() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();

    let stray = sp.human_upload(
        "Departments/Road and Bridge",
        "notes.docx",
        b"hand-made notes",
        "Pat Clerk",
    );
    sp.app_upload(
        "Departments/Road and Bridge",
        "unfinished.pdf",
        &pdf("orphan"),
    );
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!(r.unexpected, 2, "{r:?}");
    assert!(
        status::unexpected_files(&core.db).await.unwrap().is_empty(),
        "not shown for ten minutes"
    );

    sqlx::query(
        "UPDATE sharepoint_unexpected_items SET first_seen = now() - interval '11 minutes'",
    )
    .execute(&core.db)
    .await
    .unwrap();
    let mut seen: Vec<(String, String, bool)> = status::unexpected_files(&core.db)
        .await
        .unwrap()
        .into_iter()
        .map(|u| (u.path, u.created_by, u.by_pdms))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (
                "Departments/Road and Bridge/notes.docx".into(),
                "Pat Clerk <pat.clerk@county.example>".into(),
                false
            ),
            (
                "Departments/Road and Bridge/unfinished.pdf".into(),
                "PDMS".into(),
                true
            ),
        ]
    );
    assert!(
        sp.item("Departments/Road and Bridge/notes.docx").is_some(),
        "PDMS never deletes it"
    );

    // Deleted in SharePoint: forgotten.
    sp.human_delete(&stray, "Pat Clerk");
    changes::detect_changes(&core).await.unwrap();
    assert_eq!(status::unexpected_files(&core.db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_upload_in_progress_is_not_reported() {
    let sp = MockGraph::start().await;
    let (core, db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (_doc, _v, item) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    // The feed sees the file before its upload's transaction commits.
    sqlx::query("UPDATE sharepoint_files SET item_id = 'not-yet-' || item_id WHERE item_id = $1")
        .bind(&item)
        .execute(&db.owner)
        .await
        .unwrap();
    assert_eq!(changes::detect_changes(&core).await.unwrap().unexpected, 1);
    // The transaction commits.
    sqlx::query("UPDATE sharepoint_files SET item_id = $1 WHERE item_id = 'not-yet-' || $1")
        .bind(&item)
        .execute(&db.owner)
        .await
        .unwrap();
    changes::detect_changes(&core).await.unwrap();
    sqlx::query(
        "UPDATE sharepoint_unexpected_items SET first_seen = now() - interval '11 minutes'",
    )
    .execute(&core.db)
    .await
    .unwrap();
    assert!(status::unexpected_files(&core.db).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_change_token_rereads_without_false_alarms() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.expire_delta();
    let r = changes::detect_changes(&core).await.unwrap();
    assert!(r.resynced);
    assert_eq!((r.modified, r.missing, r.unexpected), (0, 0, 0), "{r:?}");
    assert_eq!(last_error(&core, status::DELTA_CURSOR).await, None);
}

#[tokio::test]
async fn renamed_files_and_folders_get_pdms_layout_back() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (doc, _v, item) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.human_rename(&item, "renamed by hand.pdf", "Pat Clerk");
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!(
        (r.mirrors, r.modified),
        (1, 0),
        "a rename is not a content change: {r:?}"
    );
    let folder: String = sqlx::query_scalar(
        "SELECT item_id FROM sharepoint_document_folders WHERE document_id = $1",
    )
    .bind(doc)
    .fetch_one(&core.db)
    .await
    .unwrap();
    sp.human_rename(&folder, "Fuel (old)", "Pat Clerk");
    assert_eq!(changes::detect_changes(&core).await.unwrap().mirrors, 1);
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'sharepoint.mirror_document' AND status = 'pending'
           AND payload->>'document_id' = $1",
    )
    .bind(doc.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(pending, 1, "one pending mirror job for the document");

    // An area folder in the feed is not a change unless it was renamed.
    let area = sp.item("Departments").unwrap().id;
    sp.human_rename(&area, "Departments", "Pat Clerk");
    assert_eq!(changes::detect_changes(&core).await.unwrap().mirrors, 0);
    sp.human_rename(&area, "Departments (old)", "Pat Clerk");
    assert_eq!(
        changes::detect_changes(&core).await.unwrap().mirrors,
        1,
        "a renamed area folder queues a sweep"
    );
}

async fn read_all(mut s: pdms_storage::ByteStream) -> Result<Vec<u8>, std::io::Error> {
    use futures_util::StreamExt;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

#[tokio::test]
async fn a_change_found_by_a_download_reaches_admins_once() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let boss = admin(&core, "admin@county.example", None).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (fuel, fuel_v1, fuel_item) =
        stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.human_edit(&fuel_item, &pdf("fuel, edited by hand"), "Pat Clerk");

    // A download finds it first: stopped, flagged, administrators told once.
    // From then on the file is paused, so a second download is refused.
    let ctx = as_user(&core, alice).await;
    let dl = documents::open_file(&core, &ctx, fuel, None).await.unwrap();
    assert!(
        read_all(dl.stream).await.is_err(),
        "the download must not complete"
    );
    assert!(matches!(
        documents::open_file(&core, &ctx, fuel, None).await.err(),
        Some(CoreError::Conflict(_))
    ));
    assert_eq!(
        file_state(&core, fuel_v1).await,
        ("changed_outside".to_owned(), None)
    );
    assert_eq!(
        in_app_notices(&core).await,
        vec![(
            boss,
            "SharePoint: 1 file(s) changed outside PDMS".to_owned()
        )]
    );
    let flagged = audit_actions(&core)
        .await
        .iter()
        .filter(|a| *a == "sharepoint.changed_outside")
        .count();
    assert_eq!(flagged, 1, "audited once, not on every download");

    // The change check then records who did it, without a second alarm.
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!((r.modified, r.missing), (0, 0), "{r:?}");
    assert_eq!(
        file_state(&core, fuel_v1).await,
        (
            "changed_outside".to_owned(),
            Some("Pat Clerk <pat.clerk@county.example>".to_owned())
        )
    );
    assert_eq!(in_app_notices(&core).await.len(), 1);
}

/// A department, an administrator who is a member (so they may edit), the
/// document owner, and one document whose file someone edited in SharePoint.
async fn edited_in_sharepoint(
    sp: &MockGraph,
    core: &Core,
    admin_in_department: bool,
) -> (Uuid, Uuid, Uuid, String, Vec<u8>) {
    let roads = department(core, "RB", "Road and Bridge").await;
    let boss = admin(
        core,
        "admin@county.example",
        admin_in_department.then_some(roads),
    )
    .await;
    let alice = entra_user(core, "alice@county.example", Some(roads)).await;
    let original = pdf("fuel at 2026 rates");
    let (doc, v1, item) = stored_document(core, alice, roads, "Fuel Supply", &original).await;
    changes::detect_changes(core).await.unwrap();
    sp.human_edit(&item, &pdf("fuel at 2027 rates"), "Pat Clerk");
    changes::detect_changes(core).await.unwrap();
    assert_eq!(file_state(core, v1).await.0, "changed_outside");
    (boss, doc, v1, item, original)
}

#[tokio::test]
async fn restore_original_puts_the_recorded_file_back() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (boss, doc, v1, item, original) = edited_in_sharepoint(&sp, &core, true).await;
    let owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM documents WHERE id = $1")
        .bind(doc)
        .fetch_one(&core.db)
        .await
        .unwrap();
    // Only administrators resolve, not the document's owner.
    let owner_ctx = as_user(&core, owner).await;
    assert!(matches!(
        resolve::restore_original(&core, &owner_ctx, v1).await,
        Err(CoreError::Forbidden(_))
    ));
    let ctx = as_user(&core, boss).await;
    resolve::restore_original(&core, &ctx, v1).await.unwrap();
    assert_eq!(sp.content_of(&item), original);
    assert_eq!(file_state(&core, v1).await, ("ok".to_owned(), None));
    assert!(
        audit_actions(&core)
            .await
            .contains(&"sharepoint.change_resolved".to_owned())
    );
    // The restore was PDMS's own change: the next check finds nothing.
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!((r.modified, r.missing, r.cleared), (0, 0, 0), "{r:?}");
}

#[tokio::test]
async fn accept_as_new_version_keeps_the_change_and_repairs_history() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (boss, doc, v1, item, original) = edited_in_sharepoint(&sp, &core, true).await;
    let ctx = as_user(&core, boss).await;
    let n = resolve::accept_as_new_version(&core, &ctx, v1)
        .await
        .unwrap();
    assert_eq!(n, 2);
    let f = documents::open_file(&core, &ctx, doc, Some(2))
        .await
        .unwrap();
    assert_eq!(read_all(f.stream).await.unwrap(), pdf("fuel at 2027 rates"));
    assert_eq!(
        sp.content_of(&item),
        original,
        "version 1's file is back as it was"
    );
    assert_eq!(file_state(&core, v1).await.0, "ok");
    let note: String = sqlx::query_scalar(
        "SELECT note FROM document_versions WHERE document_id = $1 AND version_no = 2",
    )
    .bind(doc)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert!(note.contains("Pat Clerk"), "{note}");
}

#[tokio::test]
async fn accept_needs_edit_rights_restore_does_not() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    // An administrator outside the department may view but not edit the document.
    let (boss, _doc, v1, _item, _original) = edited_in_sharepoint(&sp, &core, false).await;
    let ctx = as_user(&core, boss).await;
    assert!(matches!(
        resolve::accept_as_new_version(&core, &ctx, v1).await,
        Err(CoreError::Forbidden(_))
    ));
    assert_eq!(
        file_state(&core, v1).await.0,
        "changed_outside",
        "nothing changed"
    );
    resolve::restore_original(&core, &ctx, v1).await.unwrap();
    assert_eq!(file_state(&core, v1).await.0, "ok");
}

#[tokio::test]
async fn a_deleted_file_is_cleared_after_a_recycle_bin_restore_and_recheck() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let boss = admin(&core, "admin@county.example", Some(roads)).await;
    let (_doc, v1, item) = stored_document(&core, boss, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.human_delete(&item, "Pat Clerk");
    changes::detect_changes(&core).await.unwrap();
    let ctx = as_user(&core, boss).await;
    let err = resolve::restore_original(&core, &ctx, v1)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("recycle bin"), "{err}");
    assert!(!resolve::recheck(&core, &ctx, v1).await.unwrap());
    sp.restore_from_recycle_bin(&item);
    assert!(resolve::recheck(&core, &ctx, v1).await.unwrap());
    assert_eq!(file_state(&core, v1).await, ("ok".to_owned(), None));
}

#[tokio::test]
async fn dismissed_untracked_files_leave_the_list() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let boss = admin(&core, "admin@county.example", Some(roads)).await;
    stored_document(&core, boss, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    let stray = sp.human_upload(
        "Departments/Road and Bridge",
        "notes.docx",
        b"notes",
        "Pat Clerk",
    );
    changes::detect_changes(&core).await.unwrap();
    sqlx::query(
        "UPDATE sharepoint_unexpected_items SET first_seen = now() - interval '11 minutes'",
    )
    .execute(&core.db)
    .await
    .unwrap();
    assert_eq!(status::unexpected_files(&core.db).await.unwrap().len(), 1);
    resolve::dismiss_unexpected(&core, &as_user(&core, boss).await, &stray)
        .await
        .unwrap();
    assert!(status::unexpected_files(&core.db).await.unwrap().is_empty());
    assert!(
        audit_actions(&core)
            .await
            .contains(&"sharepoint.unexpected_dismissed".to_owned())
    );
}

#[tokio::test]
async fn a_sharepoint_outage_while_resolving_changes_nothing() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (boss, _doc, v1, _item, _original) = edited_in_sharepoint(&sp, &core, true).await;
    let ctx = as_user(&core, boss).await;
    sp.fail("GET /drives/drive-1/items/");
    let err = resolve::restore_original(&core, &ctx, v1)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Unavailable(m) if m.contains("the change was not made")),
        "{err:?}"
    );
    assert_eq!(
        file_state(&core, v1).await.0,
        "changed_outside",
        "nothing changed"
    );
    sp.heal();
    resolve::restore_original(&core, &ctx, v1).await.unwrap();
    assert_eq!(file_state(&core, v1).await.0, "ok");
}

#[tokio::test]
async fn verify_integrity_checks_sharepoint_files() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (_a, _va, edited) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    let (_b, _vb, deleted) =
        stored_document(&core, alice, roads, "Office Lease", &pdf("lease")).await;
    let clean = integrity::verify(&core, true).await.unwrap();
    assert!(clean.problems.is_empty(), "{:?}", clean.problems);
    assert_eq!(clean.sharepoint_files_checked, 2);

    // Same size, different content: only a hash shows it.
    sp.human_edit(&edited, &pdf("FUEL"), "Pat Clerk");
    sp.human_delete(&deleted, "Pat Clerk");
    let fast = integrity::verify(&core, false).await.unwrap();
    assert_eq!(
        fast.problems.len(),
        1,
        "presence and size only: {:?}",
        fast.problems
    );
    assert!(fast.problems[0].detail.contains("missing from SharePoint"));
    let full = integrity::verify(&core, true).await.unwrap();
    assert!(
        full.problems
            .iter()
            .any(|p| p.detail.contains("QuickXorHash")),
        "{:?}",
        full.problems
    );
    let deep = integrity::verify_with(
        &core,
        integrity::VerifyOptions {
            rehash: true,
            download_sharepoint: true,
        },
    )
    .await
    .unwrap();
    assert!(
        deep.problems.iter().any(|p| p.detail.contains("SHA-256")),
        "{:?}",
        deep.problems
    );
    assert!(deep.problems.iter().all(|p| p.kind == "sharepoint_file"));
}
