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
use pdms_core::directory::entra_sync::SignInLookup;
use pdms_core::directory::{self, entra_sync, entra_values};
use pdms_core::model::UserStatus;
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
    let SignInLookup::Account(a) = entra_sync::sync_one(&core, OID_A).await.unwrap() else {
        panic!("Alice is a member");
    };
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert_eq!(
        entra_sync::sync_one(&core, OID_A).await.unwrap(),
        SignInLookup::Account(a),
        "idempotent"
    );
    assert_eq!(
        entra_sync::sync_one(&core, OID_G).await.unwrap(),
        SignInLookup::NoAccount
    );
    assert_eq!(
        entra_sync::sync_one(&core, OID_E).await.unwrap(),
        SignInLookup::NoAccount,
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
        matches!(
            entra_sync::sync_one(&core, OID_F).await.unwrap(),
            SignInLookup::Account(_)
        ),
        "a member"
    );
    let mut odd = member(OID_D, "Dora", "dora@partner.example", None);
    odd["userType"] = json!("guest");
    m.add_user(odd);
    assert_eq!(
        entra_sync::sync_one(&core, OID_D).await.unwrap(),
        SignInLookup::NoAccount,
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

async fn open_session(core: &Core, user: Uuid) {
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, user_id, auth_method, expires_at)
         VALUES ($1, $2, $3, 'oidc', now() + interval '1 hour')",
    )
    .bind(Uuid::new_v4())
    .bind(vec![7u8; 32])
    .bind(user)
    .execute(&core.db)
    .await
    .unwrap();
}

async fn live_sessions(core: &Core, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

async fn group_mark(core: &Core, user: Uuid) -> bool {
    sqlx::query_scalar("SELECT departed_from_group FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

/// The `reason` in the newest `user.status_changed` audit row for the user.
async fn last_status_reason(core: &Core, user: Uuid) -> Option<String> {
    sqlx::query_scalar(
        "SELECT context->>'reason' FROM audit_log
          WHERE action = 'user.status_changed' AND object_id = $1
          ORDER BY seq DESC LIMIT 1",
    )
    .bind(user.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn departing_for_the_group_is_marked_and_audited() {
    let (core, _db) = core().await;
    let u = directory::create_user(&core.db, &system(), "Una", "una@county.example", None)
        .await
        .unwrap();
    open_session(&core, u).await;
    assert!(
        directory::depart_from_group(&core.db, &system(), u)
            .await
            .unwrap()
    );
    assert_eq!(status_of(&core, u).await, "departed");
    assert!(group_mark(&core, u).await);
    assert_eq!(live_sessions(&core, u).await, 0);
    assert_eq!(
        last_status_reason(&core, u).await.as_deref(),
        Some("is no longer in the PDMS group")
    );
    assert!(
        !directory::depart_from_group(&core.db, &system(), u)
            .await
            .unwrap(),
        "already departed: nothing changes"
    );
}

#[tokio::test]
async fn only_a_group_departure_is_undone_by_reactivate_back_in_group() {
    let (core, _db) = core().await;
    let u = directory::create_user(&core.db, &system(), "Una", "una@county.example", None)
        .await
        .unwrap();
    let role = directory::role_id_by_key(&core.db, "auditor")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), u, role, None)
        .await
        .unwrap();
    directory::set_user_status(&core.db, &system(), u, UserStatus::Departed)
        .await
        .unwrap();
    assert!(
        !directory::reactivate_back_in_group(&core.db, &system(), u)
            .await
            .unwrap()
    );
    assert_eq!(status_of(&core, u).await, "departed");

    directory::set_user_status(&core.db, &system(), u, UserStatus::Active)
        .await
        .unwrap();
    assert!(
        directory::depart_from_group(&core.db, &system(), u)
            .await
            .unwrap()
    );
    assert!(
        directory::reactivate_back_in_group(&core.db, &system(), u)
            .await
            .unwrap()
    );
    assert_eq!(status_of(&core, u).await, "active");
    assert!(!group_mark(&core, u).await);
    assert_eq!(
        last_status_reason(&core, u).await.as_deref(),
        Some("is back in the PDMS group")
    );
    let roles: i64 = sqlx::query_scalar("SELECT count(*) FROM role_assignments WHERE user_id = $1")
        .bind(u)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(roles, 1, "the role is still assigned");
    assert!(
        !directory::reactivate_back_in_group(&core.db, &system(), u)
            .await
            .unwrap(),
        "an active person is left alone"
    );
}

#[tokio::test]
async fn any_other_status_change_clears_the_group_mark() {
    let (core, _db) = core().await;
    let u = directory::create_user(&core.db, &system(), "Una", "una@county.example", None)
        .await
        .unwrap();
    directory::depart_from_group(&core.db, &system(), u)
        .await
        .unwrap();
    directory::set_user_status(&core.db, &system(), u, UserStatus::Active)
        .await
        .unwrap();
    directory::set_user_status(&core.db, &system(), u, UserStatus::Departed)
        .await
        .unwrap();
    assert!(!group_mark(&core, u).await);
    assert!(
        !directory::reactivate_back_in_group(&core.db, &system(), u)
            .await
            .unwrap()
    );
    assert_eq!(status_of(&core, u).await, "departed");
}

#[tokio::test]
async fn the_last_administrator_is_not_departed_for_the_group() {
    let (core, _db) = core().await;
    let a = directory::create_user(&core.db, &system(), "Ada", "ada@county.example", None)
        .await
        .unwrap();
    let admin = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), a, admin, None)
        .await
        .unwrap();
    assert!(matches!(
        directory::depart_from_group(&core.db, &system(), a).await,
        Err(CoreError::Conflict(_))
    ));
    assert_eq!(status_of(&core, a).await, "active");
    assert!(!group_mark(&core, a).await);
}

const OID_H: &str = "80000000-0000-4000-8000-000000000008";

/// The context of the newest `directory.synced` audit row.
async fn last_sync_context(core: &Core) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT context FROM audit_log WHERE action = 'directory.synced' ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&core.db)
    .await
    .unwrap()
}

