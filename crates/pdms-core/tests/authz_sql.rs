#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! The SQL predicate must agree with the in-memory engine on every input.

mod common;

use std::collections::BTreeSet;

use chrono::Utc;
use pdms_authz::strategies::{self, DEPARTMENTS, DOCUMENTS, GROUPS, ROLES, USERS};
use pdms_authz::*;
use pdms_core::acl::push_fragment;
use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

/// Seed the fixed universe of departments, users, groups, and roles.
async fn setup(owner: &PgPool) {
    for d in DEPARTMENTS {
        sqlx::query("INSERT INTO departments (id, code, name) VALUES ($1, $2, $2)")
            .bind(d)
            .bind(format!("D{}", d.as_u128()))
            .execute(owner)
            .await
            .unwrap();
    }
    for u in USERS {
        sqlx::query("INSERT INTO users (id, display_name, email) VALUES ($1, $2, $2 || '@x.org')")
            .bind(u)
            .bind(format!("u{}", u.as_u128()))
            .execute(owner)
            .await
            .unwrap();
    }
    for g in GROUPS {
        sqlx::query("INSERT INTO groups (id, name) VALUES ($1, $2)")
            .bind(g)
            .bind(format!("g{}", g.as_u128()))
            .execute(owner)
            .await
            .unwrap();
    }
    for doc in DOCUMENTS {
        sqlx::query(
            "INSERT INTO documents (id, department_id, owner_id, external_id, document_type_id, title)
             VALUES ($1, $2, $3, $1::text, (SELECT id FROM document_types WHERE key = 'other'), 'Doc')",
        )
        .bind(doc)
        .bind(DEPARTMENTS[0])
        .bind(USERS[0])
        .execute(owner)
        .await
        .unwrap();
    }
    for r in ROLES {
        sqlx::query("INSERT INTO roles (id, key, name) VALUES ($1, $2, $2)")
            .bind(r)
            .bind(format!("role_{}", r.as_u128()))
            .execute(owner)
            .await
            .unwrap();
    }
}

async fn load_case(owner: &PgPool, grants: &[Grant], docs: &[DocumentAttrs]) {
    sqlx::query("DELETE FROM acl_entries")
        .execute(owner)
        .await
        .unwrap();
    for d in docs {
        sqlx::query(
            "INSERT INTO documents (id, department_id, owner_id, inherit_acl, is_confidential,
                                    external_id, document_type_id, title)
             VALUES ($1, $2, $3, $4, $5, $1::text, (SELECT id FROM document_types WHERE key = 'other'), 'Doc')
             ON CONFLICT (id) DO UPDATE SET department_id = EXCLUDED.department_id, owner_id = EXCLUDED.owner_id,
                inherit_acl = EXCLUDED.inherit_acl, is_confidential = EXCLUDED.is_confidential",
        )
            .bind(d.id)
            .bind(d.department_id)
            .bind(d.owner_id)
            .bind(d.inherit_acl)
            .bind(d.is_confidential)
            .execute(owner)
            .await
            .unwrap();
    }
    for g in grants {
        let (u, gr, de, r) = match g.subject {
            Subject::User(x) => (Some(x), None, None, None),
            Subject::Group(x) => (None, Some(x), None, None),
            Subject::Department(x) => (None, None, Some(x), None),
            Subject::Role(x) => (None, None, None, Some(x)),
        };
        let (scope, sd, sdoc) = match g.scope {
            Scope::Global => ("global", None, None),
            Scope::Department(x) => ("department", Some(x), None),
            Scope::Document(x) => ("document", None, Some(x)),
        };
        sqlx::query(
            "INSERT INTO acl_entries (id, subject_user_id, subject_group_id, subject_department_id, subject_role_id,
                                      scope, scope_department_id, scope_document_id, permission, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6::acl_scope, $7, $8, $9::permission, $10)",
        )
        .bind(g.id)
        .bind(u)
        .bind(gr)
        .bind(de)
        .bind(r)
        .bind(scope)
        .bind(sd)
        .bind(sdoc)
        .bind(g.permission.as_str())
        .bind(g.expires_at)
        .execute(owner)
        .await
        .unwrap();
    }
}

async fn visible(app: &PgPool, p: &Principal, perm: Permission) -> BTreeSet<Uuid> {
    let frag =
        visible_documents_predicate(&Policy::default(), p, perm, strategies::now(), "d").unwrap();
    let mut qb = QueryBuilder::new("SELECT d.id FROM documents d WHERE ");
    push_fragment(&mut qb, &frag);
    qb.build_query_scalar::<Uuid>()
        .fetch_all(app)
        .await
        .unwrap()
        .into_iter()
        .collect()
}

#[test]
fn sql_predicate_matches_engine() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(async {
        let db = pdms_testkit::test_db().await;
        setup(&db.owner).await;
        db
    });

    let strategy = (
        strategies::grants(),
        proptest::collection::vec(strategies::document(), 1..4),
        proptest::collection::vec(strategies::principal(), 1..4),
    );
    let allowed = std::sync::atomic::AtomicUsize::new(0);
    let denied = std::sync::atomic::AtomicUsize::new(0);
    let mut runner = TestRunner::new(PtConfig {
        cases: 300,
        ..PtConfig::default()
    });
    runner
        .run(&strategy, |(grants, docs, principals)| {
            // Documents share IDs in the small universe; keep the first of each.
            let mut seen = BTreeSet::new();
            let docs: Vec<DocumentAttrs> = docs.into_iter().filter(|d| seen.insert(d.id)).collect();
            rt.block_on(async {
                load_case(&db.owner, &grants, &docs).await;
                for p in &principals {
                    for perm in Permission::ALL {
                        let in_case: BTreeSet<Uuid> = docs.iter().map(|d| d.id).collect();
                        let sql: BTreeSet<Uuid> = visible(&db.app, p, perm)
                            .await
                            .intersection(&in_case)
                            .copied()
                            .collect();
                        let engine: BTreeSet<Uuid> = docs
                            .iter()
                            .filter(|d| {
                                decide(&Policy::default(), p, perm, d, &grants, strategies::now())
                                    .is_allowed()
                            })
                            .map(|d| d.id)
                            .collect();
                        prop_assert_eq!(&sql, &engine, "perm={} principal={:?}", perm, p);
                        allowed.fetch_add(engine.len(), std::sync::atomic::Ordering::Relaxed);
                        denied.fetch_add(
                            docs.len() - engine.len(),
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
                Ok(())
            })
        })
        .unwrap();
    let (allowed, denied) = (allowed.into_inner(), denied.into_inner());
    eprintln!(
        "equivalence: {allowed} allowed and {denied} denied (principal, permission, document) checks"
    );
    assert!(
        allowed > 500 && denied > 500,
        "generator too skewed: {allowed} allowed / {denied} denied"
    );
    let _ = (DOCUMENTS, Utc::now());
}
