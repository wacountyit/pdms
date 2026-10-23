// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! The Microsoft Graph client against an in-process mock Graph.

mod common;
mod mock_graph;

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use mock_graph::*;
use pdms_core::Core;
use pdms_core::config::Secret;
use pdms_core::graph::{GraphClient, GraphError};
use serde_json::{Value, json};

async fn client() -> (MockGraph, GraphClient) {
    let m = MockGraph::start().await;
    let g = GraphClient::new(&m.config(certificate())).unwrap();
    (m, g)
}

fn count(m: &MockGraph, needle: &str) -> usize {
    m.requests().iter().filter(|r| r.contains(needle)).count()
}

#[tokio::test]
async fn token_comes_from_a_certificate_assertion_and_is_cached() {
    let (m, g) = client().await;
    let v: Value = g.get("/test/json").await.unwrap();
    assert_eq!(v["ok"], true);
    let _: Value = g.get("/test/json").await.unwrap();
    assert_eq!(m.tokens_issued(), 1, "the token is cached");
    let form = m.state.lock().unwrap().token_forms[0].clone();
    assert_eq!(form["scope"], format!("{}/.default", m.base));
    assert!(!form.contains_key("client_secret"));
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(form["client_assertion"].split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        claims["aud"],
        format!("{}/{TENANT}/oauth2/v2.0/token", m.base)
    );
    assert_eq!(claims["iss"], CLIENT);
}

#[tokio::test]
async fn a_rejected_token_request_reports_the_entra_reason() {
    let m = MockGraph::start().await;
    let mut cfg = m.config(certificate());
    cfg.client_id = "not-registered".into();
    let g = GraphClient::new(&cfg).unwrap();
    let e = g.get::<Value>("/test/json").await.unwrap_err();
    let GraphError::Auth(msg) = &e else {
        panic!("{e}")
    };
    assert!(msg.contains("invalid_client"), "{msg}");
    assert!(msg.contains("AADSTS700027: bad assertion"), "{msg}");
    let assertion = m.state.lock().unwrap().token_forms[0]["client_assertion"].clone();
    assert!(!format!("{e} {e:?}").contains(&assertion));
    assert_eq!(count(&m, "/test/json"), 0, "no Graph call without a token");
}

#[tokio::test]
async fn an_expired_token_is_renewed_once() {
    let (m, g) = client().await;
    let _: Value = g.get("/test/json").await.unwrap();
    m.state.lock().unwrap().min_token = 2;
    let _: Value = g.get("/test/json").await.unwrap();
    assert_eq!(m.tokens_issued(), 2);

    m.state.lock().unwrap().min_token = usize::MAX;
    let e = g.get::<Value>("/test/json").await.unwrap_err();
    assert!(matches!(e, GraphError::Auth(_)), "{e}");
    assert_eq!(m.tokens_issued(), 3, "one renewal, then give up");
}

#[tokio::test]
async fn throttling_is_retried_then_gives_up() {
    let (m, g) = client().await;
    m.fail_next(429, 0);
    m.fail_next(503, 0);
    let started = Instant::now();
    let v: Value = g.get("/test/json").await.unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(count(&m, "/test/json"), 3);

    for _ in 0..4 {
        m.fail_next(429, 0);
    }
    let e = g.get::<Value>("/test/json").await.unwrap_err();
    assert!(
        matches!(e, GraphError::Throttled { retry_after } if retry_after == Duration::ZERO),
        "{e:?}"
    );
    assert_eq!(
        count(&m, "/test/json"),
        3 + 4,
        "one try plus max_retries (3)"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Retry-After: 0 is honoured, not replaced by backoff"
    );
}

#[tokio::test]
async fn errors_map_to_kinds() {
    let (_m, g) = client().await;
    assert!(matches!(
        g.get::<Value>("/nope").await,
        Err(GraphError::NotFound)
    ));
    assert!(matches!(
        g.get::<Value>("/test/gone").await,
        Err(GraphError::ResyncRequired)
    ));
    assert!(matches!(
        g.get::<Value>("/test/conflict").await,
        Err(GraphError::Conflict)
    ));
    assert!(matches!(
        g.get::<Value>("/test/download").await,
        Err(GraphError::Transport(ref s)) if s.contains("redirect")
    ));
}

#[tokio::test]
async fn delta_follows_next_links_to_the_delta_link() {
    let (m, g) = client().await;
    for i in 0..5 {
        m.add_user(member(
            &format!("00000000-0000-0000-0000-00000000000{i}"),
            "U",
            "u@x.org",
            None,
        ));
    }
    let page = g
        .get_delta::<Value>("/users/delta?$select=id")
        .await
        .unwrap();
    assert_eq!(page.items.len(), 5);
    assert!(
        page.delta_link.ends_with("$deltatoken=1"),
        "{}",
        page.delta_link
    );
    assert_eq!(count(&m, "$skiptoken="), 2, "pages of 2");
}

