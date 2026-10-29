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
    let mut cfg = (*core.config()).clone();
    cfg.graph = m.config(certificate());
    cfg.entra_sync.enabled = true;
    cfg.auth.oidc.issuer_url = ISSUER.into();
    (Core::new(core.db.clone(), cfg).unwrap(), db)
}

/// The same core with `entra_sync.group_id` set.
async fn with_group(core: &Core, group: &str) -> Core {
    let mut cfg = (*core.config()).clone();
    cfg.entra_sync.group_id = group.into();
    Core::new(core.db.clone(), cfg).unwrap()
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

/// `user.idp_memberships_synced` events about one person.
async fn membership_audits(core: &Core, user: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'user.idp_memberships_synced' AND object_id = $1",
    )
    .bind(user.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap()
}

/// Initial (full) rounds requested so far.
fn full_rounds(m: &MockGraph) -> usize {
    m.requests()
        .iter()
        .filter(|r| r.starts_with("GET /v1.0/users/delta?$select="))
        .count()
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
async fn mapping_changes_apply_at_once_to_people_already_synced() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let ops = directory::create_department(&core.db, &system(), "OPS", "Operations")
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
        Some("road and bridge "),
    ));
    m.add_user(member(OID_C, "Carl", "carl@county.example", Some("Clerk")));
    // The first sync, before any mapping: nobody has a department.
    entra_sync::run(&core, &system()).await.unwrap();
    let (a, b, c) = (
        id_of(&core, OID_A).await.unwrap(),
        id_of(&core, OID_B).await.unwrap(),
        id_of(&core, OID_C).await.unwrap(),
    );
    assert!(idp_departments(&core, a).await.is_empty());
    // Bob is already in Road and Bridge by hand.
    directory::add_department_member(&core.db, &system(), b, rb, false)
        .await
        .unwrap();

    // Mapping the value gives Alice the department now, with no full resync.
    entra_values::upsert(&core.db, &system(), "ROAD  AND BRIDGE", rb)
        .await
        .unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert_eq!(membership_audits(&core, a).await, 1);
    assert!(
        idp_departments(&core, b).await.is_empty(),
        "the manual membership satisfies it"
    );
    assert_eq!(membership_audits(&core, b).await, 0);
    assert!(idp_departments(&core, c).await.is_empty(), "another value");
    let un = entra_values::unmapped(&core.db).await.unwrap();
    assert_eq!(un.len(), 1, "{un:?}");
    assert_eq!(un[0].value, "Clerk");

    // A following incremental run leaves it in place.
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert!(r.changed_departments.is_empty(), "{r:?}");
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert_eq!(full_rounds(&m), 1);

    // Pointing the value at another department moves her.
    entra_values::upsert(&core.db, &system(), "Road & Bridge", ops)
        .await
        .unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![ops]);
    assert_eq!(idp_departments(&core, b).await, vec![ops]);
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb)
        .await
        .unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert!(idp_departments(&core, b).await.is_empty());
    let (a_events, b_events) = (
        membership_audits(&core, a).await,
        membership_audits(&core, b).await,
    );
    assert_eq!((a_events, b_events), (3, 2));

    // Removing the mapping takes the Entra-managed membership away again.
    entra_values::remove(&core.db, &system(), "road & bridge")
        .await
        .unwrap();
    assert!(idp_departments(&core, a).await.is_empty());
    assert_eq!(membership_audits(&core, a).await, a_events + 1);
    assert_eq!(
        membership_audits(&core, b).await,
        b_events,
        "Bob had no Entra-managed membership"
    );
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT department_id, source::text FROM department_members WHERE user_id = $1",
    )
    .bind(b)
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(rows, vec![(rb, "manual".to_owned())]);
    let un = entra_values::unmapped(&core.db).await.unwrap();
    let rb_value = un.iter().find(|u| u.normalized == "road and bridge");
    assert_eq!(rb_value.map(|u| u.users), Some(2), "{un:?}");
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
    let mut odd = member(OID_D, "Dora", "dora@partner.example", None);
    odd["userType"] = json!("guest");
    m.add_user(odd);
    assert_eq!(
        entra_sync::sync_one(&core, OID_D).await.unwrap(),
        None,
        "any spelling of Guest is a guest"
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

const PW: &str = "correct horse battery staple";

/// A local account whose identity an earlier OIDC sign-in linked.
async fn linked_local(core: &Core, name: &str, email: &str, oid: &str) -> Uuid {
    let id = directory::create_user(&core.db, &system(), name, email, None)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET oidc_issuer = $2, oidc_subject = $3 WHERE id = $1")
        .bind(id)
        .bind(ISSUER)
        .bind(oid)
        .execute(&core.db)
        .await
        .unwrap();
    id
}

fn oid(i: usize) -> String {
    format!("{i:08x}-0000-4000-8000-000000000000")
}

#[tokio::test]
async fn a_full_round_departs_accounts_missing_from_entra() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_user(member(OID_C, "Carl", "carl@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let (a, b, c) = (
        id_of(&core, OID_A).await.unwrap(),
        id_of(&core, OID_B).await.unwrap(),
        id_of(&core, OID_C).await.unwrap(),
    );
    // Linked at a sign-in before the sync, and no longer in Entra.
    let gone = linked_local(&core, "Gone", "gone@county.example", OID_E).await;
    // Another issuer's accounts are not this sync's business.
    let other = directory::create_user(&core.db, &system(), "Other", "o@elsewhere.example", None)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE users SET oidc_issuer = 'https://other.example', oidc_subject = 'x' WHERE id = $1",
    )
    .bind(other)
    .execute(&core.db)
    .await
    .unwrap();

    // Bob is deleted while the saved link has expired: no delta reports it.
    m.remove_user(OID_B, "deleted");
    m.state.lock().unwrap().expire_delta = true;
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 2, "{r:?}");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(status_of(&core, b).await, "departed");
    assert_eq!(status_of(&core, gone).await, "departed");
    assert_eq!(status_of(&core, a).await, "active");
    assert_eq!(status_of(&core, c).await, "active");
    assert_eq!(status_of(&core, other).await, "active");

    // A delta round lists only changes: people it does not mention stay.
    m.update_user(OID_A, json!({ "displayName": "Alice Again" }));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.updated, r.departed), (1, 0), "{r:?}");
    assert_eq!(status_of(&core, c).await, "active");

    // "Full resync" finds a removal the deltas never delivered.
    m.remove_user(OID_C, "deleted");
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 1, "{r:?}");
    assert_eq!(status_of(&core, c).await, "departed");
    assert_eq!(status_of(&core, a).await, "active");
}

