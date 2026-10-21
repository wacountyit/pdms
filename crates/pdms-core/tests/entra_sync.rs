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

use common::*;
use pdms_core::CoreError;
use pdms_core::directory::{self, entra_values};

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