/// `user.status_changed` audit rows about one person.
async fn status_audits(core: &Core, user: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'user.status_changed' AND object_id = $1",
    )
    .bind(user.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap()
}

/// `GET /users/{oid}` requests (single-user reads) so far.
fn single_reads(m: &MockGraph, oid: &str) -> usize {
    let prefix = format!("GET /v1.0/users/{oid}?$select=");
    m.requests()
        .iter()
        .filter(|r| r.starts_with(&prefix))
        .count()
}

#[tokio::test]
async fn with_a_group_only_members_get_accounts_and_others_are_departed() {
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
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_A);
    m.add_to_group(GROUP_P, OID_B);
    let grouped = with_group(&core, GROUP_P).await;
    m.add_user(member(OID_D, "Dora", "dora@county.example", None));

    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!((r.created, r.departed), (0, 1), "{r:?}");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.held_back.is_empty() && r.reactivated.is_empty(), "{r:?}");
    assert_eq!(status_of(&core, a).await, "active");
    assert_eq!(status_of(&core, b).await, "active");
    assert_eq!(status_of(&core, c).await, "departed");
    assert!(group_mark(&core, c).await);
    assert_eq!(
        last_status_reason(&core, c).await.as_deref(),
        Some("is no longer in the PDMS group")
    );
    assert!(id_of(&core, OID_D).await.is_none(), "not in the group");

    // The next run, a delta, has nothing more to do.
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!((r.created, r.updated, r.departed), (0, 0, 0), "{r:?}");
    assert!(id_of(&core, OID_D).await.is_none());
}

#[tokio::test]
async fn a_member_added_to_the_group_is_created_at_the_next_run() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_E, "Eve", "eve@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_A);
    let grouped = with_group(&core, GROUP_P).await;
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert!(id_of(&core, OID_E).await.is_none(), "not in the group");

    // Joining a group is no change to the user, so no delta lists her.
    m.add_to_group(GROUP_P, OID_E);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let e = id_of(&core, OID_E).await.unwrap();
    assert_eq!(status_of(&core, e).await, "active");
    assert_eq!(single_reads(&m, OID_E), 1, "{:?}", m.requests());
    assert_eq!(full_rounds(&m), 1, "the second run was a delta");

    // Once she has an account, she is not read again.
    entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(single_reads(&m, OID_E), 1, "{:?}", m.requests());
    assert_eq!(single_reads(&m, OID_A), 0, "{:?}", m.requests());
}

#[tokio::test]
async fn members_of_a_nested_group_count() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_F, "Fay", "fay@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_group(GROUP_N, "Road and Bridge");
    m.add_to_group(GROUP_P, OID_A);
    m.nest_group(GROUP_P, GROUP_N);
    m.add_to_group(GROUP_N, OID_F);
    let grouped = with_group(&core, GROUP_P).await;
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.created, 2, "{r:?}");
    let f = id_of(&core, OID_F).await.unwrap();
    assert_eq!(status_of(&core, f).await, "active");

    // Leaving the nested group is leaving the PDMS group.
    m.remove_from_group(GROUP_N, OID_F);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.departed, 1, "{r:?}");
    assert_eq!(status_of(&core, f).await, "departed");
}

