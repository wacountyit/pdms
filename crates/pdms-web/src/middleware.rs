// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Request pipeline: client info, security headers, CSRF/Origin checks, flash, metrics.

use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::PrivateCookieJar;
use http_body_util::BodyExt;
use ipnet::IpNet;
use pdms_core::ClientInfo;

use crate::error::AppError;
use crate::state::AppState;
use crate::views::Flash;

pub const CSRF_HEADER: &str = "x-csrf-token";
pub const CSRF_FIELD: &str = "_csrf";

/// Determine the client IP. `X-Forwarded-For` is honoured only when the peer
/// is a trusted proxy, and then the right-most untrusted address wins (entries
/// further left are client-controlled).
pub fn resolve_client_ip(
    peer: Option<IpAddr>,
    xff: Option<&str>,
    trusted: &[IpNet],
) -> Option<IpAddr> {
    let is_trusted = |ip: &IpAddr| trusted.iter().any(|n| n.contains(ip));
    let peer = peer?;
    if !is_trusted(&peer) {
        return Some(peer);
    }
    let Some(xff) = xff else { return Some(peer) };
    let hops: Vec<IpAddr> = xff
        .split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .collect();
    hops.iter()
        .rev()
        .find(|ip| !is_trusted(ip))
        .or(hops.first())
        .copied()
        .or(Some(peer))
}

pub async fn client_info(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let headers = req.headers();
    let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
    let info = ClientInfo {
        ip: resolve_client_ip(peer, xff, &state.core.config.server.trusted_proxies),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(512).collect()),
        request_id: headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(128).collect()),
    };
    req.extensions_mut().insert(info);
    next.run(req).await
}

/// Paths that never show branding: probes, metrics and embedded assets.
fn skips_branding(path: &str) -> bool {
    matches!(path, "/healthz" | "/readyz" | "/metrics") || path.starts_with("/static/")
}

/// Refreshes the branding cache once it is older than [`BRANDING_REFRESH`]
/// (or was never loaded). One request claims the refresh and starts it in
/// the background; no request waits for it, and pages keep the current
/// snapshot until the new one is swapped in.
///
/// [`BRANDING_REFRESH`]: crate::state::BRANDING_REFRESH
pub async fn branding_refresh(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !skips_branding(req.uri().path()) && state.claim_branding_refresh() {
        let state = state.clone();
        tokio::spawn(async move { state.reload_branding().await });
    }
    next.run(req).await
}

pub async fn security_headers(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let is_static = req.uri().path().starts_with("/static/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, name: &'static str, value: &'static str| {
        h.insert(name, HeaderValue::from_static(value));
    };
    set(
        h,
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
         object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
    );
    set(h, "x-content-type-options", "nosniff");
    set(h, "x-frame-options", "DENY");
    set(h, "referrer-policy", "same-origin");
    set(h, "cross-origin-opener-policy", "same-origin");
    set(
        h,
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=()",
    );
    if state.secure() {
        set(
            h,
            "strict-transport-security",
            "max-age=31536000; includeSubDomains",
        );
    }
    if !is_static && !h.contains_key(header::CACHE_CONTROL) {
        set(h, "cache-control", "no-store");
    }
    res
}

/// Paths that carry their own CSRF protection (pre-session login form).
const CSRF_EXEMPT: &[&str] = &["/login/local"];

/// Multipart upload routes. Browsers cannot add a header to a plain file
/// form, so for these the token is checked by the handler as the *first*
/// multipart field, before any file data is read (see `upload::read`).
fn is_upload_route(path: &str) -> bool {
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    matches!(
        parts.as_slice(),
        ["", "documents"]
            | ["", "documents", _, "versions"]
            | ["", "documents", _, "signatories", _, "signed"]
            | ["", "admin", "branding", "seal"]
    )
}

/// Present on multipart upload requests whose CSRF token is still to be verified.
#[derive(Clone)]
pub struct DeferredCsrf(pub String);

