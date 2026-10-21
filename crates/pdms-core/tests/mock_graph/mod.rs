// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![allow(dead_code)]
//! A small in-process Microsoft Graph for tests: the token endpoint, users
//! delta, single users, and a few generic endpoints for the client tests.
//! Bearer tokens look like `tok-<n>`; pre-authenticated URLs (`/blob/…`,
//! `/upload/…`) reject any Authorization header, as the real ones may.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use pdms_core::config::{GraphConfig, Secret};
use pdms_core::graph::cert;
use serde_json::{Value, json};

pub const TENANT: &str = "tenant-1";
pub const CLIENT: &str = "client-1";

/// One certificate per test binary (key generation is the slow part).
pub fn certificate() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| cert::generate("PADM test", 30).unwrap().pem)
}

#[derive(Default)]
pub struct MockState {
    pub tokens_issued: usize,
    /// Tokens numbered below this are rejected with 401 (simulates expiry).
    pub min_token: usize,
    pub token_forms: Vec<HashMap<String, String>>,
    pub requests: Vec<String>,
    /// The requests (as in `requests`) that carried an Authorization header.
    pub authorized: Vec<String>,
    pub users: BTreeMap<String, Value>,
    /// Changes since the last delta round.
    pub pending: Vec<Value>,
    round: Vec<Value>,
    pub delta_token: u64,
    pub page_size: usize,
    /// Origin for `@odata.nextLink`/`@odata.deltaLink` (default: this mock).
    pub link_base: Option<String>,
    /// Origin `/test/download` redirects to (default: this mock).
    pub download_base: Option<String>,
    /// Scripted failures for the next requests other than token requests:
    /// (status, Retry-After seconds).
    pub fail_next: VecDeque<(u16, u64)>,
    /// The next request with a `$deltatoken` gets HTTP 410.
    pub expire_delta: bool,
    pub uploads: Vec<(String, String, Vec<u8>)>,
}

#[derive(Clone)]
pub struct MockGraph {
    pub base: String,
    pub state: Arc<Mutex<MockState>>,
}

type Shared = (Arc<Mutex<MockState>>, String);

