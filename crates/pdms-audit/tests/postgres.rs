#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Audit log behaviour against a real Postgres with the production grants.

use std::net::{IpAddr, Ipv4Addr};

use pdms_audit::*;
use pdms_testkit::test_db;
use sqlx::PgPool;

fn event(n: i64) -> AuditEvent {
    let mut actor = Actor::system("test");
    actor.ip = Some(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)));
    actor.user_agent = Some("tests".into());
    AuditEvent::new(
        Category::Metadata,
        "document.metadata_updated",
        actor,
        "document",
    )
    .object(uuid::Uuid::now_v7(), Some("Test document"))
    .change("renewal_date", "2026-01-01", "2027-01-01")
    .change("owner", "alice", "bob")
    .context("n", n)
    .context(
        "nested",
        AuditValue::List(vec!["a".into(), 1.into(), AuditValue::Null]),
    )
}

async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn append_round_trips_and_verifies() {
    let db = test_db().await;
    let mut tx = db.app.begin().await.unwrap();
    let sealed = append(&mut tx, vec![event(1), event(2)]).await.unwrap();
    tx.commit().await.unwrap();
    record(&db.app, vec![event(3)]).await.unwrap();

    assert_eq!(sealed.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![1, 2]);
    let report = verify(&db.app).await.unwrap();
    assert!(report.is_intact(), "{:?}", report.breaks);
    assert_eq!(report.entries_checked, 3);
    assert_eq!(report.last.seq, 3);
}

#[tokio::test]
async fn rolled_back_entries_leave_no_gap() {
    let db = test_db().await;
    record(&db.app, vec![event(1)]).await.unwrap();
    let mut tx = db.app.begin().await.unwrap();
    append(&mut tx, vec![event(2)]).await.unwrap();
    tx.rollback().await.unwrap();
    record(&db.app, vec![event(3)]).await.unwrap();
    let report = verify(&db.app).await.unwrap();
    assert!(report.is_intact(), "{:?}", report.breaks);
    assert_eq!(report.last.seq, 2);
}

#[tokio::test]
async fn app_role_cannot_modify_audit_rows() {
    let db = test_db().await;
    record(&db.app, vec![event(1)]).await.unwrap();
    for sql in [
        "UPDATE audit_log SET action = 'auth.logout'",
        "DELETE FROM audit_log",
        "TRUNCATE audit_log",
        "UPDATE audit_chain_head SET seq = 0",
        "DELETE FROM audit_chain_head",
        "ALTER TABLE audit_log DISABLE TRIGGER ALL",
        "DROP TABLE audit_log",
    ] {
        let err = sqlx::query(sql).execute(&db.app).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("permission denied") || msg.contains("must be owner"),
            "{sql}: {msg}"
        );
    }
    assert_eq!(count(&db.app).await, 1);
}

#[tokio::test]
async fn owner_role_is_blocked_by_triggers() {
    let db = test_db().await;
    record(&db.app, vec![event(1)]).await.unwrap();
    for sql in [
        "UPDATE audit_log SET action = 'auth.logout'",
        "DELETE FROM audit_log",
        "TRUNCATE audit_log",
        "DELETE FROM audit_chain_head",
    ] {
        let err = sqlx::query(sql).execute(&db.owner).await.unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
}

#[tokio::test]
async fn out_of_chain_insert_is_rejected() {
    let db = test_db().await;
    record(&db.app, vec![event(1)]).await.unwrap();
    // Forge a row that claims to follow genesis.
    let forged = seal(event(2), ChainHead::GENESIS);
    let err = sqlx::query(
        "INSERT INTO audit_log (seq, id, occurred_at, category, action, outcome, actor_auth_method,
           object_type, context, hash_version, prev_hash, entry_hash)
         VALUES ($1, $2, $3, 'auth', 'auth.login', 'success', 'system', 'user', '{}', 1, $4, $5)",
    )
    .bind(5_i64)
    .bind(forged.event.id)
    .bind(forged.event.occurred_at)
    .bind(&forged.prev_hash[..])
    .bind(&forged.entry_hash[..])
    .execute(&db.app)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("audit chain mismatch"), "{err}");
}

#[tokio::test]
async fn superuser_tampering_is_detected() {
    let db = test_db().await;
    record(&db.app, (1..=5).map(event).collect()).await.unwrap();

    let mut tx = db.superuser.begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role = replica") // disables triggers
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET changes = '[{\"field\":\"owner\",\"before\":\"alice\",\"after\":\"mallory\"}]' WHERE seq = 3")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let report = verify(&db.app).await.unwrap();
    assert!(!report.is_intact());
    assert!(
        matches!(
            report.breaks.as_slice(),
            [Break {
                seq: 3,
                kind: BreakKind::EntryHashMismatch { .. }
            }]
        ),
        "{:?}",
        report.breaks
    );
}

#[tokio::test]
async fn superuser_deletion_is_detected() {
    let db = test_db().await;
    record(&db.app, (1..=5).map(event).collect()).await.unwrap();
    let mut tx = db.superuser.begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_log WHERE seq = 5")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let report = verify(&db.app).await.unwrap();
    assert!(
        report
            .breaks
            .iter()
            .any(|b| matches!(b.kind, BreakKind::HeadMismatch { .. })),
        "{:?}",
        report.breaks
    );
}

#[tokio::test]
async fn injected_float_is_reported_not_fatal() {
    let db = test_db().await;
    record(&db.app, (1..=3).map(event).collect()).await.unwrap();
    let mut tx = db.superuser.begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET context = '{\"n\": 1.5}' WHERE seq = 2")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let report = verify(&db.app).await.unwrap();
    assert!(matches!(
        report.breaks.as_slice(),
        [Break {
            seq: 2,
            kind: BreakKind::Undecodable(_)
        }]
    ));
}

#[tokio::test]
async fn concurrent_writers_keep_a_linear_chain() {
    let db = test_db().await;
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let pool = db.app.clone();
            tokio::spawn(async move {
                for j in 0..10 {
                    record(&pool, vec![event(i * 100 + j)]).await.unwrap();
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let report = verify(&db.app).await.unwrap();
    assert!(report.is_intact(), "{:?}", report.breaks);
    assert_eq!(report.entries_checked, 160);
}