/// Reject cross-origin state changes and require the session-bound CSRF token
/// (header `X-CSRF-Token` or form field `_csrf`) on every authenticated unsafe request.
pub async fn csrf_guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let headers = req.headers();
    match headers
        .get(header::ORIGIN)
        .map(|o| o.to_str().unwrap_or(""))
    {
        Some(origin) if origin != state.origin => return AppError::Csrf.into_response(),
        Some(_) => {}
        None => {
            if headers
                .get("sec-fetch-site")
                .is_some_and(|v| v == "cross-site")
            {
                return AppError::Csrf.into_response();
            }
        }
    }
    // The API authenticates with bearer tokens only and ignores cookies, so
    // a forged cross-site request carries no credentials.
    if req.uri().path().starts_with("/api/") {
        return next.run(req).await;
    }
    if CSRF_EXEMPT.contains(&req.uri().path()) {
        return next.run(req).await;
    }
    let jar = axum_extra::extract::CookieJar::from_headers(headers);
    let Some(session_token) = jar.get(state.cookies.session).map(|c| c.value().to_owned()) else {
        // No session: the handler's authentication check rejects the request.
        return next.run(req).await;
    };

    let header_token = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let (req, presented) = match header_token {
        Some(t) => (req, Some(t)),
        None => {
            let is_form = headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
            let is_multipart = headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("multipart/form-data"));
            if is_multipart && is_upload_route(req.uri().path()) {
                let mut req = req;
                req.extensions_mut().insert(DeferredCsrf(session_token));
                return next.run(req).await;
            }
            if !is_form {
                return AppError::Csrf.into_response();
            }
            let limit = state.core.config.server.max_body_bytes;
            let (parts, body) = req.into_parts();
            let Ok(collected) = http_body_util::Limited::new(body, limit).collect().await else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            let bytes = collected.to_bytes();
            let token = serde_urlencoded::from_bytes::<Vec<(String, String)>>(&bytes)
                .ok()
                .and_then(|pairs| {
                    pairs
                        .into_iter()
                        .find(|(k, _)| k == CSRF_FIELD)
                        .map(|(_, v)| v)
                });
            (Request::from_parts(parts, Body::from(bytes)), token)
        }
    };
    match presented {
        Some(t) if state.core.keys.verify_csrf(&session_token, &t) => next.run(req).await,
        _ => AppError::Csrf.into_response(),
    }
}

/// Move a flash message from its cookie into request extensions (GET only) and clear it.
pub async fn flash(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    if req.method() != Method::GET {
        return next.run(req).await;
    }
    let jar = PrivateCookieJar::from_headers(req.headers(), state.cookie_key.clone());
    let Some(cookie) = jar.get(state.cookies.flash) else {
        return next.run(req).await;
    };
    if let Ok(f) = serde_json::from_str::<Flash>(cookie.value()) {
        req.extensions_mut().insert(f);
    }
    let res = next.run(req).await;
    let jar = jar.remove(crate::routes::auth::removal(&state, state.cookies.flash));
    (jar, res).into_response()
}

pub async fn http_metrics(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    metrics::counter!("pdms_http_requests_total", "method" => method.clone(), "route" => route.clone(), "status" => status)
        .increment(1);
    metrics::histogram!("pdms_http_request_duration_seconds", "method" => method, "route" => route)
        .record(start.elapsed().as_secs_f64());
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn probes_and_assets_skip_the_branding_refresh() {
        for p in [
            "/healthz",
            "/readyz",
            "/metrics",
            "/static/app.css",
            "/static/fonts/x.woff2",
        ] {
            assert!(skips_branding(p), "{p}");
        }
        for p in [
            "/",
            "/login",
            "/theme.css",
            "/branding/seal",
            "/documents",
            "/staticx",
        ] {
            assert!(!skips_branding(p), "{p}");
        }
    }

    #[test]
    fn upload_routes() {
        assert!(is_upload_route("/documents"));
        assert!(is_upload_route("/documents/0193/versions"));
        assert!(!is_upload_route("/documents/0193/edit"));
        assert!(is_upload_route("/admin/branding/seal"));
        assert!(!is_upload_route("/admin/users"));
        assert!(!is_upload_route("/documents/a/b/versions"));
        assert!(is_upload_route("/documents/a/signatories/b/signed"));
        assert!(!is_upload_route("/documents/a/signatories/b/request"));
    }

    #[test]
    fn untrusted_peer_ignores_forwarded_header() {
        let trusted: Vec<IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            resolve_client_ip(Some(ip("203.0.113.9")), Some("1.2.3.4"), &trusted),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn trusted_proxy_uses_rightmost_untrusted_hop() {
        let trusted: Vec<IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
        let r = resolve_client_ip(
            Some(ip("10.0.0.2")),
            Some("6.6.6.6, 198.51.100.7, 10.0.0.5"),
            &trusted,
        );
        assert_eq!(
            r,
            Some(ip("198.51.100.7")),
            "spoofed left-most entry ignored"
        );
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), None, &trusted),
            Some(ip("10.0.0.2"))
        );
        assert_eq!(resolve_client_ip(None, Some("1.2.3.4"), &trusted), None);
    }
}
