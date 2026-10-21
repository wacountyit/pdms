// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft Graph over HTTPS with an app-only token. Throttling is retried;
//! the bearer token goes only to `api_url`'s origin and never follows a
//! redirect; pre-authenticated URLs are fetched without it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use rand::Rng;
use reqwest::{Method, StatusCode, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;
use url::Url;

use super::GraphError;
use super::cert::Credential;
use crate::config::{GraphConfig, is_loopback};

/// Longest single wait honoured from `Retry-After`.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// Renew the token this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);
/// Loop guard when following `@odata.nextLink`.
const MAX_PAGES: usize = 10_000;
/// Longest piece of an Entra error message kept in a `GraphError`.
const MAX_REASON: usize = 300;

/// A Graph client acting as the PADM app registration. Cheap to clone.
#[derive(Clone)]
pub struct GraphClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    credential: Credential,
    client_id: String,
    token_url: String,
    scope: String,
    api: Url,
    max_retries: u32,
    token: Mutex<Option<(String, Instant)>>,
}

/// Everything a delta round returned, plus the link for the next round.
#[derive(Debug)]
pub struct DeltaPage<T> {
    pub items: Vec<T>,
    pub delta_link: String,
}

#[derive(serde::Deserialize)]
#[serde(bound(deserialize = "T: DeserializeOwned"))]
struct RawPage<T> {
    #[serde(default)]
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

impl std::fmt::Debug for GraphClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphClient")
            .field("api", &self.inner.api.as_str())
            .field("client_id", &self.inner.client_id)
            .finish_non_exhaustive()
    }
}