#[tokio::test]
async fn narrowing_the_group_holds_back_a_mass_departure() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    for i in 1..=9 {
        let (name, mail) = (format!("P{i}"), format!("p{i}@county.example"));
        m.add_user(member(&oid(i), &name, &mail, None));
    }
    entra_sync::run(&core, &system()).await.unwrap();
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, &oid(1));
    m.add_to_group(GROUP_P, &oid(2));
    let grouped = with_group(&core, GROUP_P).await;

    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.departed, 0, "{r:?}");
    let mut others = BTreeSet::new();
    for i in 3..=9 {
        others.insert(id_of(&core, &oid(i)).await.unwrap());
    }
    assert_eq!(r.held_back.len(), 7, "{r:?}");
    assert_eq!(r.held_back.iter().copied().collect::<BTreeSet<_>>(), others);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    for i in 1..=9 {
        let id = id_of(&core, &oid(i)).await.unwrap();
        assert_eq!(status_of(&core, id).await, "active");
    }
    assert_eq!(last_sync_context(&core).await["held_back"], json!(7));
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().held_back, r.held_back);
}

#[tokio::test]
async fn an_empty_group_departs_nobody_and_says_so() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    m.add_group(GROUP_P, "PDMS");
    let grouped = with_group(&core, GROUP_P).await;

    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.departed, 0, "{r:?}");
    assert!(r.held_back.is_empty(), "{r:?}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    let line = &r.errors[0];
    assert!(
        line.contains("lists no members")
            && line.contains("the 2 linked account(s) outside it were not departed")
            && line.contains("Check the group's object ID and its members"),
        "{line}"
    );
    for o in [OID_A, OID_B] {
        let id = id_of(&core, o).await.unwrap();
        assert_eq!(status_of(&core, id).await, "active");
    }
}

#[tokio::test]
async fn a_missing_permission_fails_the_run_and_changes_nothing() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    let before = saved_link(&core).await;
    // Had the run gone ahead, Hal would be created and Alice departed.
    m.add_group(GROUP_P, "PDMS");
    m.add_user(member(OID_H, "Hal", "hal@county.example", None));
    m.add_to_group(GROUP_P, OID_H);
    m.forbid_groups(true);
    let grouped = with_group(&core, GROUP_P).await;

    let err = entra_sync::run(&grouped, &system()).await.unwrap_err();
    assert!(err.to_string().contains("GroupMember.Read.All"), "{err}");
    let st = entra_sync::status(&core.db).await.unwrap();
    assert!(
        st.last_error
            .as_deref()
            .is_some_and(|e| e.contains("GroupMember.Read.All")),
        "{st:?}"
    );
    assert!(id_of(&core, OID_H).await.is_none());
    assert_eq!(status_of(&core, a).await, "active");
    assert_eq!(saved_link(&core).await, before);

    // With the permission granted, the next run does the work.
    m.forbid_groups(false);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!((r.created, r.departed), (1, 1), "{r:?}");
    assert_eq!(status_of(&core, a).await, "departed");
}

#[tokio::test]
async fn the_last_administrator_and_the_break_glass_account_are_kept() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_user(member(OID_C, "Carl", "carl@county.example", None));
    let c = linked_local(&core, "Carl", "carl@county.example", OID_C).await;
    directory::set_local_credentials(&core.db, &core.keys, &system(), c, PW, None)
        .await
        .unwrap();
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    let admin = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), a, admin, None)
        .await
        .unwrap();
    // The break-glass account: local, with no directory link.
    let glass = directory::create_user(
        &core.db,
        &system(),
        "Break Glass",
        "glass@county.example",
        None,
    )
    .await
    .unwrap();
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_B);
    let grouped = with_group(&core, GROUP_P).await;

    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.departed, 1, "{r:?}");
    assert!(r.held_back.is_empty(), "{r:?}");
    assert_eq!(r.errors.len(), 2, "{:?}", r.errors);
    assert!(
        r.errors.iter().any(|e| e.contains("alice@county.example")
            && e.contains("is no longer in the PDMS group but was not deactivated")),
        "{:?}",
        r.errors
    );
    assert!(
        r.errors.iter().any(|e| e.contains("carl@county.example")
            && e.contains("is no longer in the PDMS group")
            && e.contains("the account also had a local password")),
        "{:?}",
        r.errors
    );
    assert_eq!(status_of(&core, a).await, "active");
    assert!(!group_mark(&core, a).await);
    assert_eq!(status_of(&core, c).await, "departed");
    assert!(group_mark(&core, c).await);
    assert_eq!(status_of(&core, glass).await, "active");
    assert!(!group_mark(&core, glass).await);
    assert_eq!(status_audits(&core, glass).await, 0, "untouched");
}