#[tokio::test]
async fn the_sweep_holds_back_when_entra_looks_empty_or_too_many_vanish() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();

    // Only a guest comes back: no member accounts, so nobody is departed.
    m.remove_user(OID_A, "deleted");
    m.remove_user(OID_B, "deleted");
    let mut guest = member(OID_G, "Gwen", "gwen@partner.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 0, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    let line = &r.errors[0];
    assert!(line.contains("no member accounts"), "{line}");
    assert!(
        line.contains("Alice <alice@county.example>") && line.contains("Bob <bob@county.example>"),
        "{line}"
    );
    assert!(
        line.contains("Check Entra and the Graph permissions")
            && line.contains("depart those people by hand on their pages"),
        "{line}"
    );
    for o in [OID_A, OID_B] {
        let id = id_of(&core, o).await.unwrap();
        assert_eq!(status_of(&core, id).await, "active");
    }

    // Nine linked accounts; seven vanish at once (limit max(5, 20%) = 5).
    for i in 1..=7 {
        let (name, mail) = (format!("P{i}"), format!("p{i}@county.example"));
        m.add_user(member(&oid(i), &name, &mail, None));
    }
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    entra_sync::run_full(&core, &system()).await.unwrap();
    for i in 1..=7 {
        m.remove_user(&oid(i), "deleted");
    }
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 0, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(r.errors[0].contains("7 of 9"), "{:?}", r.errors);
    assert!(
        r.errors[0].contains("P1 <p1@county.example>")
            && r.errors[0].contains("P7 <p7@county.example>")
            && !r.errors[0].contains("Alice"),
        "{:?}",
        r.errors
    );
    for i in 1..=7 {
        let id = id_of(&core, &oid(i)).await.unwrap();
        assert_eq!(status_of(&core, id).await, "active");
    }

    // Five at once is within the limit.
    m.add_user(member(&oid(1), "P1", "p1@county.example", None));
    m.add_user(member(&oid(2), "P2", "p2@county.example", None));
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 5, "{r:?}");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

