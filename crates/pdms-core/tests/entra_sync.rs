// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Entra ID user sync: Department mapping, delta sync, sign-in lookups.

mod common;
mod mock_graph;

use std::collections::BTreeSet;

use common::*;
use mock_graph::*;
use pdms_core::directory::{self, entra_sync, entra_values};
use pdms_core::{Core, CoreError};
use serde_json::json;
use uuid::Uuid;

const ISSUER: &str = "https://login.microsoftonline.com/tenant-1/v2.0";
const OID_A: &str = "a0000000-0000-4000-8000-00000000000a";
const OID_B: &str = "b0000000-0000-4000-8000-00000000000b";
const OID_C: &str = "c0000000-0000-4000-8000-00000000000c";
const OID_D: &str = "d0000000-0000-4000-8000-00000000000d";
const OID_E: &str = "e0000000-0000-4000-8000-00000000000e";
const OID_F: &str = "f0000000-0000-4000-8000-00000000000f";
const OID_G: &str = "90000000-0000-4000-8000-000000000009";

/// A core whose Graph is the mock, with the sync on.
async fn synced(m: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut cfg = (*core.config).clone();
    cfg.graph = m.config(certificate());
    cfg.entra_sync.enabled = true;
    cfg.auth.oidc.issuer_url = ISSUER.into();
    (Core::new(core.db.clone(), cfg).unwrap(), db)
}

async fn id_of(core: &Core, oid: &str) -> Option<Uuid> {
    sqlx::query_scalar("SELECT id FROM users WHERE oidc_subject = $1")
        .bind(oid)
        .fetch_optional(&core.db)
        .await
        .unwrap()
}

async fn idp_departments(core: &Core, user: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT department_id FROM department_members WHERE user_id = $1 AND source = 'idp' ORDER BY 1",
    )
    .bind(user)
    .fetch_all(&core.db)
    .await
    .unwrap()
}

async fn status_of(core: &Core, user: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

async fn saved_link(core: &Core) -> Option<String> {
    sqlx::query_scalar("SELECT delta_link FROM sync_cursors WHERE name = 'entra.users'")
        .fetch_optional(&core.db)
        .await
        .unwrap()
        .flatten()
}

#[tokio::test]
async fn department_values_map_case_and_spacing_insensitively() {
    let (core, _db) = core().await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb)
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "R and B", rb)
        .await
        .unwrap();
    assert_eq!(
        entra_values::lookup(&core.db, Some("  road AND   bridge"))
            .await
            .unwrap(),
        Some(rb)
    );
    assert_eq!(
        entra_values::lookup(&core.db, Some("")).await.unwrap(),
        None
    );
    assert_eq!(entra_values::lookup(&core.db, None).await.unwrap(), None);
    assert_eq!(entra_values::list(&core.db).await.unwrap().len(), 2);

    entra_values::remove(&core.db, &system(), "r and b")
        .await
        .unwrap();
    assert!(matches!(
        entra_values::remove(&core.db, &system(), "nothing").await,
        Err(CoreError::NotFound)
    ));
    assert!(matches!(
        entra_values::upsert(&core.db, &system(), "   ", rb).await,
        Err(CoreError::Validation(_))
    ));
    let (clerk, dept) = user_in_new_department(&core, "HR", "hr@x.org").await;
    assert!(matches!(
        entra_values::upsert(&core.db, &as_user(&core, clerk).await, "HR", dept).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        audit_actions(&core)
            .await
            .contains(&"directory.department_mapping_changed".to_owned())
    );
}