#[tokio::test]
async fn someone_back_in_the_group_is_reactivated_with_their_roles() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_user(member(OID_C, "Carl", "carl@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    for o in [OID_A, OID_B, OID_C] {
        m.add_to_group(GROUP_P, o);
    }
    let grouped = with_group(&core, GROUP_P).await;
    entra_sync::run(&grouped, &system()).await.unwrap();
    let (a, b, c) = (
        id_of(&core, OID_A).await.unwrap(),
        id_of(&core, OID_B).await.unwrap(),
        id_of(&core, OID_C).await.unwrap(),
    );
    let auditor = directory::role_id_by_key(&core.db, "auditor")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), a, auditor, None)
        .await
        .unwrap();

    // Alice and Bob leave the group.
    m.remove_from_group(GROUP_P, OID_A);
    m.remove_from_group(GROUP_P, OID_B);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.departed, 2, "{r:?}");
    assert!(group_mark(&core, a).await && group_mark(&core, b).await);

    // Entra then disables Bob: his departure is Entra's now.
    m.update_user(OID_B, json!({ "accountEnabled": false }));
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.departed, 0, "{r:?}");
    assert_eq!(status_of(&core, b).await, "departed");
    assert!(!group_mark(&core, b).await);
    // Carl, still in the group, is departed by hand.
    directory::set_user_status(&core.db, &system(), c, UserStatus::Departed)
        .await
        .unwrap();

    // Alice and Bob are added back, and Bob is enabled again.
    m.add_to_group(GROUP_P, OID_A);
    m.add_to_group(GROUP_P, OID_B);
    m.update_user(OID_B, json!({ "accountEnabled": true }));
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.reactivated, vec![a], "{r:?}");
    assert_eq!(status_of(&core, a).await, "active");
    assert!(!group_mark(&core, a).await);
    assert_eq!(
        last_status_reason(&core, a).await.as_deref(),
        Some("is back in the PDMS group")
    );
    let roles: i64 = sqlx::query_scalar("SELECT count(*) FROM role_assignments WHERE user_id = $1")
        .bind(a)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(roles, 1, "the role is still assigned");
    assert_eq!(status_of(&core, b).await, "departed");
    assert_eq!(status_of(&core, c).await, "departed");
    assert_eq!(last_sync_context(&core).await["reactivated"], json!(1));
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().reactivated, vec![a]);

    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert!(r.reactivated.is_empty(), "{r:?}");
    assert_eq!(status_of(&core, b).await, "departed");
    assert_eq!(status_of(&core, c).await, "departed");
}

#[tokio::test]
async fn clearing_the_group_reactivates_nobody() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_A);
    m.add_to_group(GROUP_P, OID_B);
    let grouped = with_group(&core, GROUP_P).await;
    entra_sync::run(&grouped, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    m.remove_from_group(GROUP_P, OID_A);
    entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(status_of(&core, a).await, "departed");

    // Back to the whole tenant: Alice stays departed, still marked.
    let r = entra_sync::run_full(&core, &system()).await.unwrap();
    assert!(r.reactivated.is_empty(), "{r:?}");
    assert_eq!(status_of(&core, a).await, "departed");
    assert!(group_mark(&core, a).await);

    // A group again, with her in it: she comes back.
    m.add_to_group(GROUP_P, OID_A);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.reactivated, vec![a], "{r:?}");
    assert_eq!(status_of(&core, a).await, "active");
}