#[tokio::test]
async fn accounts_with_a_local_password_are_departed_and_reported() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let a = linked_local(&core, "Alice", "alice@county.example", OID_A).await;
    let c = linked_local(&core, "Carl", "carl@county.example", OID_C).await;
    for id in [a, c] {
        directory::set_local_credentials(&core.db, &core.keys, &system(), id, PW, None)
            .await
            .unwrap();
    }
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_user(member(OID_C, "Carl", "carl@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();

    m.update_user(OID_A, json!({ "accountEnabled": false }));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 1, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    let line = &r.errors[0];
    assert!(
        line.contains("alice@county.example")
            && line.contains("local password")
            && line.contains("departed"),
        "{line}"
    );
    assert_eq!(status_of(&core, a).await, "departed");

    // Also by the sweep of a full round.
    m.remove_user(OID_C, "deleted");
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 1, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(
        r.errors[0].contains("carl@county.example") && r.errors[0].contains("local password"),
        "{:?}",
        r.errors
    );
    assert_eq!(status_of(&core, c).await, "departed");
}

#[tokio::test]
async fn the_sweep_spares_accounts_created_after_the_round_started() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let gone = linked_local(&core, "Gone", "gone@county.example", OID_E).await;
    // A first sign-in after the round listed Entra, before its sweep: the
    // account is newer than the round's start.
    let late = linked_local(&core, "Late", "late@county.example", OID_F).await;
    sqlx::query("UPDATE users SET created_at = now() + interval '1 hour' WHERE id = $1")
        .bind(late)
        .execute(&core.db)
        .await
        .unwrap();
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 1, "{r:?}");
    assert_eq!(status_of(&core, gone).await, "departed");
    assert_eq!(status_of(&core, late).await, "active");
}

#[tokio::test]
async fn a_held_back_sweep_names_at_most_twenty_accounts() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.state.lock().unwrap().page_size = 50;
    for i in 1..=25 {
        let (name, mail) = (format!("P{i:02}"), format!("p{i:02}@county.example"));
        m.add_user(member(&oid(i), &name, &mail, None));
    }
    entra_sync::run(&core, &system()).await.unwrap();
    for i in 1..=21 {
        m.remove_user(&oid(i), "deleted");
    }
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 0, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    let line = &r.errors[0];
    assert!(line.contains("21 of 25"), "{line}");
    assert!(line.contains("P01 <p01@county.example>"), "{line}");
    assert!(line.contains("P20 <p20@county.example>"), "{line}");
    assert!(!line.contains("P21"), "{line}");
    assert!(line.contains("and 1 more"), "{line}");
    assert!(line.contains("depart those people by hand"), "{line}");
    assert!(!line.contains("run a full sync"), "{line}");
}

#[tokio::test]
async fn the_last_administrator_does_not_count_toward_the_sweep_limit() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    for i in 1..=6 {
        let (name, mail) = (format!("P{i}"), format!("p{i}@county.example"));
        m.add_user(member(&oid(i), &name, &mail, None));
    }
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    let admin = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), a, admin, None)
        .await
        .unwrap();

    // Six of seven vanish, but the last administrator is never departed:
    // five departures are within the limit of five.
    m.remove_user(OID_A, "deleted");
    for i in 1..=5 {
        m.remove_user(&oid(i), "deleted");
    }
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 5, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(
        r.errors[0].contains("alice@county.example")
            && r.errors[0].contains("administrator must remain"),
        "{:?}",
        r.errors
    );
    assert_eq!(status_of(&core, a).await, "active");
}

#[tokio::test]
async fn the_sync_refuses_to_run_while_turned_off() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let mut cfg = (*core.config()).clone();
    cfg.entra_sync.enabled = false;
    let off = Core::new(core.db.clone(), cfg).unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    for full in [false, true] {
        let r = if full {
            entra_sync::run_full(&off, &system()).await
        } else {
            entra_sync::run(&off, &system()).await
        };
        assert!(
            matches!(&r, Err(CoreError::Validation(m)) if m.contains("turned off")),
            "{r:?}"
        );
    }
    // A run queued before the sync was turned off finishes without retrying.
    for full in [false, true] {
        entra_sync::run_job(&off, &json!({ "full": full }))
            .await
            .unwrap();
    }
    assert!(m.requests().is_empty(), "{:?}", m.requests());
    assert!(id_of(&core, OID_A).await.is_none());
}

