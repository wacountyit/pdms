// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Settings in Administration (S1): where values come from, saving,
//! encryption, audit, and applying them in every process.

mod common;

use std::time::Duration;

use common::*;
use pdms_core::settings::{self, Change, NewValue, Source};
use pdms_core::{Config, Core, CoreError};
use serde_json::json;

fn set(key: &str, value: serde_json::Value) -> Change {
    Change {
        key: key.into(),
        value: NewValue::Set(value),
    }
}

/// A second `Core` on the same database, as another process would have.
fn other_process(core: &Core) -> Core {
    Core::new(core.db.clone(), (*core.file_config()).clone()).unwrap()
}

async fn stored_settings(core: &Core) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM app_settings WHERE key NOT LIKE 'branding.%')
              + (SELECT count(*) FROM app_secrets)",
    )
    .fetch_one(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_saved_setting_applies_at_once_and_is_audited() {
    let (core, _db) = core().await;
    assert_eq!(core.config().email.smtp_port, 587);
    settings::save(&core, &system(), vec![set("email.smtp_port", json!(2525))])
        .await
        .unwrap();
    assert_eq!(
        core.config().email.smtp_port,
        2525,
        "applied without a restart"
    );
    assert!(
        audit_actions(&core)
            .await
            .contains(&"settings.changed".to_owned())
    );
    let sources = settings::sources(&core).await.unwrap();
    assert_eq!(sources["email.smtp_port"], Source::Database);
    assert_eq!(sources["email.smtp_host"], Source::Default);

    // Clearing it brings the default back.
    settings::save(
        &core,
        &system(),
        vec![Change {
            key: "email.smtp_port".into(),
            value: NewValue::Clear,
        }],
    )
    .await
    .unwrap();
    assert_eq!(core.config().email.smtp_port, 587);
}

#[tokio::test]
async fn a_setting_in_the_configuration_file_wins_and_cannot_be_changed() {
    let (core, _db) = core().await;
    let mut file: Config = (*core.file_config()).clone();
    file.email.smtp_host = "mail.example.org".into();
    file.locked.insert("email.smtp_host".into());
    let core = Core::new(core.db.clone(), file).unwrap();
    let err = settings::save(
        &core,
        &system(),
        vec![set("email.smtp_host", json!("other"))],
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("configuration file"), "{err}");
    assert_eq!(core.config().email.smtp_host, "mail.example.org");
    assert_eq!(
        settings::sources(&core).await.unwrap()["email.smtp_host"],
        Source::File
    );
    assert_eq!(stored_settings(&core).await, 0);
}

#[tokio::test]
async fn an_invalid_combination_is_refused_and_nothing_is_stored() {
    let (core, _db) = core().await;
    // A tenant without a client ID and certificate is a partial [graph].
    let err = settings::save(
        &core,
        &system(),
        vec![set(
            "graph.tenant_id",
            json!("01234567-89ab-cdef-0123-456789abcdef"),
        )],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "{err:?}");
    assert_eq!(stored_settings(&core).await, 0);
    assert!(
        !audit_actions(&core)
            .await
            .contains(&"settings.changed".to_owned())
    );
    // An unknown key and a wrong type are refused too.
    assert!(
        settings::save(
            &core,
            &system(),
            vec![set("server.bind", json!("0.0.0.0:1"))]
        )
        .await
        .is_err()
    );
    assert!(
        settings::save(&core, &system(), vec![set("email.smtp_port", json!("x"))])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn secrets_are_encrypted_and_never_in_the_audit_log() {
    let (core, _db) = core().await;
    settings::save(
        &core,
        &system(),
        vec![set("email.smtp_password", json!("correct-horse-42"))],
    )
    .await
    .unwrap();
    assert_eq!(
        core.config().email.smtp_password.expose(),
        "correct-horse-42"
    );
    let raw: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM app_secrets WHERE key = 'email.smtp_password'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert!(
        !String::from_utf8_lossy(&raw).contains("correct-horse-42"),
        "encrypted at rest"
    );
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log a WHERE a::text LIKE '%correct-horse-42%'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
    let in_settings: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_settings WHERE value::text LIKE '%correct-horse%'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(in_settings, 0);
}

#[tokio::test]
async fn only_settings_managers_can_save() {
    let (core, _db) = core().await;
    let (clerk, _) = user_in_new_department(&core, "RB", "clerk@x.org").await;
    let ctx = as_user(&core, clerk).await;
    let err = settings::save(&core, &ctx, vec![set("email.smtp_port", json!(2525))])
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Forbidden(_)), "{err:?}");
}

#[tokio::test]
async fn another_process_picks_the_change_up() {
    let (core, _db) = core().await;
    let other = other_process(&core);
    let mut seen = settings::watch(other.clone());
    settings::save(&core, &system(), vec![set("email.smtp_port", json!(2525))])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), seen.changed())
        .await
        .expect("the notification arrives")
        .unwrap();
    assert_eq!(other.config().email.smtp_port, 2525);
    // A process that starts later reads the stored settings at once.
    let late = other_process(&core);
    assert!(late.reload().await.unwrap());
    assert_eq!(late.config().email.smtp_port, 2525);
}

#[tokio::test]
async fn settings_that_cannot_be_applied_leave_the_running_ones_alone() {
    let (core, db) = core().await;
    // Stored outside the checks (as a hand edit of the database would be).
    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES ('email.smtp_port', '\"not a port\"')",
    )
    .execute(&db.owner)
    .await
    .unwrap();
    sqlx::query("UPDATE settings_version SET version = version + 1")
        .execute(&db.owner)
        .await
        .unwrap();
    assert!(core.reload().await.is_err());
    assert_eq!(core.config().email.smtp_port, 587, "the old settings stay");
}

#[tokio::test]
async fn the_worker_restarts_on_a_settings_change_and_still_stops_cleanly() {
    let (core, _db) = core().await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let (changed, settings) = tokio::sync::watch::channel(0i64);
    let worker = tokio::spawn(pdms_core::maintenance::run_worker(
        core.clone(),
        stopped,
        settings,
    ));
    changed.send(1).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    changed.send(2).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !worker.is_finished(),
        "a settings change restarts it, never stops it"
    );
    // The settings watcher going away keeps it running as it is.
    drop(changed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!worker.is_finished());
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("it stops on shutdown")
        .unwrap();
}