#[tokio::test]
async fn disabled_and_guest_members_are_skipped_quietly() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    let mut off = member(OID_D, "Dan Disabled", "dan@county.example", None);
    off["accountEnabled"] = json!(false);
    m.add_user(off);
    let mut guest = member(OID_G, "Gwen Guest", "gwen@partner.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    m.add_group(GROUP_P, "PDMS");
    for o in [OID_A, OID_D, OID_G] {
        m.add_to_group(GROUP_P, o);
    }
    let grouped = with_group(&core, GROUP_P).await;
    for run in 1..=2 {
        let r = entra_sync::run(&grouped, &system()).await.unwrap();
        assert!(r.errors.is_empty(), "run {run}: {:?}", r.errors);
        assert!(id_of(&core, OID_D).await.is_none(), "run {run}");
        assert!(id_of(&core, OID_G).await.is_none(), "run {run}");
    }
    assert_eq!(full_rounds(&m), 1, "the second run was a delta");
    assert!(id_of(&core, OID_A).await.is_some());
}

/// `user.updated` audit rows that cleared the group mark, with this reason.
async fn unmarked_audits(core: &Core, user: Uuid, reason: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log
          WHERE action = 'user.updated' AND object_id = $1 AND context->>'reason' = $2
            AND changes @> '[{\"field\": \"departed_from_group\", \"before\": true, \"after\": false}]'",
    )
    .bind(user.to_string())
    .bind(reason)
    .fetch_one(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_member_deleted_from_entra_is_departed_as_not_in_entra() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_A);
    m.add_to_group(GROUP_P, OID_B);
    let grouped = with_group(&core, GROUP_P).await;
    entra_sync::run(&grouped, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();

    // Alice is deleted while the saved link has expired: a complete round,
    // which no longer lists her, and the group no longer lists her either.
    m.remove_user(OID_A, "deleted");
    m.state.lock().unwrap().expire_delta = true;
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(full_rounds(&m), 2);
    assert_eq!(r.departed, 1, "{r:?}");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(status_of(&core, a).await, "departed");
    assert!(
        !group_mark(&core, a).await,
        "Entra's departure, not the group's"
    );
    assert_ne!(
        last_status_reason(&core, a).await.as_deref(),
        Some("is no longer in the PDMS group")
    );

    // Restored in Entra, and still in the group: she stays departed.
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_to_group(GROUP_P, OID_A);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert!(r.reactivated.is_empty(), "{r:?}");
    assert_eq!(status_of(&core, a).await, "departed");
}

#[tokio::test]
async fn a_group_departure_later_deleted_from_entra_loses_its_mark() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_A);
    m.add_to_group(GROUP_P, OID_B);
    let grouped = with_group(&core, GROUP_P).await;
    entra_sync::run(&grouped, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    m.remove_from_group(GROUP_P, OID_A);
    entra_sync::run(&grouped, &system()).await.unwrap();
    assert!(group_mark(&core, a).await);

    // A complete round that lists no member accounts proves nothing.
    m.remove_user(OID_A, "deleted");
    m.remove_user(OID_B, "deleted");
    entra_sync::run_full(&grouped, &system()).await.unwrap();
    assert!(group_mark(&core, a).await, "kept while Entra lists nobody");

    // Once Entra lists its members again without her, she was deleted.
    m.add_user(member(OID_B, "Bob", "bob@county.example", None));
    entra_sync::run_full(&grouped, &system()).await.unwrap();
    assert_eq!(status_of(&core, a).await, "departed");
    assert!(!group_mark(&core, a).await);
    assert_eq!(unmarked_audits(&core, a, "is not in Entra").await, 1);

    // Restored in Entra and added back to the group: she stays departed.
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_to_group(GROUP_P, OID_A);
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert!(r.reactivated.is_empty(), "{r:?}");
    assert_eq!(status_of(&core, a).await, "departed");
}

/// `POST /users/{oid}/checkMemberGroups` requests so far.
fn group_checks(m: &MockGraph, oid: &str) -> usize {
    let line = format!("POST /v1.0/users/{oid}/checkMemberGroups");
    m.requests().iter().filter(|r| **r == line).count()
}