#[tokio::test]
async fn links_to_other_hosts_are_refused() {
    let (m, g) = client().await;
    // Another port is another origin.
    let other = MockGraph::start().await;

    let e = g
        .get::<Value>(&format!("{}/v1.0/test/json", other.base))
        .await
        .unwrap_err();
    assert!(
        matches!(e, GraphError::Transport(ref s) if s.contains("refusing")),
        "{e}"
    );

    m.state.lock().unwrap().link_base = Some(other.base.clone());
    for i in 0..3 {
        m.add_user(member(
            &format!("00000000-0000-0000-0000-00000000000{i}"),
            "U",
            "u@x.org",
            None,
        ));
    }
    let e = g.get_delta::<Value>("/users/delta").await.unwrap_err();
    assert!(
        matches!(e, GraphError::Transport(ref s) if s.contains("refusing")),
        "{e}"
    );
    assert_eq!(
        count(&m, "/users/delta"),
        1,
        "the first page came from Graph"
    );
    assert!(other.requests().is_empty(), "{:?}", other.requests());

    // A pre-authenticated link must be https (plain http only on loopback).
    let e = g.download("/test/elsewhere").await.err().unwrap();
    assert!(
        matches!(e, GraphError::Transport(ref s) if s.contains("refusing")),
        "{e}"
    );
}

#[tokio::test]
async fn download_follows_the_redirect_without_the_token() {
    let (m, g) = client().await;
    // Graph redirects to a pre-authenticated URL on another host.
    let files = MockGraph::start().await;
    m.state.lock().unwrap().download_base = Some(files.base.clone());
    let mut s = g.download("/test/download").await.unwrap();
    let mut bytes = Vec::new();
    while let Some(c) = s.next().await {
        bytes.extend_from_slice(&c.unwrap());
    }
    assert_eq!(bytes, b"contents of report.pdf");
    assert_eq!(files.requests(), vec!["GET /blob/report.pdf"]);
    assert!(files.authorized().is_empty(), "{:?}", files.authorized());
    assert_eq!(m.authorized(), vec!["GET /v1.0/test/download"]);
}

#[tokio::test]
async fn upload_chunks_are_sent_without_the_token() {
    let (m, g) = client().await;
    let url = format!("{}/upload/a", m.base);
    let first = g
        .put_bytes(&url, bytes::Bytes::from_static(b"hello"), (0, 4, 10))
        .await
        .unwrap();
    assert!(first.is_none(), "202: more to send");
    let last = g
        .put_bytes(&url, bytes::Bytes::from_static(b"world"), (5, 9, 10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last["id"], "item-a");
    let ranges: Vec<String> = m
        .state
        .lock()
        .unwrap()
        .uploads
        .iter()
        .map(|u| u.1.clone())
        .collect();
    assert_eq!(ranges, vec!["bytes 0-4/10", "bytes 5-9/10"]);
    assert_eq!(count(&m, "/upload/a"), 2);
    assert!(m.authorized().is_empty(), "{:?}", m.authorized());
}

#[tokio::test]
async fn failed_preauthenticated_requests_do_not_echo_the_url() {
    let (_m, g) = client().await;
    // A host that hangs up on every connection (kept open, so no other test
    // can take its port).
    let hang_up = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = hang_up.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((conn, _)) = hang_up.accept().await {
            drop(conn);
        }
    });
    let url = format!("http://{addr}/upload/a?tempauth=secret");
    let e = g
        .put_bytes(&url, bytes::Bytes::from_static(b"x"), (0, 0, 1))
        .await
        .unwrap_err();
    assert!(matches!(e, GraphError::Transport(_)), "{e}");
    assert!(!format!("{e} {e:?}").contains("tempauth"), "{e:?}");
}

#[tokio::test]
async fn post_patch_and_delete_send_json() {
    let (_m, g) = client().await;
    let v: Value = g.post("/test/echo", &json!({ "a": 1 })).await.unwrap();
    assert_eq!(v["a"], 1);
    let v: Value = g.patch("/test/echo", &json!({ "b": 2 })).await.unwrap();
    assert_eq!(v["b"], 2);
    g.delete("/test/item").await.unwrap();
}

#[tokio::test]
async fn core_has_a_graph_client_only_when_configured() {
    let (core, _db) = common::core().await;
    assert!(core.graph.is_none());
    let m = MockGraph::start().await;
    let mut cfg = (*core.config).clone();
    cfg.graph = m.config(certificate());
    let configured = Core::new(core.db.clone(), cfg.clone()).unwrap();
    assert!(configured.graph.is_some());
    cfg.graph.certificate =
        Secret::new("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n");
    let err = Core::new(core.db.clone(), cfg).unwrap_err().to_string();
    assert!(err.contains("PRIVATE KEY"), "fails at startup: {err}");
}

#[test]
fn certificate_lifetime_outside_1_to_3650_days_is_an_error() {
    for days in [0, 3651, u32::MAX] {
        assert!(
            pdms_core::graph::cert::generate("PDMS test", days).is_err(),
            "{days} days"
        );
    }
}