/// The issuer backfill from migration 0010, run as written in the file (tests
/// start from a migrated database, so the statement is executed directly).
#[tokio::test]
async fn migration_backfill_normalizes_issuers_and_skips_collisions() {
    let (_core, db) = core().await;
    let sql = include_str!("../../../migrations/0010_entra_sync.sql");
    let start = sql.find("UPDATE users u").unwrap();
    let stmt = &sql[start..start + sql[start..].find(';').unwrap()];

    let insert = |email: &'static str, issuer: &'static str, subject: &'static str| {
        let owner = db.owner.clone();
        async move {
            sqlx::query(
                "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject)
                 VALUES (gen_random_uuid(), 'T', $1, $2, $3)",
            )
            .bind(email)
            .bind(issuer)
            .bind(subject)
            .execute(&owner)
            .await
            .unwrap();
        }
    };
    insert("a@x.org", "https://login.example/t/v2.0/", "s1").await;
    insert("b@x.org", "  https://login.example/t/v2.0  ", "s2").await;
    insert("c@x.org", "https://login.example/t/v2.0", "s3").await;
    // The same subject under both spellings: neither row may be changed.
    insert("d@x.org", "https://other.example/", "dup").await;
    insert("e@x.org", "https://other.example", "dup").await;

    sqlx::query(stmt).execute(&db.owner).await.unwrap();

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT email, oidc_issuer FROM users WHERE email LIKE '_@x.org' ORDER BY email",
    )
    .fetch_all(&db.owner)
    .await
    .unwrap();
    let issuer = |e: &str| rows.iter().find(|r| r.0 == e).unwrap().1.clone();
    assert_eq!(issuer("a@x.org"), "https://login.example/t/v2.0");
    assert_eq!(issuer("b@x.org"), "https://login.example/t/v2.0");
    assert_eq!(issuer("c@x.org"), "https://login.example/t/v2.0");
    assert_eq!(issuer("d@x.org"), "https://other.example/");
    assert_eq!(issuer("e@x.org"), "https://other.example");
}

#[tokio::test]
async fn first_sync_creates_members_maps_departments_and_skips_guests() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb)
        .await
        .unwrap();
    m.add_user(member(
        OID_A,
        "Alice Road",
        "alice@county.example",
        Some("ROAD AND BRIDGE"),
    ));
    m.add_user(member(
        OID_B,
        "Bob Nowhere",
        "bob@county.example",
        Some("Mystery Dept"),
    ));
    m.add_user(member(OID_C, "Carl Blank", "carl@county.example", None));
    let mut guest = member(
        OID_G,
        "Gwen Guest",
        "gwen_partner.example#EXT#@county.example",
        None,
    );
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    let mut off = member(OID_D, "Dan Disabled", "dan@county.example", None);
    off["accountEnabled"] = json!(false);
    m.add_user(off);

    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.created, r.unmapped, r.departed), (3, 2, 0), "{r:?}");
    assert_eq!(r.changed_departments, BTreeSet::from([rb]));
    assert!(r.errors.is_empty(), "{:?}", r.errors);

    let a = id_of(&core, OID_A).await.unwrap();
    let (issuer, source): (String, String) =
        sqlx::query_as("SELECT oidc_issuer, source::text FROM users WHERE id = $1")
            .bind(a)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!((issuer.as_str(), source.as_str()), (ISSUER, "entra"));
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert!(id_of(&core, OID_G).await.is_none(), "guests are not synced");
    assert!(
        id_of(&core, OID_D).await.is_none(),
        "disabled accounts are not created"
    );

    assert!(saved_link(&core).await.unwrap().ends_with("$deltatoken=1"));
    let acts = audit_actions(&core).await;
    for a in [
        "directory.user_created",
        "user.idp_memberships_synced",
        "directory.synced",
    ] {
        assert!(acts.contains(&a.to_owned()), "missing {a}");
    }
    assert!(
        m.requests()
            .iter()
            .any(|r| r.starts_with("GET /v1.0/users/delta?$select=")
                && r.contains("department")
                && r.contains("accountEnabled")
                && r.contains("userType"))
    );

    let un = entra_values::unmapped(&core.db).await.unwrap();
    let un: Vec<(&str, i64)> = un.iter().map(|u| (u.value.as_str(), u.users)).collect();
    assert_eq!(un, vec![("", 1), ("Mystery Dept", 1)]);
}

