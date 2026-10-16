// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
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
    /// When this was read from the database, or when a background refresh
    /// was claimed; `None` until then, so the first request claims one.
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

/// The cached snapshot, and the number of the load that produced it.
#[derive(Debug)]
struct BrandingCache {
    snapshot: BrandingSnapshot,
    applied: u64,
}

impl BrandingCache {
    /// Swaps in the result of load number `seq`, unless a later load has
    /// already been applied (a slow background refresh must not undo the
    /// reload that follows a save).
    fn apply(&mut self, seq: u64, snapshot: BrandingSnapshot) -> bool {
        if seq <= self.applied {
            return false;
        }
        self.applied = seq;
        self.snapshot = snapshot;
        true
    }
}

fn read_cache(lock: &RwLock<BrandingCache>) -> RwLockReadGuard<'_, BrandingCache> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_cache(lock: &RwLock<BrandingCache>) -> RwLockWriteGuard<'_, BrandingCache> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Claims the refresh of a stale cache: under the write lock, a stale
/// snapshot is stamped with the current time, so only one caller per
/// refresh interval gets `true`.
fn claim_refresh(lock: &RwLock<BrandingCache>) -> bool {
    // The common case (fresh) only needs the read lock.
    if !read_cache(lock).snapshot.is_stale() {
        return false;
    }
    let mut cache = write_cache(lock);
    if !cache.snapshot.is_stale() {
        return false;
    }
    cache.snapshot.loaded_at = Some(Instant::now());
    true
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
    branding: Arc<RwLock<BrandingCache>>,
    /// Numbers branding loads, so an older load never replaces a newer one.
    branding_loads: Arc<AtomicU64>,
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
            // Config only: no database access here. The server's startup, or
            // else a refresh claimed by the first request, loads the stored
            // branding.
            branding: Arc::new(RwLock::new(BrandingCache {
                snapshot: BrandingSnapshot {
                    loaded_at: None,
                    ..BrandingSnapshot::from(&cfg.org.name, &Branding::default())
                },
                applied: 0,
            })),
            branding_loads: Arc::new(AtomicU64::new(0)),
            core,
        }
    }

    /// The cached branding.
    pub fn branding(&self) -> BrandingSnapshot {
        read_cache(&self.branding).snapshot.clone()
    }

    /// True for exactly one caller once the cache is stale; that caller
    /// should then run [`AppState::reload_branding`]. Until the reload
    /// finishes, everyone keeps the current snapshot.
    pub fn claim_branding_refresh(&self) -> bool {
        claim_refresh(&self.branding)
    }

    /// Reads branding from the database and swaps it into the cache before
    /// returning. On failure the cached values stay, and the next attempt
    /// waits a full refresh interval, so an outage does not add a query and
    /// a warning to every request.
    pub async fn reload_branding(&self) {
        let seq = self.branding_loads.fetch_add(1, Ordering::Relaxed) + 1;
        match pdms_core::branding::load(&self.core.db).await {
            Ok(b) => {
                let snapshot = BrandingSnapshot::from(&self.core.config.org.name, &b);
                let mut cache = write_cache(&self.branding);
                let (name, version) = (snapshot.org_name.clone(), snapshot.version.clone());
                if cache.apply(seq, snapshot) {
                    crate::error::set_branding(&name, &version);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not load branding; keeping the cached values");
                write_cache(&self.branding).snapshot.loaded_at = Some(Instant::now());
            }
        }
    }

    /// The organization name to show: the branding setting if set, else the
    /// configured name.
    pub fn org_name(&self) -> String {
        read_cache(&self.branding).snapshot.org_name.clone()
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

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;

    fn snapshot(name: &str) -> BrandingSnapshot {
        BrandingSnapshot::from(name, &Branding::default())
    }

    fn backdated(mut s: BrandingSnapshot) -> BrandingSnapshot {
        s.loaded_at = Some(
            Instant::now()
                .checked_sub(BRANDING_REFRESH + Duration::from_secs(1))
                .expect("the clock has run for over a minute"),
        );
        s
    }

    #[test]
    fn fresh_snapshot_is_not_stale_and_backdated_or_unloaded_is() {
        assert!(!snapshot("X").is_stale());
        assert!(backdated(snapshot("X")).is_stale());
        let never = BrandingSnapshot {
            loaded_at: None,
            ..snapshot("X")
        };
        assert!(never.is_stale());
    }

    #[test]
    fn simultaneous_stale_checks_claim_the_refresh_once() {
        let lock = Arc::new(RwLock::new(BrandingCache {
            snapshot: backdated(snapshot("X")),
            applied: 0,
        }));
        let n = 8;
        let start = Arc::new(Barrier::new(n));
        let claims: usize = (0..n)
            .map(|_| {
                let (lock, start) = (lock.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    claim_refresh(&lock)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum();
        assert_eq!(claims, 1);
        // The claim stamped the snapshot, so it is fresh until the next interval.
        assert!(!read_cache(&lock).snapshot.is_stale());
        assert!(!claim_refresh(&lock));
    }

    #[test]
    fn an_older_load_never_replaces_a_newer_one() {
        let mut cache = BrandingCache {
            snapshot: snapshot("Config"),
            applied: 0,
        };
        assert!(cache.apply(2, snapshot("After save")));
        assert!(!cache.apply(1, snapshot("Slow background load")));
        assert_eq!(cache.snapshot.org_name, "After save");
        assert!(cache.apply(3, snapshot("Next refresh")));
        assert_eq!(cache.snapshot.org_name, "Next refresh");
    }
}