#[tokio::test]
async fn first_sign_in_checks_the_group() {
    use pdms_core::auth::oidc::resolve_identity;
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    m.add_user(member(OID_B, "Ned", "ned@county.example", None));
    m.add_user(member(OID_D, "Dora", "dora@county.example", None));
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, OID_A);
    m.add_to_group(GROUP_P, OID_D);
    let core = with_group(&core, GROUP_P).await;

    let (a, name) = resolve_identity(&core, ISSUER, OID_A, Some("alice@county.example"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(Some(a), id_of(&core, OID_A).await);
    assert_eq!(name, "Alice");
    assert_eq!(group_checks(&m, OID_A), 1);

    // Ned is not in the group: refused, with no account, and never read.
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_B, Some("ned@county.example"), None)
            .await
            .unwrap(),
        Err("not_in_group")
    );
    assert_eq!(id_of(&core, OID_B).await, None, "no account for Ned");
    assert!(
        m.requests()
            .contains(&format!("POST /v1.0/users/{OID_B}/checkMemberGroups")),
        "{:?}",
        m.requests()
    );
    assert_eq!(single_reads(&m, OID_B), 0, "a non-member is not read");

    // The group can't be checked: refused, never let in.
    m.forbid_groups(true);
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_D, Some("dora@county.example"), None)
            .await
            .unwrap(),
        Err("directory_unavailable")
    );
    assert_eq!(id_of(&core, OID_D).await, None, "no account for Dora");
    assert_eq!(single_reads(&m, OID_D), 0);
}

/// The actor of the newest `user.status_changed` audit row for the user.
async fn last_status_actor(core: &Core, user: Uuid) -> Option<String> {
    sqlx::query_scalar(
        "SELECT actor_display FROM audit_log
          WHERE action = 'user.status_changed' AND object_id = $1
          ORDER BY seq DESC LIMIT 1",
    )
    .bind(user.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn someone_back_in_the_group_is_reactivated_when_they_sign_in() {
    use pdms_core::auth::oidc::resolve_identity;
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let people = [
        (OID_A, "Alice", "alice@county.example"),
        (OID_B, "Bob", "bob@county.example"),
        (OID_C, "Carl", "carl@county.example"),
        (OID_D, "Dora", "dora@county.example"),
        (OID_E, "Eve", "eve@county.example"),
    ];
    m.add_group(GROUP_P, "PDMS");
    for (oid, name, mail) in people {
        m.add_user(member(oid, name, mail, None));
        m.add_to_group(GROUP_P, oid);
    }
    let grouped = with_group(&core, GROUP_P).await;
    entra_sync::run(&grouped, &system()).await.unwrap();
    let [a, b, c, d, e] = {
        let mut ids = [Uuid::nil(); 5];
        for (i, (oid, _, _)) in people.iter().enumerate() {
            ids[i] = id_of(&core, oid).await.unwrap();
        }
        ids
    };

    // Alice, Carl, Dora and Eve left the group and were departed for it;
    // Alice, Dora and Eve are back in it. Bob was departed by hand.
    for (oid, id) in [(OID_A, a), (OID_C, c), (OID_D, d), (OID_E, e)] {
        m.remove_from_group(GROUP_P, oid);
        assert!(
            directory::depart_from_group(&core.db, &system(), id)
                .await
                .unwrap()
        );
    }
    for oid in [OID_A, OID_D, OID_E] {
        m.add_to_group(GROUP_P, oid);
    }
    directory::set_user_status(&core.db, &system(), b, UserStatus::Departed)
        .await
        .unwrap();

    // Alice is back in the group: reactivated and signed in.
    assert_eq!(
        resolve_identity(&grouped, ISSUER, OID_A, None, None)
            .await
            .unwrap(),
        Ok((a, "Alice".to_owned()))
    );
    assert_eq!(status_of(&core, a).await, "active");
    assert!(!group_mark(&core, a).await);
    assert_eq!(
        last_status_reason(&core, a).await.as_deref(),
        Some("is back in the PDMS group")
    );
    assert_eq!(
        last_status_actor(&core, a).await.as_deref(),
        Some("entra-sync")
    );

    // Carl is still outside the group.
    assert_eq!(
        resolve_identity(&grouped, ISSUER, OID_C, None, None)
            .await
            .unwrap(),
        Err("not_in_group")
    );
    assert_eq!(status_of(&core, c).await, "departed");
    assert!(group_mark(&core, c).await, "still marked");

    // Bob was departed by hand: refused as before, and Graph is not asked.
    assert_eq!(
        resolve_identity(&grouped, ISSUER, OID_B, None, None)
            .await
            .unwrap(),
        Err("inactive")
    );
    assert_eq!(group_checks(&m, OID_B), 0, "{:?}", m.requests());
    assert_eq!(single_reads(&m, OID_B), 0);
    assert_eq!(status_of(&core, b).await, "departed");

    // Without a group, nobody is reactivated at sign-in, and Graph is not asked.
    let asked = m.requests().len();
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_D, None, None)
            .await
            .unwrap(),
        Err("inactive")
    );
    assert_eq!(m.requests().len(), asked, "{:?}", m.requests());
    assert_eq!(status_of(&core, d).await, "departed");

    // Eve is back in the group but disabled in Entra: she stays departed.
    m.update_user(OID_E, json!({ "accountEnabled": false }));
    assert_eq!(
        resolve_identity(&grouped, ISSUER, OID_E, None, None)
            .await
            .unwrap(),
        Err("inactive")
    );
    assert_eq!(status_of(&core, e).await, "departed");

    // The group can't be checked: Dora is refused and stays departed.
    m.forbid_groups(true);
    assert_eq!(
        resolve_identity(&grouped, ISSUER, OID_D, None, None)
            .await
            .unwrap(),
        Err("directory_unavailable")
    );
    assert_eq!(status_of(&core, d).await, "departed");
    assert!(group_mark(&core, d).await);
}