#[tokio::test]
async fn delta_moves_departments_and_departs_disabled_and_deleted_users() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let ops = directory::create_department(&core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let hr = directory::create_department(&core.db, &system(), "HR", "Human Resources")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb)
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Operations", ops)
        .await
        .unwrap();
    m.add_user(member(
        OID_A,
        "Alice",
        "alice@county.example",
        Some("Road & Bridge"),
    ));
    m.add_user(member(
        OID_B,
        "Bob",
        "bob@county.example",
        Some("Operations"),
    ));
    m.add_user(member(
        OID_C,
        "Carl",
        "carl@county.example",
        Some("Operations"),
    ));
    entra_sync::run(&core, &system()).await.unwrap();
    let (a, b, c) = (
        id_of(&core, OID_A).await.unwrap(),
        id_of(&core, OID_B).await.unwrap(),
        id_of(&core, OID_C).await.unwrap(),
    );
    // A manual membership is never touched by the sync.
    directory::add_department_member(&core.db, &system(), a, hr, false)
        .await
        .unwrap();

    m.update_user(
        OID_A,
        json!({ "department": "Operations", "displayName": "Alice Ops" }),
    );
    m.update_user(OID_B, json!({ "accountEnabled": false }));
    m.remove_user(OID_C, "changed");
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.departed, r.updated), (2, 1), "{r:?}");
    assert!(
        r.changed_departments
            .is_superset(&BTreeSet::from([rb, ops]))
    );
    assert_eq!(idp_departments(&core, a).await, vec![ops]);
    let manual: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM department_members WHERE user_id = $1 AND department_id = $2 AND source = 'manual'",
    )
    .bind(a)
    .bind(hr)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(manual, 1);
    assert_eq!(status_of(&core, b).await, "departed");
    assert_eq!(status_of(&core, c).await, "departed");
    let name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(a)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(name, "Alice Ops");
    assert!(
        m.requests().iter().any(|r| r.contains("$deltatoken=1")),
        "used the saved link"
    );

    // Replays and re-enabling change nothing: the sync never reactivates anyone.
    m.update_user(
        OID_A,
        json!({ "department": "Operations", "displayName": "Alice Ops" }),
    );
    m.add_user(member(
        OID_B,
        "Bob",
        "bob@county.example",
        Some("Operations"),
    ));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.created, r.updated, r.departed), (0, 0, 0), "{r:?}");
    assert_eq!(status_of(&core, b).await, "departed");

    // A departed person is not applied, so their Department value is not counted.
    m.update_user(OID_B, json!({ "department": "Mystery Dept" }));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.updated, r.unmapped), (0, 0), "{r:?}");
}

#[tokio::test]
async fn a_manual_membership_in_the_mapped_department_satisfies_the_mapping() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let ops = directory::create_department(&core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb)
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Operations", ops)
        .await
        .unwrap();
    m.add_user(member(
        OID_A,
        "Alice",
        "alice@county.example",
        Some("Operations"),
    ));
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    directory::add_department_member(&core.db, &system(), a, rb, false)
        .await
        .unwrap();

    // Entra now says Road and Bridge, where she already is by hand: the
    // Entra-managed Operations membership goes and nothing is added.
    m.update_user(OID_A, json!({ "department": "Road & Bridge" }));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.updated, r.unmapped), (1, 0), "{r:?}");
    assert_eq!(r.changed_departments, BTreeSet::from([ops]));
    assert!(idp_departments(&core, a).await.is_empty());
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT department_id, source::text FROM department_members WHERE user_id = $1",
    )
    .bind(a)
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(rows, vec![(rb, "manual".to_owned())]);

    // The same value again (a delta replay, then a full read) is no change at all.
    m.update_user(OID_A, json!({ "department": "Road & Bridge" }));
    for full in [false, true] {
        let before = audit_actions(&core).await.len();
        let r = if full {
            entra_sync::run_full(&core, &system()).await.unwrap()
        } else {
            entra_sync::run(&core, &system()).await.unwrap()
        };
        assert_eq!((r.created, r.updated, r.unmapped), (0, 0, 0), "{r:?}");
        assert!(r.changed_departments.is_empty(), "{r:?}");
        let added = audit_actions(&core).await.split_off(before);
        assert_eq!(
            added,
            vec!["directory.synced".to_owned()],
            "only the run itself"
        );
    }
}

#[tokio::test]
async fn an_account_linked_at_an_earlier_sign_in_becomes_entra_managed() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let local = directory::create_user(&core.db, &system(), "Alice", "alice@county.example", None)
        .await
        .unwrap();
    // Linked by an OIDC sign-in before the sync was turned on.
    sqlx::query("UPDATE users SET oidc_issuer = $2, oidc_subject = $3 WHERE id = $1")
        .bind(local)
        .bind(ISSUER)
        .bind(OID_A)
        .execute(&core.db)
        .await
        .unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.created, r.updated, r.unmapped), (0, 1, 1), "{r:?}");
    let source: String = sqlx::query_scalar("SELECT source::text FROM users WHERE id = $1")
        .bind(local)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(source, "entra");
    let changes: serde_json::Value = sqlx::query_scalar(
        "SELECT changes FROM audit_log WHERE action = 'user.updated' AND object_id = $1",
    )
    .bind(local.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(
        changes,
        json!([{ "field": "source", "before": "local", "after": "entra" }])
    );

    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.updated, 0, "{r:?}");
}

