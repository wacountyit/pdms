// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use common::{audit_actions, system, user_in_new_department};
use pdms_core::auth::local::{LoginError, LoginFailure, login};
use pdms_core::config::Secret;
use pdms_core::{ClientInfo, Config, Core, CoreError, directory};
use pdms_testkit::TestDb;
use uuid::Uuid;

const PW: &str = "correct horse battery staple";

/// A core whose local sign-in does not require an authenticator code.
async fn core_no_totp() -> (Core, TestDb) {
    let db = pdms_testkit::test_db().await;
    let mut config = Config::default();
    config.secret_key = Secret::new(common::MASTER_KEY);
    config.auth.local.require_totp = false;
    let dir = std::env::temp_dir().join(format!("pdms-test-{}", Uuid::new_v4()));
    config.storage.local_path = dir.join("blobs");
    config.storage.spool_path = dir.join("spool");
    let core = Core::new(db.app.clone(), config).unwrap();
    (core, db)
}

async fn local_user(core: &Core, code: &str, email: &str) -> Uuid {
    let (user, _) = user_in_new_department(core, code, email).await;
    directory::set_local_credentials(&core.db, &core.keys, &system(), user, PW, None)
        .await
        .unwrap();
    user
}

fn username_set_count(actions: &[String]) -> usize {
    actions.iter().filter(|a| *a == "user.username_set").count()
}

#[tokio::test]
async fn set_username_validates_and_is_unique_ignoring_case() {
    let (core, _db) = core_no_totp().await;
    let u1 = local_user(&core, "A", "one@example.org").await;
    let u2 = local_user(&core, "B", "two@example.org").await;
    let set = |id: Uuid, v: Option<&str>| {
        let db = core.db.clone();
        let v = v.map(str::to_owned);
        async move { directory::set_username(&db, &system(), id, v.as_deref()).await }
    };

    set(u1, Some("sysadmin")).await.unwrap();
    assert_eq!(username_set_count(&audit_actions(&core).await), 1);
    // Same value again: no change, no audit.
    set(u1, Some("sysadmin")).await.unwrap();
    assert_eq!(username_set_count(&audit_actions(&core).await), 1);

    match set(u2, Some("SysAdmin")).await {
        Err(CoreError::Conflict(m)) => assert_eq!(m, "That username is already in use."),
        other => panic!("expected conflict, got {other:?}"),
    }
    let long = "a".repeat(65);
    for bad in ["has@sign", "a", "has space", long.as_str()] {
        assert!(
            matches!(set(u2, Some(bad)).await, Err(CoreError::Validation(_))),
            "{bad}"
        );
    }
    assert_eq!(username_set_count(&audit_actions(&core).await), 1);

    let got = directory::get_user(&core.db, u1).await.unwrap();
    assert_eq!(got.user.username.as_deref(), Some("sysadmin"));
    set(u1, None).await.unwrap();
    assert_eq!(username_set_count(&audit_actions(&core).await), 2);
    set(u1, Some("  ")).await.unwrap();
    assert_eq!(username_set_count(&audit_actions(&core).await), 2);
    let got = directory::get_user(&core.db, u1).await.unwrap();
    assert_eq!(got.user.username, None);
    // Freed name can be reused.
    set(u2, Some("SysAdmin")).await.unwrap();
}

#[tokio::test]
async fn username_requires_local_credentials() {
    let (core, _db) = core_no_totp().await;
    let (user, _) = user_in_new_department(&core, "A", "sso@example.org").await;
    match directory::set_username(&core.db, &system(), user, Some("someone")).await {
        Err(CoreError::Validation(m)) => {
            assert_eq!(m, "Only accounts with local sign-in can have a username.");
        }
        other => panic!("expected validation, got {other:?}"),
    }
    assert!(matches!(
        directory::set_username(&core.db, &system(), Uuid::new_v4(), Some("someone")).await,
        Err(CoreError::NotFound)
    ));
}

#[tokio::test]
async fn login_by_username_any_case_and_by_email() {
    let (core, _db) = core_no_totp().await;
    let u = local_user(&core, "A", "admin@example.org").await;
    directory::set_username(&core.db, &system(), u, Some("sysadmin"))
        .await
        .unwrap();
    let c = ClientInfo::default();
    login(&core, "SYSADMIN", PW, "", &c).await.unwrap();
    login(&core, "sysadmin", PW, "", &c).await.unwrap();
    login(&core, "Admin@Example.org", PW, "", &c).await.unwrap();
    // A username is not usable as an email lookup, and vice versa.
    assert!(
        login(&core, "sysadmin@example.org", PW, "", &c)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unknown_username_and_wrong_password_both_fail_generically() {
    let (core, _db) = core_no_totp().await;
    let u = local_user(&core, "A", "admin@example.org").await;
    directory::set_username(&core.db, &system(), u, Some("sysadmin"))
        .await
        .unwrap();
    let c = ClientInfo::default();
    let unknown = login(&core, "nobody", PW, "", &c).await;
    let wrong = login(&core, "sysadmin", "not the password at all", "", &c).await;
    assert!(matches!(
        unknown,
        Err(LoginError::Failed(LoginFailure::UnknownUser))
    ));
    assert!(matches!(
        wrong,
        Err(LoginError::Failed(LoginFailure::BadPassword))
    ));
}

#[tokio::test]
async fn check_username_refuses_taken_and_bad_names() {
    let (core, _db) = core_no_totp().await;
    let u1 = local_user(&core, "A", "one@example.org").await;
    let u2 = local_user(&core, "B", "two@example.org").await;
    directory::set_username(&core.db, &system(), u1, Some("sysadmin"))
        .await
        .unwrap();
    match directory::check_username(&core.db, "SysAdmin", Some(u2)).await {
        Err(CoreError::Conflict(m)) => assert_eq!(m, "That username is already in use."),
        other => panic!("expected conflict, got {other:?}"),
    }
    assert!(matches!(
        directory::check_username(&core.db, "SysAdmin", None).await,
        Err(CoreError::Conflict(_))
    ));
    for bad in ["has@sign", "a", &"a".repeat(65)] {
        assert!(matches!(
            directory::check_username(&core.db, bad, None).await,
            Err(CoreError::Validation(_))
        ));
    }
    // The owner may keep its own name; blank means none.
    assert_eq!(
        directory::check_username(&core.db, " SysAdmin ", Some(u1))
            .await
            .unwrap()
            .as_deref(),
        Some("SysAdmin")
    );
    assert_eq!(
        directory::check_username(&core.db, "  ", None)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn removing_local_credentials_clears_the_username() {
    let (core, _db) = core_no_totp().await;
    let u = local_user(&core, "A", "one@example.org").await;
    directory::set_username(&core.db, &system(), u, Some("sysadmin"))
        .await
        .unwrap();
    directory::remove_local_credentials(&core.db, &system(), u)
        .await
        .unwrap();
    assert_eq!(
        directory::get_user(&core.db, u)
            .await
            .unwrap()
            .user
            .username,
        None
    );
    assert_eq!(username_set_count(&audit_actions(&core).await), 2);
    // The name is free again.
    let u2 = local_user(&core, "B", "two@example.org").await;
    directory::set_username(&core.db, &system(), u2, Some("sysadmin"))
        .await
        .unwrap();
}

#[tokio::test]
async fn clearing_a_username_needs_no_local_credentials() {
    let (core, _db) = core_no_totp().await;
    let (u, _) = user_in_new_department(&core, "A", "sso@example.org").await;
    directory::set_username(&core.db, &system(), u, None)
        .await
        .unwrap();
    assert_eq!(username_set_count(&audit_actions(&core).await), 0);
}