#[tokio::test]
async fn a_mapping_change_waits_for_a_person_being_applied() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    // A sync holds the shared lock while it applies a person.
    let mut tx = core.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(entra_values::MAPPING_LOCK)
        .execute(&mut *tx)
        .await
        .unwrap();
    let c2 = core.clone();
    let task =
        tokio::spawn(
            async move { entra_values::upsert(&c2.db, &system(), "Road & Bridge", rb).await },
        );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the mapping change must wait");
    tx.commit().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_person_is_applied_with_the_mapping_committed_before_them() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    m.add_user(member(
        OID_A,
        "Alice",
        "alice@county.example",
        Some("Road & Bridge"),
    ));
    // A mapping change is in flight: its lock is held, its row inserted.
    let mut tx = core.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(entra_values::MAPPING_LOCK)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO department_entra_values (value_normalized, value_display, department_id)
         VALUES ('road and bridge', 'Road & Bridge', $1)",
    )
    .bind(rb)
    .execute(&mut *tx)
    .await
    .unwrap();
    let c2 = core.clone();
    let task = tokio::spawn(async move { entra_sync::run(&c2, &system()).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the sync must wait for the mapping");
    tx.commit().await.unwrap();
    task.await.unwrap().unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
}

/// Nine people synced, then the group narrowed to P1 and P2: P3 to P9 are
/// held back. Returns the core with the group set and the held-back IDs.
async fn seven_held_back(m: &MockGraph, core: &Core) -> (Core, Vec<Uuid>) {
    for i in 1..=9 {
        let (name, mail) = (format!("P{i}"), format!("p{i}@county.example"));
        m.add_user(member(&oid(i), &name, &mail, None));
    }
    entra_sync::run(core, &system()).await.unwrap();
    m.add_group(GROUP_P, "PDMS");
    m.add_to_group(GROUP_P, &oid(1));
    m.add_to_group(GROUP_P, &oid(2));
    let grouped = with_group(core, GROUP_P).await;
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!((r.departed, r.held_back.len()), (0, 7), "{r:?}");
    (grouped, r.held_back)
}

#[tokio::test]
async fn depart_these_people_departs_only_those_still_outside() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let (grouped, held) = seven_held_back(&m, &core).await;
    let (p3, p4) = (
        id_of(&core, &oid(3)).await.unwrap(),
        id_of(&core, &oid(4)).await.unwrap(),
    );

    // The page lists them by name, all still active.
    let listed = entra_sync::people(&core.db, &held).await.unwrap();
    let names: Vec<&str> = listed.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["P3", "P4", "P5", "P6", "P7", "P8", "P9"]);
    assert!(listed.iter().all(|p| p.active && p.roles.is_empty()));
    assert_eq!(listed[0].email, "p3@county.example");

    // A clerk may not confirm.
    let (clerk, _) = user_in_new_department(&core, "HR", "hr@county.example").await;
    assert!(matches!(
        entra_sync::depart_held_back(&grouped, &as_user(&core, clerk).await).await,
        Err(CoreError::Forbidden(_))
    ));

    // Since the run, P3 is back in the group and P4 was departed by hand.
    m.add_to_group(GROUP_P, &oid(3));
    directory::set_user_status(&core.db, &system(), p4, UserStatus::Departed)
        .await
        .unwrap();

    let (n, problems) = entra_sync::depart_held_back(&grouped, &system())
        .await
        .unwrap();
    assert_eq!((n, problems), (5, Vec::<String>::new()));
    assert_eq!(status_of(&core, p3).await, "active");
    assert!(!group_mark(&core, p4).await, "departed by hand, not marked");
    for i in 5..=9 {
        let id = id_of(&core, &oid(i)).await.unwrap();
        assert_eq!(status_of(&core, id).await, "departed");
        assert!(group_mark(&core, id).await);
        assert_eq!(status_audits(&core, id).await, 1);
        assert_eq!(
            last_status_reason(&core, id).await.as_deref(),
            Some("is no longer in the PDMS group")
        );
    }
    for i in 1..=2 {
        let id = id_of(&core, &oid(i)).await.unwrap();
        assert_eq!(status_of(&core, id).await, "active");
    }
    let st = entra_sync::status(&core.db).await.unwrap();
    assert!(st.last_summary.unwrap().held_back.is_empty());

    // Pressed again: nobody is waiting.
    assert_eq!(
        entra_sync::depart_held_back(&grouped, &system())
            .await
            .unwrap(),
        (0, vec![])
    );
}