impl GraphClient {
    pub fn new(cfg: &GraphConfig) -> Result<GraphClient, GraphError> {
        let credential = Credential::from_pem(cfg.certificate.expose())?;
        let api = Url::parse(cfg.api_url.trim_end_matches('/'))
            .map_err(|e| GraphError::Transport(format!("graph.api_url: {e}")))?;
        let scope = format!("{}/.default", api.origin().ascii_serialization());
        let token_url = format!(
            "{}/{}/oauth2/v2.0/token",
            cfg.authority_url.trim_end_matches('/'),
            cfg.tenant_id
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_seconds.max(5)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("PADM/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| GraphError::Transport(e.to_string()))?;
        Ok(GraphClient {
            inner: Arc::new(Inner {
                http,
                credential,
                client_id: cfg.client_id.clone(),
                token_url,
                scope,
                api,
                max_retries: cfg.max_retries,
                token: Mutex::new(None),
            }),
        })
    }

    /// The certificate's `x5t#S256` thumbprint (shown on the admin page).
    pub fn thumbprint(&self) -> &str {
        self.inner.credential.thumbprint()
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, GraphError> {
        json_body(self.send(Method::GET, path, None).await?).await
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GraphError> {
        self.send_json(Method::POST, path, body).await
    }

    pub async fn patch<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GraphError> {
        self.send_json(Method::PATCH, path, body).await
    }

    pub async fn delete(&self, path: &str) -> Result<(), GraphError> {
        not_redirected(self.send(Method::DELETE, path, None).await?)?;
        Ok(())
    }

    /// Follow `@odata.nextLink` from `start` until a page carries `@odata.deltaLink`.
    pub async fn get_delta<T: DeserializeOwned>(
        &self,
        start: &str,
    ) -> Result<DeltaPage<T>, GraphError> {
        let mut items = Vec::new();
        let mut next = start.to_owned();
        for _ in 0..MAX_PAGES {
            let page: RawPage<T> = self.get(&next).await?;
            items.extend(page.value);
            match (page.next_link, page.delta_link) {
                (Some(n), _) => next = n,
                (None, Some(d)) => {
                    return Ok(DeltaPage {
                        items,
                        delta_link: d,
                    });
                }
                (None, None) => {
                    return Err(GraphError::Http {
                        status: 200,
                        code: "noDeltaLink".into(),
                        message: "a delta page had neither nextLink nor deltaLink".into(),
                    });
                }
            }
        }
        Err(GraphError::Http {
            status: 200,
            code: "tooManyPages".into(),
            message: format!("more than {MAX_PAGES} delta pages"),
        })
    }

    /// One chunk of an upload session. `range` is `(first byte, last byte, total)`.
    /// `Ok(None)` means "send the next chunk"; `Ok(Some(item))` is the finished file.
    pub async fn put_bytes(
        &self,
        absolute_url: &str,
        bytes: Bytes,
        range: (u64, u64, u64),
    ) -> Result<Option<Value>, GraphError> {
        let url = self.preauthenticated(absolute_url)?;
        let (start, end, total) = range;
        let mut attempt = 0;
        loop {
            // Upload URLs are pre-authenticated: no Authorization header.
            let resp = self
                .inner
                .http
                .put(url.clone())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .body(bytes.clone())
                .send()
                .await
                .map_err(transport)?;
            let status = resp.status();
            if status == StatusCode::ACCEPTED {
                return Ok(None);
            }
            if status.is_success() {
                return Ok(Some(json_body(resp).await?));
            }
            if is_throttle(status) {
                let wait = retry_wait(retry_after(&resp), attempt, self.inner.max_retries)?;
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }
            let text = resp.text().await.unwrap_or_default();
            return Err(GraphError::from_response(status, &text));
        }
    }

    /// Stream a file. Graph answers `/content` with a 302 to a pre-authenticated
    /// URL, which is fetched without the token.
    pub async fn download(&self, path: &str) -> Result<pdms_storage::ByteStream, GraphError> {
        let first = self.send(Method::GET, path, None).await?;
        let resp = if first.status().is_redirection() {
            let location = first
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| GraphError::Transport("redirect without Location".into()))?
                .to_owned();
            let url = self.preauthenticated(&location)?;
            let r = self.inner.http.get(url).send().await.map_err(transport)?;
            if !r.status().is_success() {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                return Err(GraphError::from_response(status, &text));
            }
            r
        } else {
            first
        };
        Ok(Box::pin(resp.bytes_stream().map(|r| {
            r.map_err(|e| std::io::Error::other(e.without_url()))
        })))
    }

    async fn send_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T, GraphError> {
        let v = serde_json::to_value(body).map_err(|e| GraphError::Transport(e.to_string()))?;
        json_body(self.send(method, path, Some(&v)).await?).await
    }

    async fn token(&self) -> Result<String, GraphError> {
        let mut cached = self.inner.token.lock().await;
        if let Some((t, until)) = cached.as_ref()
            && *until > Instant::now()
        {
            return Ok(t.clone());
        }
        let assertion = self.inner.credential.client_assertion(
            &self.inner.client_id,
            &self.inner.token_url,
            chrono::Utc::now().timestamp(),
        )?;
        let resp = self
            .inner
            .http
            .post(&self.inner.token_url)
            .form(&[
                ("client_id", self.inner.client_id.as_str()),
                ("scope", self.inner.scope.as_str()),
                ("grant_type", "client_credentials"),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| GraphError::Transport(format!("token request: {e}")))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            // Entra answers `{"error": "invalid_client", "error_description":
            // "AADSTS700027: …"}`; the AADSTS code names the certificate or
            // consent problem.
            let field = |k: &str| {
                body.get(k)
                    .and_then(Value::as_str)
                    .and_then(|s| s.lines().next())
                    .map(|s| s.chars().take(MAX_REASON).collect::<String>())
            };
            let why = match (field("error"), field("error_description")) {
                (Some(e), Some(d)) => format!("{e}: {d}"),
                (Some(one), None) | (None, Some(one)) => one,
                (None, None) => "no details".into(),
            };
            return Err(GraphError::Auth(format!(
                "token request: HTTP {} {why}",
                status.as_u16()
            )));
        }
        let token = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| GraphError::Auth("token response without access_token".into()))?
            .to_owned();
        let lifetime = Duration::from_secs(
            body.get("expires_in")
                .and_then(Value::as_u64)
                .unwrap_or(3600),
        );
        *cached = Some((
            token.clone(),
            Instant::now() + lifetime.saturating_sub(TOKEN_MARGIN),
        ));
        Ok(token)
    }

    /// Drop the cached token if it is still `rejected` (another task may
    /// already have renewed it).
    async fn forget_token(&self, rejected: &str) {
        let mut cached = self.inner.token.lock().await;
        if cached.as_ref().is_some_and(|(t, _)| t == rejected) {
            *cached = None;
        }
    }

    /// A path relative to `api_url`, or an absolute link Graph gave us; either
    /// way it must be on `api_url`'s origin, because it will carry the token.
    fn url(&self, path: &str) -> Result<Url, GraphError> {
        let url = if path.starts_with("https://") || path.starts_with("http://") {
            Url::parse(path).map_err(|e| GraphError::Transport(format!("bad link: {e}")))?
        } else {
            let base = self.inner.api.as_str().trim_end_matches('/');
            Url::parse(&format!("{base}/{}", path.trim_start_matches('/')))
                .map_err(|e| GraphError::Transport(format!("bad path {path:?}: {e}")))?
        };
        if url.origin() != self.inner.api.origin() {
            return Err(GraphError::Transport(format!(
                "refusing to send credentials to {}",
                url.origin().ascii_serialization()
            )));
        }
        Ok(url)
    }

    /// A pre-authenticated URL (download or upload): https, or loopback in tests.
    fn preauthenticated(&self, s: &str) -> Result<Url, GraphError> {
        let url = Url::parse(s).map_err(|e| GraphError::Transport(format!("bad URL: {e}")))?;
        let ok = url.scheme() == "https"
            || (url.scheme() == "http" && is_loopback(&url) && is_loopback(&self.inner.api));
        if !ok {
            return Err(GraphError::Transport(format!(
                "refusing to fetch {}",
                url.origin().ascii_serialization()
            )));
        }
        Ok(url)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response, GraphError> {
        let url = self.url(path)?;
        let mut attempt: u32 = 0;
        let mut renewed = false;
        loop {
            let token = self.token().await?;
            let mut req = self
                .inner
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| GraphError::Transport(e.to_string()))?;
            let status = resp.status();
            if status.is_success() || status.is_redirection() {
                return Ok(resp);
            }
            if status == StatusCode::UNAUTHORIZED && !renewed {
                renewed = true;
                self.forget_token(&token).await;
                continue;
            }
            if is_throttle(status) {
                let wait = retry_wait(retry_after(&resp), attempt, self.inner.max_retries)?;
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }
            let text = resp.text().await.unwrap_or_default();
            return Err(GraphError::from_response(status, &text));
        }
    }
}

async fn json_body<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, GraphError> {
    let bytes = not_redirected(resp)?.bytes().await.map_err(transport)?;
    if bytes.is_empty() {
        serde_json::from_value(Value::Null)
    } else {
        serde_json::from_slice(&bytes)
    }
    .map_err(|e| GraphError::Transport(format!("unexpected response: {e}")))
}

/// Only `download` follows a redirect; anywhere else one is an error.
fn not_redirected(resp: reqwest::Response) -> Result<reqwest::Response, GraphError> {
    if resp.status().is_redirection() {
        return Err(GraphError::Transport(format!(
            "unexpected redirect (HTTP {})",
            resp.status().as_u16()
        )));
    }
    Ok(resp)
}

/// A transport failure without its URL: a pre-authenticated URL is a credential.
fn transport(e: reqwest::Error) -> GraphError {
    GraphError::Transport(e.without_url().to_string())
}

fn is_throttle(s: StatusCode) -> bool {
    matches!(s.as_u16(), 429 | 503 | 504)
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let v = resp.headers().get(header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// After throttled try number `attempt` (from 0): how long to wait before
/// trying again (`Retry-After`, else backoff; at most 60 s), or `Throttled`
/// once `max_retries` retries are spent.
fn retry_wait(
    retry_after: Option<Duration>,
    attempt: u32,
    max_retries: u32,
) -> Result<Duration, GraphError> {
    let wait = retry_after.unwrap_or_else(|| backoff(attempt));
    if attempt < max_retries {
        Ok(wait.min(MAX_WAIT))
    } else {
        Err(GraphError::Throttled { retry_after: wait })
    }
}

/// 1, 2, 4, 8, 16, 32 seconds, each plus up to half again of random jitter.
fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(1u64 << attempt.min(5));
    base + base.mul_f64(rand::thread_rng().gen_range(0.0..0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_waits_honour_retry_after_back_off_with_jitter_and_cap() {
        assert_eq!(
            retry_wait(Some(Duration::from_secs(7)), 0, 4).unwrap(),
            Duration::from_secs(7)
        );
        assert_eq!(
            retry_wait(Some(Duration::from_secs(600)), 0, 4).unwrap(),
            MAX_WAIT
        );
        for attempt in 0..8 {
            let base = Duration::from_secs(1 << attempt.min(5));
            let w = retry_wait(None, attempt, 10).unwrap();
            assert!(w >= base && w < base.mul_f64(1.5) && w <= MAX_WAIT, "{w:?}");
        }
        let waits: std::collections::HashSet<Duration> = (0..20).map(|_| backoff(3)).collect();
        assert!(waits.len() > 1, "backoff is jittered");
        assert!(matches!(
            retry_wait(Some(Duration::from_secs(600)), 4, 4),
            Err(GraphError::Throttled { retry_after }) if retry_after == Duration::from_secs(600)
        ));
    }
}