#[tokio::test]
async fn changing_include_guests_starts_a_full_round() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let with_guests = |on: bool| {
        let mut cfg = (*core.config()).clone();
        cfg.entra_sync.include_guests = on;
        Core::new(core.db.clone(), cfg).unwrap()
    };
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    let mut guest = member(OID_G, "Gwen", "gwen@partner.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    entra_sync::run(&core, &system()).await.unwrap();
    assert!(id_of(&core, OID_G).await.is_none());

    // Guests on: a full round, since a delta would not list her again.
    let on = with_guests(true);
    let r = entra_sync::run(&on, &system()).await.unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert_eq!(full_rounds(&m), 2);
    let g = id_of(&core, OID_G).await.unwrap();
    entra_sync::run(&on, &system()).await.unwrap();
    assert_eq!(full_rounds(&m), 2, "the same setting continues the delta");

    // Guests off again: the full round departs the guest account.
    let off = with_guests(false);
    let r = entra_sync::run(&off, &system()).await.unwrap();
    assert_eq!(full_rounds(&m), 3);
    assert_eq!(r.departed, 1, "{r:?}");
    assert_eq!(status_of(&core, g).await, "departed");
    let a = id_of(&core, OID_A).await.unwrap();
    assert_eq!(status_of(&core, a).await, "active");
}

#[tokio::test]
async fn changing_the_attribute_starts_a_full_read() {
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
    let mut alice = member(
        OID_A,
        "Alice",
        "alice@county.example",
        Some("Road & Bridge"),
    );
    alice["officeLocation"] = json!("Operations");
    m.add_user(alice);
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![rb]);

    let mut cfg = (*core.config()).clone();
    cfg.entra_sync.attribute = "officeLocation".into();
    let core = Core::new(core.db.clone(), cfg).unwrap();
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.updated, 1, "{r:?}");
    assert_eq!(idp_departments(&core, a).await, vec![ops]);
    let initial: Vec<String> = m
        .requests()
        .into_iter()
        .filter(|r| r.starts_with("GET /v1.0/users/delta?$select="))
        .collect();
    assert_eq!(initial.len(), 2, "{initial:?}");
    assert!(initial[1].contains("officeLocation"), "{initial:?}");

    // The new cursor carries the new attribute: the next run is a delta again.
    entra_sync::run(&core, &system()).await.unwrap();
    let rounds = m
        .requests()
        .iter()
        .filter(|r| r.starts_with("GET /v1.0/users/delta?$select="))
        .count();
    assert_eq!(rounds, 2);
}

#[tokio::test]
async fn the_daily_run_is_queued_once_after_run_at_local_time() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let mut cfg = (*core.config()).clone();
    cfg.org.timezone = "America/Chicago".into();
    cfg.entra_sync.run_at = "02:30".into();
    let core = Core::new(core.db.clone(), cfg).unwrap();
    let at = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().unwrap();
    // 2026-10-15 07:00Z is 02:00 CDT: too early.
    assert!(
        !entra_sync::schedule_daily(&core, at("2026-10-15T07:00:00Z"))
            .await
            .unwrap()
    );
    // 08:00Z is 03:00 CDT: queued, once.
    assert!(
        entra_sync::schedule_daily(&core, at("2026-10-15T08:00:00Z"))
            .await
            .unwrap()
    );
    assert!(
        !entra_sync::schedule_daily(&core, at("2026-10-15T20:00:00Z"))
            .await
            .unwrap()
    );
    // The next local day queues again.
    assert!(
        entra_sync::schedule_daily(&core, at("2026-10-16T08:00:00Z"))
            .await
            .unwrap()
    );
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM jobs WHERE kind = 'directory.entra_sync' ORDER BY 1",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec!["entra_sync:daily:2026-10-15", "entra_sync:daily:2026-10-16"]
    );

    let (off, _db2) = common::core().await;
    assert!(
        !entra_sync::schedule_daily(&off, at("2026-10-15T08:00:00Z"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn the_job_runs_incremental_or_full() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run_job(&core, &json!({ "full": false }))
        .await
        .unwrap();
    entra_sync::run_job(&core, &json!({ "full": true }))
        .await
        .unwrap();
    let full_rounds = m
        .requests()
        .iter()
        .filter(|r| r.starts_with("GET /v1.0/users/delta?$select="))
        .count();
    assert_eq!(full_rounds, 2, "the full job ignored the saved cursor");
}

#[tokio::test]
async fn the_job_skips_quietly_while_another_run_holds_the_lock() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let mut held = core.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(entra_sync::LOCK_KEY)
        .execute(&mut *held)
        .await
        .unwrap();
    entra_sync::run_job(&core, &json!({ "full": false }))
        .await
        .unwrap();
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn first_sign_in_asks_entra_when_the_sync_is_on() {
    use pdms_core::auth::oidc::resolve_identity;
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

    let (id, name) = resolve_identity(
        &core,
        &format!("{ISSUER}/"),
        OID_A,
        Some("x@x.org"),
        Some("Token Name"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(name, "Alice", "Entra, not the token, names the person");
    assert_eq!(
        idp_departments(&core, id).await,
        vec![rb],
        "department is set before the first page"
    );
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_A, None, None)
            .await
            .unwrap(),
        Ok((id, "Alice".to_owned()))
    );
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_G, Some("gwen@partner.example"), None)
            .await
            .unwrap(),
        Err("not_provisioned")
    );
    m.fail_next(500, 0);
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_E, Some("e@county.example"), None)
            .await
            .unwrap(),
        Err("directory_unavailable")
    );
}