impl MockGraph {
    pub async fn start() -> MockGraph {
        let state = Arc::new(Mutex::new(MockState {
            page_size: 2,
            ..Default::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(handle)
            .with_state((state.clone(), base.clone()));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        MockGraph { base, state }
    }

    /// `[graph]` settings pointing at this mock.
    pub fn config(&self, certificate_pem: &str) -> GraphConfig {
        GraphConfig {
            tenant_id: TENANT.into(),
            client_id: CLIENT.into(),
            certificate: Secret::new(certificate_pem),
            authority_url: self.base.clone(),
            api_url: format!("{}/v1.0", self.base),
            timeout_seconds: 10,
            max_retries: 3,
        }
    }

    pub fn add_user(&self, user: Value) {
        let mut s = self.state.lock().unwrap();
        let id = user["id"].as_str().unwrap().to_owned();
        s.users.insert(id, user.clone());
        s.pending.push(user);
    }

    /// Change some properties; the delta returns the whole user (default mode).
    pub fn update_user(&self, id: &str, patch: Value) {
        let mut s = self.state.lock().unwrap();
        let user = s.users.get_mut(id).unwrap();
        for (k, v) in patch.as_object().unwrap() {
            user[k] = v.clone();
        }
        let merged = user.clone();
        s.pending.push(merged);
    }

    pub fn remove_user(&self, id: &str, reason: &str) {
        let mut s = self.state.lock().unwrap();
        s.users.remove(id);
        s.pending
            .push(json!({ "id": id, "@removed": { "reason": reason } }));
    }

    pub fn fail_next(&self, status: u16, retry_after_secs: u64) {
        self.state
            .lock()
            .unwrap()
            .fail_next
            .push_back((status, retry_after_secs));
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn authorized(&self) -> Vec<String> {
        self.state.lock().unwrap().authorized.clone()
    }

    pub fn tokens_issued(&self) -> usize {
        self.state.lock().unwrap().tokens_issued
    }
}

/// A member account as `/users` returns it.
pub fn member(id: &str, name: &str, mail: &str, department: Option<&str>) -> Value {
    let mut v = json!({
        "id": id, "displayName": name, "mail": mail, "userPrincipalName": mail,
        "accountEnabled": true, "userType": "Member",
    });
    if let Some(d) = department {
        v["department"] = json!(d);
    }
    v
}

fn json_resp(status: u16, v: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [(header::CONTENT_TYPE, "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn graph_error(status: u16, code: &str) -> Response {
    json_resp(
        status,
        json!({ "error": { "code": code, "message": code } }),
    )
}

fn redirect(location: &str) -> Response {
    let mut r = StatusCode::FOUND.into_response();
    r.headers_mut()
        .insert(header::LOCATION, location.parse().unwrap());
    r
}

async fn handle(
    State((st, base)): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or("").to_owned();
    let mut s = st.lock().unwrap();
    let line = format!("{method} {uri}");
    let has_auth = headers.contains_key(header::AUTHORIZATION);
    if has_auth {
        s.authorized.push(line.clone());
    }
    s.requests.push(line);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer tok-"))
        .and_then(|n| n.parse::<usize>().ok());

    if method == Method::POST && path == format!("/{TENANT}/oauth2/v2.0/token") {
        let form: HashMap<String, String> = serde_urlencoded::from_bytes(&body).unwrap_or_default();
        let ok = form.get("grant_type").map(String::as_str) == Some("client_credentials")
            && form.get("client_assertion_type").map(String::as_str)
                == Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
            && form
                .get("client_assertion")
                .is_some_and(|a| a.split('.').count() == 3)
            && form.get("client_id").map(String::as_str) == Some(CLIENT);
        s.token_forms.push(form);
        if !ok {
            return json_resp(
                400,
                json!({"error": "invalid_client", "error_description": "AADSTS700027: bad assertion"}),
            );
        }
        s.tokens_issued += 1;
        let n = s.tokens_issued;
        return json_resp(
            200,
            json!({"token_type": "Bearer", "expires_in": 3600, "access_token": format!("tok-{n}")}),
        );
    }
    if let Some((status, after)) = s.fail_next.pop_front() {
        let mut r = graph_error(status, "TooManyRequests");
        r.headers_mut()
            .insert(header::RETRY_AFTER, after.to_string().parse().unwrap());
        return r;
    }
    if let Some(name) = path.strip_prefix("/blob/") {
        if has_auth {
            return graph_error(400, "authorizationNotAllowed");
        }
        return (StatusCode::OK, format!("contents of {name}")).into_response();
    }
    if let Some(name) = path.strip_prefix("/upload/") {
        if has_auth {
            return graph_error(400, "authorizationNotAllowed");
        }
        let range = headers
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let (end, total) = parse_range(&range);
        s.uploads.push((name.to_owned(), range, body.to_vec()));
        return if end + 1 == total {
            json_resp(201, json!({ "id": format!("item-{name}"), "size": total }))
        } else {
            json_resp(
                202,
                json!({ "nextExpectedRanges": [format!("{}-", end + 1)] }),
            )
        };
    }
    let Some(api) = path.strip_prefix("/v1.0") else {
        return graph_error(404, "notFound");
    };
    if !bearer.is_some_and(|n| n >= s.min_token) {
        return graph_error(401, "InvalidAuthenticationToken");
    }
    match (method, api) {
        (Method::GET, "/users/delta") => users_delta(&mut s, &base, &query),
        (Method::GET, p) if p.starts_with("/users/") => match s.users.get(&p["/users/".len()..]) {
            Some(u) => json_resp(200, u.clone()),
            None => graph_error(404, "Request_ResourceNotFound"),
        },
        (Method::GET, "/test/json") => json_resp(200, json!({ "ok": true })),
        (Method::GET, "/test/download") => {
            let to = s.download_base.clone().unwrap_or(base);
            redirect(&format!("{to}/blob/report.pdf"))
        }
        (Method::GET, "/test/elsewhere") => redirect("http://203.0.113.9/blob/x"),
        (Method::GET, "/test/gone") => graph_error(410, "resyncRequired"),
        (Method::GET, "/test/conflict") => graph_error(409, "nameAlreadyExists"),
        (Method::POST | Method::PATCH, "/test/echo") => {
            json_resp(200, serde_json::from_slice(&body).unwrap_or(Value::Null))
        }
        (Method::DELETE, "/test/item") => StatusCode::NO_CONTENT.into_response(),
        _ => graph_error(404, "itemNotFound"),
    }
}

fn parse_range(range: &str) -> (u64, u64) {
    let r = range.trim_start_matches("bytes ");
    let (span, total) = r.split_once('/').unwrap_or(("0-0", "0"));
    let end = span
        .split_once('-')
        .map_or(0, |(_, e)| e.parse().unwrap_or(0));
    (end, total.parse().unwrap_or(0))
}

fn users_delta(s: &mut MockState, base: &str, query: &str) -> Response {
    let params: HashMap<String, String> = serde_urlencoded::from_str(query).unwrap_or_default();
    if params.contains_key("$deltatoken") {
        if s.expire_delta {
            s.expire_delta = false;
            return graph_error(410, "resyncRequired");
        }
        s.round = std::mem::take(&mut s.pending);
        return delta_page(s, base, 0);
    }
    if let Some(skip) = params.get("$skiptoken") {
        let offset = skip.parse().unwrap_or(0);
        return delta_page(s, base, offset);
    }
    // A new full round: the current state of every user.
    s.pending.clear();
    s.round = s.users.values().cloned().collect();
    delta_page(s, base, 0)
}

fn delta_page(s: &mut MockState, base: &str, offset: usize) -> Response {
    let base = s.link_base.clone().unwrap_or_else(|| base.to_owned());
    let end = (offset + s.page_size.max(1)).min(s.round.len());
    let value: Vec<Value> = s.round[offset.min(end)..end].to_vec();
    let mut page = json!({ "value": value });
    if end < s.round.len() {
        page["@odata.nextLink"] = json!(format!("{base}/v1.0/users/delta?$skiptoken={end}"));
    } else {
        s.delta_token += 1;
        page["@odata.deltaLink"] = json!(format!(
            "{base}/v1.0/users/delta?$deltatoken={}",
            s.delta_token
        ));
    }
    json_resp(200, page)
}
