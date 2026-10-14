#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod common;

use common::*;
use pdms_audit::AuthMethod;
use pdms_core::auth::local::{LoginError, LoginFailure, login};
use pdms_core::auth::{principal, session, totp};
use pdms_core::model::UserStatus;
use pdms_core::{ClientInfo, directory};

const PW: &str = "correct horse battery staple";

async fn local_user(core: &pdms_core::Core) -> (uuid::Uuid, Vec<u8>) {
    let (user, _) = user_in_new_department(core, "IT", "ada@example.org").await;
    let secret = totp::generate_secret();
    directory::set_local_credentials(&core.db, &core.keys, &system(), user, PW, Some(&secret))
        .await
        .unwrap();
    (user, secret)
}

fn now_code(secret: &[u8]) -> String {
    totp::current_code(secret, chrono::Utc::now().timestamp() as u64).unwrap()
}

fn failure(r: Result<impl std::fmt::Debug, LoginError>) -> LoginFailure {
    match r {
        Err(LoginError::Failed(f)) => f,
        other => panic!("expected failure, got {other:?}"),
    }
}

#[tokio::test]
async fn local_login_succeeds_with_password_and_totp() {
    let (core, _db) = core().await;
    let (user, secret) = local_user(&core).await;
    let ok = login(
        &core,
        "ADA@example.org",
        PW,
        &now_code(&secret),
        &ClientInfo::default(),
    )
    .await
    .unwrap();
    assert_eq!(ok.user_id, user);
    let s = session::lookup(&core.db, &core.config.auth, &ok.token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.user_id, user);
    assert_eq!(s.auth_method, AuthMethod::Local);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"auth.login".to_owned())
    );
}

#[tokio::test]
async fn totp_codes_cannot_be_replayed() {
    let (core, _db) = core().await;
    let (_, secret) = local_user(&core).await;
    let code = now_code(&secret);
    login(&core, "ada@example.org", PW, &code, &ClientInfo::default())
        .await
        .unwrap();
    let r = login(&core, "ada@example.org", PW, &code, &ClientInfo::default()).await;
    assert_eq!(failure(r), LoginFailure::BadTotp);
}

#[tokio::test]
async fn failures_lock_the_account_and_are_audited() {
    let (core, _db) = core().await;
    let (_, secret) = local_user(&core).await;
    for _ in 0..3 {
        let r = login(
            &core,
            "ada@example.org",
            "wrong password!!",
            "000000",
            &ClientInfo::default(),
        )
        .await;
        assert_eq!(failure(r), LoginFailure::BadPassword);
    }
    let r = login(
        &core,
        "ada@example.org",
        PW,
        &now_code(&secret),
        &ClientInfo::default(),
    )
    .await;
    assert_eq!(
        failure(r),
        LoginFailure::Locked,
        "correct credentials rejected while locked"
    );
    let actions = audit_actions(&core).await;
    assert_eq!(
        actions.iter().filter(|a| *a == "auth.login_failed").count(),
        4
    );
    assert!(actions.contains(&"auth.account_locked".to_owned()));
}

#[tokio::test]
async fn unknown_users_and_missing_totp_fail_generically() {
    let (core, _db) = core().await;
    let r = login(
        &core,
        "nobody@example.org",
        PW,
        "123456",
        &ClientInfo::default(),
    )
    .await;
    assert_eq!(failure(r), LoginFailure::UnknownUser);
    let (user, _) = user_in_new_department(&core, "HR", "pw-only@example.org").await;
    directory::set_local_credentials(&core.db, &core.keys, &system(), user, PW, None)
        .await
        .unwrap();
    let r = login(&core, "pw-only@example.org", PW, "", &ClientInfo::default()).await;
    assert_eq!(failure(r), LoginFailure::TotpNotEnrolled);
}

#[tokio::test]
async fn deactivation_revokes_sessions_immediately() {
    let (core, _db) = core().await;
    let (user, secret) = local_user(&core).await;
    let ok = login(
        &core,
        "ada@example.org",
        PW,
        &now_code(&secret),
        &ClientInfo::default(),
    )
    .await
    .unwrap();
    directory::set_user_status(&core.db, &system(), user, UserStatus::Disabled)
        .await
        .unwrap();
    assert!(
        session::lookup(&core.db, &core.config.auth, &ok.token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!principal::load(&core.db, user).await.unwrap().active);
}

#[tokio::test]
async fn principal_reflects_memberships_and_roles() {
    let (core, _db) = core().await;
    let (user, dept) = user_in_new_department(&core, "OPS", "r@example.org").await;
    let head = directory::role_id_by_key(&core.db, "department_head")
        .await
        .unwrap();
    let assignment = directory::assign_role(&core.db, &system(), user, head, Some(dept))
        .await
        .unwrap();
    let p = principal::load(&core.db, user).await.unwrap();
    assert!(p.departments.contains(&dept));
    assert_eq!(p.roles.len(), 1);
    assert_eq!(p.roles[0].department_id, Some(dept));
    directory::revoke_role(&core.db, &system(), assignment)
        .await
        .unwrap();
    assert!(
        principal::load(&core.db, user)
            .await
            .unwrap()
            .roles
            .is_empty()
    );
}
