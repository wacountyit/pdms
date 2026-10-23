// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Microsoft sign-in settings in Administration (S1 Phase 4): the tenant
//! field, the saved secret, and the rule that a change can't lock everyone
//! out. The full sign-in against a mock provider is in the web tests.

mod common;

use common::*;
use pdms_core::auth::oidc_setup::{self, SignInForm};
use pdms_core::{Config, Core, CoreError};

const TENANT: &str = "01234567-89ab-cdef-0123-456789abcdef";
/// Nothing listens here, so discovery fails at once.
const DEAD_ISSUER: &str = "http://127.0.0.1:9/nobody";

fn form() -> SignInForm {
    SignInForm {
        enabled: true,
        tenant: TENANT.into(),
        client_id: "11234567-89ab-cdef-0123-456789abcdef".into(),
        client_secret: "s3cret-value".into(),
        display_name: "Contoso".into(),
        auto_provision: false,
    }
}

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.file_config()).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

#[test]
fn the_tenant_field_takes_a_tenant_id_or_an_issuer_url() {
    let issuer = oidc_setup::issuer_for(&TENANT.to_uppercase()).unwrap();
    assert_eq!(
        issuer,
        format!("https://login.microsoftonline.com/{TENANT}/v2.0")
    );
    assert_eq!(oidc_setup::tenant_of(&issuer), Some(TENANT));
    assert_eq!(
        oidc_setup::issuer_for("https://idp.example.org/realm").unwrap(),
        "https://idp.example.org/realm"
    );
    assert_eq!(oidc_setup::tenant_of("https://idp.example.org/realm"), None);
    assert!(oidc_setup::issuer_for("contoso").is_err());
    assert!(oidc_setup::issuer_for("http://idp.example.org/realm").is_err());
    assert_eq!(oidc_setup::issuer_for(" ").unwrap(), "");
}

#[tokio::test]
async fn saving_applies_at_once_and_keeps_the_secret_when_none_is_typed() {
    let (core, _db) = core().await;
    oidc_setup::save(&core, &system(), &form()).await.unwrap();
    let o = core.config().auth.oidc.clone();
    assert!(o.enabled);
    assert_eq!(
        o.issuer_url,
        format!("https://login.microsoftonline.com/{TENANT}/v2.0")
    );
    assert_eq!(o.client_secret.expose(), "s3cret-value");
    assert_eq!(o.display_name, "Contoso");
    assert!(!o.auto_provision);

    let mut again = form();
    again.client_secret.clear();
    again.display_name = "Microsoft".into();
    oidc_setup::save(&core, &system(), &again).await.unwrap();
    assert_eq!(
        core.config().auth.oidc.client_secret.expose(),
        "s3cret-value"
    );
    assert_eq!(core.config().auth.oidc.display_name, "Microsoft");
    let leaked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log a WHERE a::text LIKE '%s3cret-value%'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(leaked, 0);
}

#[tokio::test]
async fn values_in_the_configuration_file_are_left_alone() {
    let (core, _db) = core_with(|c| {
        c.auth.oidc.client_id = "from-the-file".into();
        c.locked.insert("auth.oidc.client_id".into());
    })
    .await;
    oidc_setup::save(&core, &system(), &form()).await.unwrap();
    assert_eq!(core.config().auth.oidc.client_id, "from-the-file");
    assert!(core.config().auth.oidc.enabled);
}

#[tokio::test]
async fn the_test_reports_a_provider_it_cannot_reach_and_saves_nothing() {
    let (core, _db) = core().await;
    let mut f = form();
    f.tenant = DEAD_ISSUER.into();
    let t = oidc_setup::test(&core, &system(), &f).await.unwrap();
    assert!(!t.ok);
    assert!(t.lines.last().unwrap().1.contains("couldn't read"), "{t:?}");
    assert!(!core.config().auth.oidc.enabled);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"settings.tested".to_owned())
    );
    f.client_secret.clear();
    let t = oidc_setup::test(&core, &system(), &f).await.unwrap();
    assert!(t.lines[0].1.contains("client secret"), "{t:?}");
}

#[tokio::test]
async fn with_local_sign_in_off_a_change_cannot_lock_everyone_out() {
    let (core, _db) = core_with(|c| {
        c.auth.local.enabled = false;
        c.auth.oidc.enabled = true;
        c.auth.oidc.issuer_url = DEAD_ISSUER.into();
        c.auth.oidc.client_id = "client".into();
    })
    .await;
    let mut off = form();
    off.enabled = false;
    let err = oidc_setup::save(&core, &system(), &off).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "{err:?}");
    assert!(err.to_string().contains("can't be turned off"), "{err}");

    // A change that fails the test is refused too.
    let mut f = form();
    f.tenant = DEAD_ISSUER.into();
    let err = oidc_setup::save(&core, &system(), &f).await.unwrap_err();
    assert!(err.to_string().contains("passes the test"), "{err}");
    assert_eq!(core.config().auth.oidc.client_id, "client", "nothing saved");
}

#[tokio::test]
async fn only_settings_managers_can_change_sign_in() {
    let (core, _db) = core().await;
    let (clerk, _) = user_in_new_department(&core, "RB", "clerk@x.org").await;
    let ctx = as_user(&core, clerk).await;
    for err in [
        oidc_setup::save(&core, &ctx, &form()).await.unwrap_err(),
        oidc_setup::test(&core, &ctx, &form()).await.unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::Forbidden(_)), "{err:?}");
    }
}
