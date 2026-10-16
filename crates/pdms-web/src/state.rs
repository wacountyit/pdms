// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::{Arc, PoisonError, RwLock, RwLockWriteGuard};
use std::time::{Duration, Instant};

use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use metrics_exporter_prometheus::PrometheusHandle;
use pdms_core::Core;
use pdms_core::auth::oidc::OidcProvider;
use pdms_core::branding::{Branding, ThemeColors};
use sha2::{Digest, Sha256};

/// How long the web process serves cached branding before reading it again
/// (spec §6.3). A save in Administration -> Branding reloads at once.
pub const BRANDING_REFRESH: Duration = Duration::from_secs(60);

/// The branding the pages show: county name, accent shades and seal version.
#[derive(Clone, Debug)]
pub struct BrandingSnapshot {
    pub org_name: String,
    pub theme: ThemeColors,
    /// Changes whenever the name, accent or seal changes; `/theme.css?v=`.
    pub version: String,
    /// The seal's sha256 prefix (`/branding/seal?v=`), or `None` without a seal.
    pub seal_version: Option<String>,
    /// When this was read from the database; `None` until the first load,
    /// so the first request loads it.
    pub loaded_at: Option<Instant>,
}

impl BrandingSnapshot {
    /// The snapshot for `b`; the configured name applies when no name is set.
    pub fn from(config_name: &str, b: &Branding) -> Self {
        let org_name = b
            .org_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(config_name)
            .to_owned();
        let theme = b.theme();
        let seal_sha = b.seal.as_ref().map_or("", |s| s.sha256.as_str());
        let digest = Sha256::digest(format!("{org_name}\n{}\n{seal_sha}", theme.accent));
        BrandingSnapshot {
            version: hex::encode(&digest[..6]),
            seal_version: b.seal.as_ref().map(|s| s.sha256.chars().take(12).collect()),
            org_name,
            theme,
            loaded_at: Some(Instant::now()),
        }
    }

    /// Whether the middleware should read branding again.
    pub fn is_stale(&self) -> bool {
        self.loaded_at
            .is_none_or(|t| t.elapsed() >= BRANDING_REFRESH)
    }
}

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
    /// Cached branding; see [`AppState::branding`].
    branding: Arc<RwLock<BrandingSnapshot>>,
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
            // Config only: no database access here. The server's startup or
            // else the first request loads the stored branding.
            branding: Arc::new(RwLock::new(BrandingSnapshot {
                loaded_at: None,
                ..BrandingSnapshot::from(&cfg.org.name, &Branding::default())
            })),
            core,
        }
    }

    /// The cached branding.
    pub fn branding(&self) -> BrandingSnapshot {
        self.branding
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn branding_mut(&self) -> RwLockWriteGuard<'_, BrandingSnapshot> {
        self.branding
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads branding from the database into the cache. On failure the
    /// cached values stay, and the next attempt waits a full refresh
    /// interval, so an outage does not add a query and a warning to every
    /// request.
    pub async fn reload_branding(&self) {
        match pdms_core::branding::load(&self.core.db).await {
            Ok(b) => {
                let snapshot = BrandingSnapshot::from(&self.core.config.org.name, &b);
                crate::error::set_org_name(&snapshot.org_name);
                *self.branding_mut() = snapshot;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not load branding; keeping the cached values");
                self.branding_mut().loaded_at = Some(Instant::now());
            }
        }
    }

    /// The organization name to show: the branding setting if set, else the
    /// configured name.
    pub fn org_name(&self) -> String {
        self.branding
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .org_name
            .clone()
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
