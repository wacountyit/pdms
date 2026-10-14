#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Full OIDC authorization-code + PKCE flow against a mock IdP container.

mod support;

use std::net::SocketAddr;

use pdms_core::directory::{self, MappingTarget};
use pdms_core::{Core, auth::oidc::OidcProvider};
use support::*;
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const CLIENT_ID: &str = "pdms-test";

async fn mock_idp(claims: serde_json::Value) -> (ContainerAsync<GenericImage>, String) {
    let config = serde_json::json!({
        "interactiveLogin": false,
        "tokenCallbacks": [{
            "issuerId": "default",
            "tokenExpiry": 300,
            "requestMappings": [{ "requestParam": "grant_type", "match": "*", "claims": claims }]
        }]
    });
    let c = GenericImage::new("ghcr.io/navikt/mock-oauth2-server", "2.1.10")
        .with_exposed_port(8080.tcp())
        .with_env_var("JSON_CONFIG", config.to_string())
        .start()
        .await
        .expect("start mock-oauth2-server");
    let port = c.get_host_port_ipv4(8080).await.unwrap();
    let issuer = format!("http://127.0.0.1:{port}/default");
    // The image logs no readiness line; poll discovery instead.
    let http = reqwest::Client::new();
    for _ in 0..120 {
        let ok = http
            .get(format!("{issuer}/.well-known/openid-configuration"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if ok {
            return (c, issuer);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("mock IdP did not become ready");
}

/// Run the app on a real port so the IdP can redirect the "browser" back to it.
async fn serve(core: Core, issuer: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = (*core.config).clone();
    config.server.public_url = format!("http://{addr}").parse().unwrap();
    config.auth.oidc.enabled = true;
    config.auth.oidc.issuer_url = issuer.to_owned();
    config.auth.oidc.client_id = CLIENT_ID.into();
    config.auth.oidc.client_secret = pdms_core::config::Secret::new("secret");
    config.auth.oidc.subject_claim = "oid".into();
    let core = Core::new(core.db.clone(), config.clone()).unwrap();
    let provider = OidcProvider::discover(&config.auth.oidc, &config.server.public_url)
        .await
        .unwrap();
    let router = pdms_web::router(pdms_web::AppState::new(core, Some(provider), None));
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

fn browser() -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .unwrap()
}

#[tokio::test]
async fn first_sso_login_provisions_user_and_syncs_groups() {
    let app = app().await;
    let ops = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    directory::create_idp_mapping(
        &app.core.db,
        &system(),
        "grp-ops",
        "",
        MappingTarget::Department(ops),
    )
    .await
    .unwrap();
    let (_idp, issuer) = mock_idp(serde_json::json!({
        "sub": "pairwise-sub-1", "oid": "entra-oid-123", "aud": [CLIENT_ID],
        "email": "sso.user@org.example", "name": "Sso User", "groups": ["grp-ops", "grp-unmapped"]
    }))
    .await;
    let base = serve(app.core.clone(), &issuer).await;
    let b = browser();

    let login = b
        .get(format!("{base}/login"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(login.contains("Sign in with Microsoft"));

    let home = b
        .get(format!("{base}/auth/oidc/start?next=/"))
        .send()
        .await
        .unwrap();
    assert_eq!(home.status(), 200);
    assert!(
        home.url().as_str().starts_with(&base),
        "landed back on the app: {}",
        home.url()
    );
    let html = home.text().await.unwrap();
    assert!(html.contains("Welcome, Sso User"), "{html}");
    assert!(
        html.contains("Operations") || html.contains("Operations"),
        "department synced from groups claim"
    );

    let row: (String, String) = sqlx::query_as(
        "SELECT oidc_subject, email FROM users WHERE email = 'sso.user@org.example'",
    )
    .fetch_one(&app.core.db)
    .await
    .unwrap();
    assert_eq!(row.0, "entra-oid-123", "identity keyed on oid, not sub");
    let acts = actions(&app.core).await;
    for a in [
        "user.provisioned",
        "user.idp_memberships_synced",
        "auth.login",
    ] {
        assert!(acts.contains(&a.to_owned()), "missing {a}: {acts:?}");
    }

    // Second login: same user, no duplicate provisioning.
    let b2 = browser();
    let again = b2
        .get(format!("{base}/auth/oidc/start?next=/"))
        .send()
        .await
        .unwrap();
    assert!(again.text().await.unwrap().contains("Welcome, Sso User"));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(users, 1);
}

#[tokio::test]
async fn forged_state_is_rejected() {
    let app = app().await;
    let (_idp, issuer) = mock_idp(serde_json::json!({
        "sub": "pairwise-sub-9", "oid": "entra-oid-9", "aud": [CLIENT_ID], "email": "x@org.example", "name": "X"
    }))
    .await;
    let base = serve(app.core.clone(), &issuer).await;
    let b = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // Begin a real flow to get the login cookie, then present a callback with a different state.
    let start = b
        .get(format!("{base}/auth/oidc/start"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 303);
    let r = b
        .get(format!(
            "{base}/auth/oidc/callback?code=anything&state=forged"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let acts = actions(&app.core).await;
    assert!(acts.contains(&"auth.login_failed".to_owned()));
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[tokio::test]
async fn deactivated_sso_user_cannot_sign_in() {
    let app = app().await;
    let (_idp, issuer) = mock_idp(serde_json::json!({
        "sub": "pairwise-sub-7", "oid": "entra-oid-7", "aud": [CLIENT_ID], "email": "left@org.example", "name": "Left"
    }))
    .await;
    let base = serve(app.core.clone(), &issuer).await;
    let first = browser()
        .get(format!("{base}/auth/oidc/start"))
        .send()
        .await
        .unwrap();
    let st = first.status();
    let body = first.text().await.unwrap();
    let ctx: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT context FROM audit_log ORDER BY seq")
            .fetch_all(&app.core.db)
            .await
            .unwrap();
    assert_eq!(st, 200, "{ctx:?} {}", &body[..body.len().min(400)]);
    let uid: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    directory::set_user_status(
        &app.core.db,
        &system(),
        uid,
        pdms_core::model::UserStatus::Departed,
    )
    .await
    .unwrap();
    let r = browser()
        .get(format!("{base}/auth/oidc/start"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.text().await.unwrap().contains("Single sign-on failed"));
}
