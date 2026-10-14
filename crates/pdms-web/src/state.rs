// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;

use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use metrics_exporter_prometheus::PrometheusHandle;
use pdms_core::Core;
use pdms_core::auth::oidc::OidcProvider;

/// Cookie names. With secure cookies the `__Host-` prefix pins them to this
/// origin over HTTPS with `Path=/` and no `Domain`.
#[derive(Clone, Copy, Debug)]
pub struct CookieNames {
    pub session: &'static str,
    /// Encrypted, short-lived: login CSRF token and OIDC flow state.
    pub login: &'static str,
    /// Encrypted one-shot message shown after a redirect.
    pub flash: &'static str,
}

impl CookieNames {
    pub fn new(secure: bool) -> Self {
        if secure {
            CookieNames {
                session: "__Host-pdms_session",
                login: "__Host-pdms_login",
                flash: "__Host-pdms_flash",
            }
        } else {
            CookieNames {
                session: "pdms_session",
                login: "pdms_login",
                flash: "pdms_flash",
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub core: Core,
    pub cookie_key: Key,
    pub cookies: CookieNames,
    pub oidc: Option<Arc<OidcProvider>>,
    pub login_limiter: Arc<DefaultKeyedRateLimiter<IpAddr>>,
    /// Per API token.
    pub api_limiter: Arc<DefaultKeyedRateLimiter<uuid::Uuid>>,
    pub metrics: Option<PrometheusHandle>,
    pub tz: chrono_tz::Tz,
    /// `scheme://host[:port]` of the public URL, for Origin checks.
    pub origin: String,
}

impl AppState {
    pub fn new(core: Core, oidc: Option<OidcProvider>, metrics: Option<PrometheusHandle>) -> Self {
        let cfg = core.config.clone();
        let per_minute =
            NonZeroU32::new(cfg.auth.login_attempts_per_minute.max(1)).unwrap_or(NonZeroU32::MIN);
        AppState {
            cookie_key: Key::from(core.keys.cookie_key()),
            cookies: CookieNames::new(cfg.server.secure_cookies),
            oidc: oidc.map(Arc::new),
            login_limiter: Arc::new(RateLimiter::keyed(Quota::per_minute(per_minute))),
            api_limiter: Arc::new(RateLimiter::keyed(Quota::per_minute(
                NonZeroU32::new(cfg.api.requests_per_minute.max(1)).unwrap_or(NonZeroU32::MIN),
            ))),
            metrics,
            tz: cfg.timezone(),
            origin: cfg.server.public_url.origin().ascii_serialization(),
            core,
        }
    }

    pub fn org_name(&self) -> &str {
        &self.core.config.org.name
    }

    pub fn secure(&self) -> bool {
        self.core.config.server.secure_cookies
    }
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}