#[tokio::test]
async fn first_sign_in_without_the_sync_never_links_by_email() {
    use pdms_core::auth::oidc::resolve_identity;
    let (core, _db) = core().await;
    let local = directory::create_user(
        &core.db,
        &system(),
        "Break Glass",
        "admin@county.example",
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        resolve_identity(
            &core,
            ISSUER,
            OID_A,
            Some("Admin@County.example"),
            Some("Mallory")
        )
        .await
        .unwrap(),
        Err("email_in_use")
    );
    let subject: Option<String> =
        sqlx::query_scalar("SELECT oidc_subject FROM users WHERE id = $1")
            .bind(local)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(subject, None);
    let (id, _) = resolve_identity(
        &core,
        ISSUER,
        OID_B,
        Some("new@county.example"),
        Some("New Person"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(id_of(&core, OID_B).await, Some(id));
}

#[tokio::test]
async fn first_sign_in_with_the_sync_on_never_takes_an_existing_email() {
    use pdms_core::auth::oidc::resolve_identity;
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
    assert_eq!(
        resolve_identity(
            &core,
            ISSUER,
            OID_A,
            Some("Alice@County.example"),
            Some("Alice")
        )
        .await
        .unwrap(),
        Err("email_in_use")
    );
    let subject: Option<String> =
        sqlx::query_scalar("SELECT oidc_subject FROM users WHERE id = $1")
            .bind(local)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(subject, None, "never linked by email");
    assert_eq!(id_of(&core, OID_A).await, None);
}

/// A first sign-in while a sync creates the same person: the sign-in's
/// insert finds the sync's row (DO NOTHING), and the person still gets in.
#[tokio::test]
async fn first_sign_in_that_races_a_sync_still_succeeds() {
    use pdms_core::auth::oidc::resolve_identity;
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    // The sync's insert, not yet committed when the sign-in looks for the person.
    let mut sync = core.db.begin().await.unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject, source, entra_synced_at)
         VALUES ($1, 'Alice', 'alice@county.example', $2, $3, 'entra', now())",
    )
    .bind(id)
    .bind(ISSUER)
    .bind(OID_A)
    .execute(&mut *sync)
    .await
    .unwrap();
    let signing_in = tokio::spawn({
        let core = core.clone();
        async move { resolve_identity(&core, ISSUER, OID_A, Some("alice@county.example"), None).await }
    });
    // Let the sync commit once the sign-in's insert is waiting on its row.
    let mut waiting = false;
    for _ in 0..500 {
        waiting = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                             WHERE datname = current_database() AND wait_event_type = 'Lock')",
        )
        .fetch_one(&core.db)
        .await
        .unwrap();
        if waiting {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(waiting, "the sign-in never reached its insert");
    sync.commit().await.unwrap();
    assert_eq!(
        signing_in.await.unwrap().unwrap(),
        Ok((id, "Alice".to_owned()))
    );
}

#[tokio::test]
async fn a_successful_sync_queues_a_folder_access_run() {
    let m = MockGraph::start().await;
    m.add_user(member(
        OID_A,
        "Alice Ops",
        "alice@county.example",
        Some("Road and Bridge"),
    ));
    let (core, _db) = synced(&m).await;
    // Only a sync that changes a department membership queues the run.
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road and Bridge", rb)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs WHERE kind = 'sharepoint.reconcile_access'")
        .execute(&core.db)
        .await
        .unwrap();
    entra_sync::run_job(&core, &json!({})).await.unwrap();
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'sharepoint.reconcile_access'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert!(n >= 1, "{n}");
}