#[tokio::test]
async fn depart_these_people_waits_for_a_running_sync() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let (grouped, held) = seven_held_back(&m, &core).await;
    let mut tx = core.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(entra_sync::LOCK_KEY)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(matches!(
        entra_sync::depart_held_back(&grouped, &system()).await,
        Err(CoreError::Conflict(_))
    ));
    tx.rollback().await.unwrap();
    for id in &held {
        assert_eq!(status_of(&core, *id).await, "active");
    }
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().held_back, held, "still waiting");
}

#[tokio::test]
async fn depart_these_people_changes_nothing_when_the_group_cannot_be_read_or_is_empty() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let (grouped, held) = seven_held_back(&m, &core).await;

    // The member list can't be read: refused, with the plain message.
    m.forbid_groups(true);
    match entra_sync::depart_held_back(&grouped, &system()).await {
        Err(CoreError::Validation(msg)) => assert_eq!(msg, entra_sync::MISSING_PERMISSION),
        other => panic!("{other:?}"),
    }
    m.forbid_groups(false);

    // The group lists no members now: nobody is departed.
    m.remove_from_group(GROUP_P, &oid(1));
    m.remove_from_group(GROUP_P, &oid(2));
    assert!(matches!(
        entra_sync::depart_held_back(&grouped, &system()).await,
        Err(CoreError::Validation(_))
    ));

    // Without a group set, there is nothing to confirm.
    assert!(matches!(
        entra_sync::depart_held_back(&core, &system()).await,
        Err(CoreError::Validation(_))
    ));
    for id in &held {
        assert_eq!(status_of(&core, *id).await, "active");
    }
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().held_back, held, "still waiting");
}

#[tokio::test]
async fn a_sign_in_reactivation_is_listed_in_the_last_report() {
    use pdms_core::auth::oidc::resolve_identity;
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_group(GROUP_P, "PDMS");
    for (oid, name, mail) in [
        (OID_A, "Alice", "alice@county.example"),
        (OID_B, "Bob", "bob@county.example"),
    ] {
        m.add_user(member(oid, name, mail, None));
        m.add_to_group(GROUP_P, oid);
    }
    let grouped = with_group(&core, GROUP_P).await;
    let sign_in = |core: Core| async move {
        resolve_identity(&core, ISSUER, OID_A, None, None)
            .await
            .unwrap()
    };

    // Before any run there is no report to add to, and none is made.
    let (a, _) = sign_in(grouped.clone()).await.unwrap();
    assert!(
        directory::depart_from_group(&core.db, &system(), a)
            .await
            .unwrap()
    );
    assert_eq!(sign_in(grouped.clone()).await, Ok((a, "Alice".to_owned())));
    let st = entra_sync::status(&core.db).await.unwrap();
    assert!(st.last_summary.is_none(), "{st:?}");

    // The last run reactivated Bob.
    entra_sync::run(&grouped, &system()).await.unwrap();
    let b = id_of(&core, OID_B).await.unwrap();
    assert!(
        directory::depart_from_group(&core.db, &system(), b)
            .await
            .unwrap()
    );
    let r = entra_sync::run(&grouped, &system()).await.unwrap();
    assert_eq!(r.reactivated, vec![b], "{r:?}");

    // Alice is reactivated when she signs in: added to that run's list.
    assert!(
        directory::depart_from_group(&core.db, &system(), a)
            .await
            .unwrap()
    );
    assert_eq!(sign_in(grouped.clone()).await, Ok((a, "Alice".to_owned())));
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().reactivated, vec![b, a]);

    // Signing in again, already active, adds nothing.
    assert_eq!(sign_in(grouped.clone()).await, Ok((a, "Alice".to_owned())));
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().reactivated, vec![b, a]);
}
