// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! The "PDMS Service" certificate made in PDMS, and the connection test (S1 Phase 2).

mod common;
mod mock_graph;

use common::*;
use mock_graph::{CLIENT, MockGraph, TENANT};
use pdms_core::graph::certs;
use pdms_core::settings::{self, Change, NewValue};
use pdms_core::{Config, Core, CoreError};
use serde_json::json;

fn ids(tenant: &str, client: &str) -> Vec<Change> {
    vec![
        Change {
            key: "graph.tenant_id".into(),
            value: NewValue::Set(json!(tenant)),
        },
        Change {
            key: "graph.client_id".into(),
            value: NewValue::Set(json!(client)),
        },
    ]
}

/// A core whose Microsoft endpoints are the mock (no certificate in the file).
async fn core_on(m: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut cfg: Config = (*core.file_config()).clone();
    let mock = m.config("");
    cfg.graph.authority_url = mock.authority_url;
    cfg.graph.api_url = mock.api_url;
    (Core::new(core.db.clone(), cfg).unwrap(), db)
}

#[tokio::test]
async fn the_first_certificate_is_used_once_the_ids_are_saved() {
    let (core, _db) = core().await;
    // The IDs alone are refused: there is nothing to sign in with.
    let err = settings::save(&core, &system(), ids(TENANT, CLIENT))
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "{err:?}");

    let c = certs::generate(&core, &system()).await.unwrap();
    assert_eq!(c.state, "active", "the first one is in use at once");
    assert_eq!(c.thumbprint_sha1.len(), 40);
    assert!(core.graph().is_none(), "no IDs yet: Graph stays off");
    let public = certs::public_certificate(&core, &system(), c.id)
        .await
        .unwrap();
    assert!(public.contains("BEGIN CERTIFICATE"));
    assert!(!public.contains("PRIVATE KEY"), "never the private key");

    settings::save(&core, &system(), ids(TENANT, CLIENT))
        .await
        .unwrap();
    let graph = core.graph().expect("Graph is on");
    assert_eq!(graph.thumbprint(), c.thumbprint_sha256);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"graph.certificate_created".to_owned())
    );
}

#[tokio::test]
async fn a_renewal_waits_until_it_is_switched_to() {
    let (core, _db) = core().await;
    let first = certs::generate(&core, &system()).await.unwrap();
    settings::save(&core, &system(), ids(TENANT, CLIENT))
        .await
        .unwrap();
    let second = certs::generate(&core, &system()).await.unwrap();
    assert_eq!(second.state, "pending");
    assert_eq!(
        core.graph().unwrap().thumbprint(),
        first.thumbprint_sha256,
        "the renewal waits"
    );
    // The certificate in use can't be discarded.
    assert!(certs::retire(&core, &system(), first.id).await.is_err());
    certs::activate(&core, &system(), second.id).await.unwrap();
    assert_eq!(core.graph().unwrap().thumbprint(), second.thumbprint_sha256);
    let listed = certs::list(&core.db).await.unwrap();
    assert_eq!(listed.len(), 1, "the old one is retired: {listed:?}");
    let old_key: i64 = sqlx::query_scalar("SELECT count(*) FROM app_secrets WHERE key = $1")
        .bind(format!("graph.certificate.{}", first.id))
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(old_key, 0, "its private key is deleted");
    // A pending one can be discarded.
    let third = certs::generate(&core, &system()).await.unwrap();
    certs::retire(&core, &system(), third.id).await.unwrap();
    assert_eq!(certs::list(&core.db).await.unwrap().len(), 1);
    let acts = audit_actions(&core).await;
    for a in ["graph.certificate_activated", "graph.certificate_retired"] {
        assert!(acts.contains(&a.to_owned()), "{a}");
    }
}

#[tokio::test]
async fn a_certificate_in_the_configuration_file_wins() {
    let (core, _db) = core().await;
    let file_cert = pdms_core::graph::cert::generate("File", 30).unwrap();
    let mut cfg: Config = (*core.file_config()).clone();
    cfg.graph.certificate = pdms_core::config::Secret::new(file_cert.pem.clone());
    let core = Core::new(core.db.clone(), cfg).unwrap();
    certs::generate(&core, &system()).await.unwrap();
    settings::save(&core, &system(), ids(TENANT, CLIENT))
        .await
        .unwrap();
    assert_eq!(core.graph().unwrap().thumbprint(), file_cert.thumbprint);
}

#[tokio::test]
async fn the_connection_test_signs_in_without_saving() {
    let m = MockGraph::start().await;
    let (core, _db) = core_on(&m).await;
    let none = certs::test_connection(&core, &system(), TENANT, CLIENT)
        .await
        .unwrap();
    assert!(!none.ok);
    assert!(none.lines[0].contains("Generate a certificate"), "{none:?}");

    certs::generate(&core, &system()).await.unwrap();
    let ok = certs::test_connection(&core, &system(), TENANT, CLIENT)
        .await
        .unwrap();
    assert!(ok.ok, "{ok:?}");
    assert!(ok.lines.iter().any(|l| l.contains("read users")), "{ok:?}");
    assert!(core.graph().is_none(), "a test saves nothing");

    let wrong = certs::test_connection(&core, &system(), TENANT, "someone-else")
        .await
        .unwrap();
    assert!(!wrong.ok);
    assert!(wrong.lines[0].contains("refused the sign-in"), "{wrong:?}");
    assert!(
        audit_actions(&core)
            .await
            .contains(&"settings.tested".to_owned())
    );
}

#[tokio::test]
async fn certificates_need_settings_managers() {
    let (core, _db) = core().await;
    let (clerk, _) = user_in_new_department(&core, "RB", "clerk@x.org").await;
    let ctx = as_user(&core, clerk).await;
    assert!(matches!(
        certs::generate(&core, &ctx).await,
        Err(CoreError::Forbidden(_))
    ));
}

#[tokio::test]
async fn administrators_are_reminded_before_the_certificate_expires() {
    let (core, _db) = core().await;
    let (boss, _) = user_in_new_department(&core, "IT", "boss@x.org").await;
    let role = pdms_core::directory::role_id_by_key(&core.db, "admin")
        .await
        .unwrap();
    pdms_core::directory::assign_role(&core.db, &system(), boss, role, None)
        .await
        .unwrap();
    let c = certs::generate(&core, &system()).await.unwrap();
    assert_eq!(c.state, "active");
    settings::save(&core, &system(), ids(TENANT, CLIENT))
        .await
        .unwrap();
    certs::remind_expiry(&core).await.unwrap();
    let count = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM notifications WHERE subject LIKE '%certificate expires soon%'",
        )
        .fetch_one(&core.db)
        .await
        .unwrap()
    };
    assert_eq!(count().await, 0, "two years left: no reminder");
    sqlx::query("UPDATE graph_certificates SET not_after = now() + interval '10 days'")
        .execute(&core.db)
        .await
        .unwrap();
    certs::remind_expiry(&core).await.unwrap();
    certs::remind_expiry(&core).await.unwrap();
    assert_eq!(count().await, 1, "once per certificate");
}