#[tokio::test]
async fn the_preview_shows_what_the_sync_would_do_and_changes_nothing() {
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
        "Ann",
        "ann@example.org",
        Some("Road & Bridge"),
    ));
    m.add_user(member(OID_B, "Bob", "bob@example.org", Some("Parks")));
    let mut guest = member(OID_C, "Gus", "gus@example.net", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    let mut off = member(OID_D, "Dee", "dee@example.org", None);
    off["accountEnabled"] = json!(false);
    m.add_user(off);

    let rows = entra_sync::preview(&core, &system()).await.unwrap();
    let by = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
    assert_eq!(by("Ann").maps_to.as_deref(), Some("Road and Bridge"));
    assert_eq!(by("Ann").skipped, None);
    assert_eq!(by("Bob").department, "Parks");
    assert_eq!(by("Bob").maps_to, None);
    assert_eq!(by("Gus").skipped, Some("guest account"));
    assert_eq!(by("Dee").skipped, Some("disabled"));
    assert_eq!(id_of(&core, OID_A).await, None, "nobody was added");
    assert_eq!(saved_link(&core).await, None);

    let (clerk, _) = user_in_new_department(&core, "CL", "clerk@x.org").await;
    let ctx = as_user(&core, clerk).await;
    let err = entra_sync::preview(&core, &ctx).await.unwrap_err();
    assert!(matches!(err, CoreError::Forbidden(_)), "{err:?}");
}

const GROUP_P: &str = "6f1b2c3d-0000-4000-8000-00000000abcd";
const GROUP_N: &str = "6f1b2c3d-0000-4000-8000-00000000abce";

#[tokio::test]
async fn test_group_names_the_group_and_counts_nested_members() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_group(GROUP_N, "Nested");
    m.add_to_group(GROUP_P, OID_A);
    m.nest_group(GROUP_P, GROUP_N);
    m.add_to_group(GROUP_N, OID_A);
    m.add_to_group(GROUP_N, OID_B);
    let check = entra_sync::test_group(&core, &system(), GROUP_P)
        .await
        .unwrap();
    assert!(check.ok, "{check:?}");
    let text: Vec<&str> = check.lines.iter().map(|(_, l)| l.as_str()).collect();
    assert!(text.contains(&"Group \"PDMS\" found."), "{text:?}");
    assert!(
        text.iter().any(|l| l.contains("It has 2 people")),
        "{text:?}"
    );
    assert!(
        audit_actions(&core)
            .await
            .contains(&"settings.tested".to_owned())
    );
}

#[tokio::test]
async fn test_group_explains_a_wrong_id_an_unknown_group_and_a_missing_permission() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_group(GROUP_P, "PDMS");
    let check = entra_sync::test_group(&core, &system(), "not-a-guid")
        .await
        .unwrap();
    assert!(!check.ok);
    assert!(check.lines.iter().any(|(_, l)| l.contains("Object ID")));

    let unknown = "11111111-0000-4000-8000-000000000001";
    let check = entra_sync::test_group(&core, &system(), unknown)
        .await
        .unwrap();
    assert!(!check.ok);
    assert!(
        check
            .lines
            .iter()
            .any(|(_, l)| l.contains("has no group with the object ID")),
        "{check:?}"
    );

    m.forbid_groups(true);
    let check = entra_sync::test_group(&core, &system(), GROUP_P)
        .await
        .unwrap();
    assert!(!check.ok);
    assert!(
        check
            .lines
            .iter()
            .any(|(_, l)| l.contains("GroupMember.Read.All")),
        "{check:?}"
    );

    let (clerk, _) = user_in_new_department(&core, "HR", "hr@x.org").await;
    let err = entra_sync::test_group(&core, &as_user(&core, clerk).await, GROUP_P)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Forbidden(_)), "{err:?}");
}

#[tokio::test]
async fn changing_the_group_starts_a_full_round() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    entra_sync::run(&core, &system()).await.unwrap();
    let before = full_rounds(&m);

    m.add_to_group(GROUP_P, OID_A);
    let grouped = with_group(&core, GROUP_P).await;
    entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(full_rounds(&m), before + 1);

    // The same ID in capitals and braces is the same setting.
    let shouted = with_group(&core, &format!("{{{}}}", GROUP_P.to_uppercase())).await;
    entra_sync::run(&shouted, &system()).await.unwrap();
    assert_eq!(full_rounds(&m), before + 1, "no new full round");
}