#[tokio::test]
async fn expired_delta_token_triggers_a_full_resync() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    m.state.lock().unwrap().expire_delta = true;
    m.add_user(member(OID_E, "Eve", "eve@county.example", None));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.created, 1);
    assert!(id_of(&core, OID_E).await.is_some());
    let full_rounds = m
        .requests()
        .iter()
        .filter(|r| r.starts_with("GET /v1.0/users/delta?$select="))
        .count();
    assert_eq!(full_rounds, 2);
}

#[tokio::test]
async fn a_failed_run_keeps_the_saved_cursor() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let before = saved_link(&core).await;
    m.fail_next(500, 0);
    assert!(entra_sync::run(&core, &system()).await.is_err());
    assert_eq!(saved_link(&core).await, before);
    let st = entra_sync::status(&core.db).await.unwrap();
    assert!(st.last_error.unwrap().contains("500"));

    // The next good run clears the error.
    entra_sync::run(&core, &system()).await.unwrap();
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_error, None);
    assert!(st.last_run.is_some() && st.last_summary.is_some());
}

#[tokio::test]
async fn email_already_used_by_an_unlinked_account_is_reported_not_linked() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let local = directory::create_user(
        &core.db,
        &system(),
        "Break Glass",
        "alice@county.example",
        None,
    )
    .await
    .unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.created, 0);
    assert_eq!(r.errors.len(), 1);
    assert!(
        r.errors[0].contains("alice@county.example"),
        "{:?}",
        r.errors
    );
    assert_eq!(r.unmapped, 0, "a skipped account is not counted");
    let subject: Option<String> =
        sqlx::query_scalar("SELECT oidc_subject FROM users WHERE id = $1")
            .bind(local)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(subject, None, "never linked by email");
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().errors.len(), 1);
    assert_eq!(st.last_error, None, "the run itself succeeded");
}

#[tokio::test]
async fn last_administrator_is_reported_not_departed() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    let admin = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), a, admin, None)
        .await
        .unwrap();
    m.update_user(OID_A, json!({ "accountEnabled": false }));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 0);
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert_eq!(status_of(&core, a).await, "active");
}

#[tokio::test]
async fn sync_one_reads_a_single_user() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb)
        .await
        .unwrap();
    m.add_user(member(
        OID_A,
        "Alice",
        "alice@county.example",
        Some("Road & Bridge"),
    ));
    let mut guest = member(OID_G, "Gwen", "gwen@partner.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    let a = entra_sync::sync_one(&core, OID_A).await.unwrap().unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert_eq!(
        entra_sync::sync_one(&core, OID_A).await.unwrap(),
        Some(a),
        "idempotent"
    );
    assert_eq!(entra_sync::sync_one(&core, OID_G).await.unwrap(), None);
    assert_eq!(
        entra_sync::sync_one(&core, OID_E).await.unwrap(),
        None,
        "unknown in Entra"
    );
    assert!(matches!(
        entra_sync::sync_one(&core, "../users").await,
        Err(CoreError::Validation(_))
    ));
    // Graph returns a null userType for some older member accounts.
    let mut older = member(OID_F, "Fay", "fay@county.example", None);
    older["userType"] = serde_json::Value::Null;
    m.add_user(older);
    assert!(
        entra_sync::sync_one(&core, OID_F).await.unwrap().is_some(),
        "a member"
    );
}

#[tokio::test]
async fn runs_do_not_overlap_and_need_graph() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let mut tx = core.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(entra_sync::LOCK_KEY)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(matches!(
        entra_sync::run(&core, &system()).await,
        Err(CoreError::Conflict(_))
    ));
    tx.rollback().await.unwrap();

    let (plain, _db2) = common::core().await;
    assert!(matches!(
        entra_sync::run(&plain, &system()).await,
        Err(CoreError::Validation(_))
    ));
    assert!(matches!(
        entra_sync::request_sync(&plain, &system(), false).await,
        Err(CoreError::Validation(_))
    ));
    entra_sync::request_sync(&core, &system(), true)
        .await
        .unwrap();
    let full: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE kind = 'directory.entra_sync'")
            .fetch_all(&core.db)
            .await
            .unwrap();
    assert_eq!(full, vec![json!({ "full": true })]);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"directory.sync_requested".to_owned())
    );
}
