# P2 — Microsoft Graph client and Entra user sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** PDMS can read Microsoft Entra ID as itself (certificate, app-only), keeps people and their departments in step with Entra every night (and on demand), and matches SSO sign-ins by Entra object ID only.

**Architecture:** A new `pdms-core::graph` module holds a certificate credential (PS256 client assertion), a token cache, and a small JSON/delta/upload/download client that retries throttling and never sends its bearer token anywhere but `api_url`. `directory::entra_values` maps Entra "Department" values to PDMS departments; `directory::entra_sync` reads `/users/delta`, creates/updates/departs people, keeps one `idp` department membership per person, and records a cursor in `sync_cursors`. OIDC sign-in uses the same identity key and, when the sync is on, asks Graph about a first-time user instead of trusting token claims. An admin page shows status, mappings, and unmapped values.

**Tech Stack:** Rust 2024, reqwest 0.12 (rustls, `stream`), ring 0.17 (RSA-PSS signing), rcgen 0.14 + rsa 0.9 (certificate generation), SQLx 0.9, Axum 0.8 (also as an in-process mock Graph in tests), Askama 0.16.

**Spec:** [docs/superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md](../specs/2026-10-14-sharepoint-storage-entra-sync-design.md) §2 D10–D13, §7.5, §9, §10, §11, §12, Appendix A step 7; decision D-086. **Index:** [2026-10-14-pdms-sharepoint-entra-index.md](2026-10-14-pdms-sharepoint-entra-index.md) ("Produced by P2"; P1 owns migration 0008, UI redesign R1 owns 0009, this plan owns 0010). **Depends on:** P1 merged (only for migration numbering).

## Global Constraints

- Graph authentication is app-only with a **certificate client assertion** (learn.microsoft.com "Microsoft identity platform certificate credentials"): JWT header `alg` = `PS256`, `typ` = `JWT`, `x5t#S256` = base64url(SHA-256(certificate DER)); claims `aud` = `{authority_url}/{tenant_id}/oauth2/v2.0/token`, `iss` = `sub` = client ID, `jti` = new GUID, `nbf` = `iat` = now, `exp` = now + 600 s. Token request form: `client_id`, `scope` = `{origin of api_url}/.default`, `grant_type` = `client_credentials`, `client_assertion_type` = `urn:ietf:params:oauth:client-assertion-type:jwt-bearer`, `client_assertion`. **No client secrets.**
- The certificate is a PEM bundle (certificate + **unencrypted** RSA private key, `PRIVATE KEY` (PKCS#8) or `RSA PRIVATE KEY` (PKCS#1)) in `[graph] certificate`, normally supplied as a file with `PDMS_GRAPH__CERTIFICATE_FILE` (the existing `_FILE` loader reads it).
- Config keys, exactly: `[graph] tenant_id, client_id, certificate, authority_url (default https://login.microsoftonline.com), api_url (default https://graph.microsoft.com/v1.0), timeout_seconds (60), max_retries (4)`; `[entra_sync] enabled (false), run_at ("02:30", HH:MM in [org] timezone), attribute ("department"), include_guests (false)`. URLs must be `https`, except `http` to `127.0.0.1`/`localhost`/`[::1]` (tests).
- The bearer token is sent **only** to the origin of `api_url`; redirects are never followed with it; pre-authenticated download and upload URLs get **no** `Authorization` header (Graph docs: "You don't need to include an Authorization header when you access the download URL").
- Retries: HTTP 429/503/504 honour `Retry-After` (seconds), otherwise exponential backoff with jitter (spec §10); at most `max_retries` retries; each wait at most 60 s; then `GraphError::Throttled`. One automatic token renewal on HTTP 401.
- Delta: `$select` only on the initial request; follow `@odata.nextLink`/`@odata.deltaLink` URLs as given; HTTP 410, or error codes `resyncRequired`/`syncStateNotFound`/`syncStateInvalid` → full resync. The same change can be replayed, so applying a user must be idempotent. Directory delta tokens expire after 7 days.
- Graph permission needed by this plan: **`User.Read.All` (application)** only.
- Identity key: `(normalized issuer, Entra object ID)` where the normalized issuer is `trim()` without trailing `/`. **Never link or merge accounts by email** (D-086).
- Guests (`userType` = `Guest`) are excluded unless `include_guests`; a null `userType` counts as a member. Disabled (`accountEnabled = false`) and removed (`@removed`) users become `departed` through `directory::set_user_status` (revokes sessions; keeps the last-administrator guard). The sync **never reactivates** anyone.
- Department value normalization: replace `&` with ` and `, lowercase, collapse whitespace (so "Road & Bridge" = "road and bridge "). One `idp` department membership per person; `manual` memberships are never touched, and a manual membership in the mapped department satisfies the mapping.
- Audit actions used: `directory.user_created`, `user.updated`, `user.idp_memberships_synced`, `user.status_changed` (departures, from `set_user_status`), `directory.synced`, `directory.department_mapping_changed`, `directory.sync_requested`. Each in the same transaction as its change.
- Migration `migrations/0010_entra_sync.sql`; never edit 0001–0009; migrations have no SPDX header (D-080).
- New Rust files start with `// SPDX-License-Identifier: AGPL-3.0-or-later` and `// Copyright (C) 2026 PDMS contributors`. `unwrap`/`expect`/`panic`/`dbg` are denied outside tests; new test files put those two SPDX lines first, then the same `#![allow(...)]` block existing tests use.
- After any `sqlx::query!` or migration change: `sqlx migrate run --source migrations` on the dev DB, then `cargo sqlx prepare --workspace -- --all-targets`, and commit `.sqlx/`. Use the index's "Shared development setup" (exports `DATABASE_URL`, `PDMS_TEST_DATABASE_URL`, …) in every shell.

## Review Focus

Inputs the spec implies that are most likely to hurt a real county user; each has a test in its owning task.

1. **Someone leaves the county** (account disabled or deleted in Entra): they must be `departed` with sessions revoked after the next sync — but the last PDMS administrator is never deactivated automatically; it is reported instead (Task 6 `delta_moves_departments_and_departs_disabled_and_deleted_users`, `last_administrator_is_reported_not_departed`).
2. **An Entra account whose email equals an existing PDMS account** (the break-glass admin, a hand-created user): never linked, never merged; the sync reports it and SSO denies with `email_in_use` (Task 6 `email_already_used_by_an_unlinked_account_is_reported_not_linked`, Task 8 `existing_account_with_the_same_email_is_never_linked`).
3. **Graph is busy or the delta token expired**: throttling is retried, an expired token restarts a full sync, and a failed run keeps the previous cursor so no change is lost (Task 3 `throttling_is_retried_then_gives_up`, Task 6 `expired_delta_token_triggers_a_full_resync`, `a_failed_run_keeps_the_saved_cursor`).
4. **Credentials leaking to the wrong place**: a `nextLink` or `Location` pointing at another host must never receive the bearer token; pre-authenticated URLs must never receive it (Task 3 `links_to_other_hosts_are_refused`, `download_follows_the_redirect_without_the_token`, `upload_chunks_are_sent_without_the_token`).
5. **Messy Department values**: "Road & Bridge", "road and bridge ", "ROAD  AND BRIDGE" are one mapping; blank or unknown values give a person no department (they can still sign in) and appear on the admin page with counts (Task 5 `normalize_folds_case_ampersands_and_spaces`, Task 6 `first_sync_creates_members_maps_departments_and_skips_guests`).

## File structure

| File | Responsibility |
|---|---|
| `crates/pdms-core/src/config.rs` (modify) | `GraphConfig`, `EntraSyncConfig`, validation |
| `crates/pdms-core/src/graph/mod.rs` (create) | module root, re-exports |
| `crates/pdms-core/src/graph/error.rs` (create) | `GraphError`, HTTP → error mapping, `From<GraphError> for CoreError` |
| `crates/pdms-core/src/graph/cert.rs` (create) | PEM parsing, thumbprint, PS256 client assertion, certificate generation |
| `crates/pdms-core/src/graph/client.rs` (create) | `GraphClient`, token cache, retries, `get/post/patch/delete/get_delta/put_bytes/download` |
| `crates/pdms-core/src/lib.rs` (modify) | `pub mod graph;`, `Core.graph` |
| `migrations/0010_entra_sync.sql` (create) | `user_source`, user columns, `department_entra_values`, `sync_cursors` |
| `crates/pdms-core/src/model.rs` (modify) | `UserSource` enum |
| `crates/pdms-core/src/directory/entra_values.rs` (create) | normalize, list, lookup, upsert, remove, unmapped |
| `crates/pdms-core/src/directory/entra_sync.rs` (create) | run, run_full, sync_one, status, request_sync, schedule_daily, run_job |
| `crates/pdms-core/src/directory/mod.rs` (modify) | declare the two modules |
| `crates/pdms-core/src/maintenance.rs` (modify) | sync job + daily tick |
| `crates/pdms-core/src/auth/oidc.rs` (modify) | `resolve_identity`, `normalize_issuer`, no email linking, skip group reconcile when syncing |
| `crates/pdms/src/main.rs` (modify) | `gen-graph-cert`, `sync-directory` |
| `crates/pdms-web/src/routes/admin.rs`, `src/lib.rs`, `templates/admin/entra.html`, `templates/base.html` | admin page |
| `crates/pdms-core/tests/mock_graph/mod.rs` (create) | in-process mock Graph (reused by P3/P4) |
| `crates/pdms-core/tests/graph.rs`, `tests/entra_sync.rs` (create) | core tests |
| `crates/pdms-web/tests/entra.rs` (create), `tests/oidc.rs` (modify) | web tests |
| `Cargo.toml`, `crates/pdms-core/Cargo.toml` (modify) | dependencies, dev-profile speed-up for RSA |
| `docs/ADMIN_GUIDE.md`, `config/pdms.example.toml`, `docs/DECISIONS.md`, `docs/ROADMAP.md` | documentation |

---

### Task 1: `[graph]` and `[entra_sync]` configuration

**Files:**
- Modify: `crates/pdms-core/src/config.rs` (struct `Config` at line ~54; new structs after `GraphCalendarConfig`'s `Default` impl at line ~623; `validate()` at line ~700; tests module at the end)

**Interfaces:**
- Consumes: nothing.
- Produces: `pdms_core::config::GraphConfig { tenant_id: String, client_id: String, certificate: Secret, authority_url: String, api_url: String, timeout_seconds: u64, max_retries: u32 }` with `is_configured(&self) -> bool`; `pdms_core::config::EntraSyncConfig { enabled: bool, run_at: String, attribute: String, include_guests: bool }` with `run_at_time(&self) -> Option<chrono::NaiveTime>`; `Config.graph`, `Config.entra_sync`.

- [ ] **Step 1: Write the failing tests.** Append inside `mod tests` in `crates/pdms-core/src/config.rs`:

```rust
    #[test]
    fn graph_and_entra_sync_defaults_and_validation() {
        let c = Config::default();
        assert!(!c.graph.is_configured());
        assert_eq!(c.graph.authority_url, "https://login.microsoftonline.com");
        assert_eq!(c.graph.api_url, "https://graph.microsoft.com/v1.0");
        assert!(!c.entra_sync.enabled);
        assert_eq!(c.entra_sync.run_at, "02:30");
        assert_eq!(c.entra_sync.attribute, "department");
        assert!(!c.entra_sync.include_guests);

        let mut c = Config::default();
        c.graph.tenant_id = "t".into();
        assert!(c.validate().is_err(), "partial [graph] is rejected");
        c.graph.client_id = "c".into();
        c.graph.certificate = Secret::new("-----BEGIN CERTIFICATE-----");
        assert!(c.validate().is_ok());
        assert!(c.graph.is_configured());
        c.graph.api_url = "http://graph.example.com/v1.0".into();
        assert!(c.validate().is_err(), "plain http only to loopback");
        c.graph.api_url = "http://127.0.0.1:9999/v1.0".into();
        assert!(c.validate().is_ok());

        let mut c = Config::default();
        c.entra_sync.enabled = true;
        assert!(c.validate().is_err(), "needs [graph] and an OIDC issuer");
        c.graph.tenant_id = "t".into();
        c.graph.client_id = "c".into();
        c.graph.certificate = Secret::new("x");
        c.auth.oidc.issuer_url = "https://login.microsoftonline.com/t/v2.0".into();
        assert!(c.validate().is_ok());
        c.entra_sync.run_at = "25:00".into();
        assert!(c.validate().is_err());
        c.entra_sync.run_at = "02:30".into();
        assert_eq!(
            c.entra_sync.run_at_time(),
            chrono::NaiveTime::from_hms_opt(2, 30, 0)
        );
        c.entra_sync.attribute = "department,mail".into();
        assert!(c.validate().is_err(), "the attribute goes into $select");
    }

    #[test]
    fn graph_certificate_is_read_from_a_file() {
        let dir = std::env::temp_dir().join(format!("pdms-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("graph_certificate");
        std::fs::write(&f, "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n").unwrap();
        let c = Config::load_from(
            None,
            &env(&[("PDMS_GRAPH__CERTIFICATE_FILE", f.to_str().unwrap())]),
        )
        .unwrap();
        assert!(c.graph.certificate.expose().starts_with("-----BEGIN CERTIFICATE-----\nAAAA"));
        assert_eq!(format!("{:?}", c.graph.certificate), "\"[redacted]\"");
        std::fs::remove_dir_all(dir).unwrap();
    }
```

- [ ] **Step 2: Run them to see them fail.**

Run: `cargo test -p pdms-core --lib config::tests::graph`
Expected: FAIL to compile — `no field graph on type Config`.

- [ ] **Step 3: Implement.** In `struct Config` add, after `pub graph_calendar: GraphCalendarConfig,`:

```rust
    /// Microsoft Graph as PDMS itself (app-only, certificate). Used by the
    /// Entra user sync and, later, SharePoint storage.
    pub graph: GraphConfig,
    pub entra_sync: EntraSyncConfig,
```

After `impl Default for GraphCalendarConfig { ... }` add:

```rust
/// Microsoft Graph with an app registration ("PDMS Service") and a
/// certificate credential. All three of `tenant_id`, `client_id`, and
/// `certificate` are needed; with none of them set, Graph is off.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct GraphConfig {
    pub tenant_id: String,
    pub client_id: String,
    /// PEM: the certificate followed by its unencrypted RSA private key
    /// (`PRIVATE KEY` or `RSA PRIVATE KEY`). Supply it as a file:
    /// `PDMS_GRAPH__CERTIFICATE_FILE=/run/secrets/graph_certificate`.
    pub certificate: Secret,
    /// Overridable for tests and national clouds.
    pub authority_url: String,
    pub api_url: String,
    pub timeout_seconds: u64,
    /// Retries after HTTP 429/503/504 before giving up.
    pub max_retries: u32,
}

impl Default for GraphConfig {
    fn default() -> Self {
        GraphConfig {
            tenant_id: String::new(),
            client_id: String::new(),
            certificate: Secret::default(),
            authority_url: "https://login.microsoftonline.com".into(),
            api_url: "https://graph.microsoft.com/v1.0".into(),
            timeout_seconds: 60,
            max_retries: 4,
        }
    }
}

impl GraphConfig {
    pub fn is_configured(&self) -> bool {
        !self.tenant_id.is_empty() && !self.client_id.is_empty() && !self.certificate.is_empty()
    }
}

/// People and departments from Entra ID (D-086).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct EntraSyncConfig {
    pub enabled: bool,
    /// Daily run time, `HH:MM` in `[org] timezone`.
    pub run_at: String,
    /// Entra user property that names the department.
    pub attribute: String,
    /// Also sync guest (B2B) accounts. Off: guests cannot sign in.
    pub include_guests: bool,
}

impl Default for EntraSyncConfig {
    fn default() -> Self {
        EntraSyncConfig {
            enabled: false,
            run_at: "02:30".into(),
            attribute: "department".into(),
            include_guests: false,
        }
    }
}

impl EntraSyncConfig {
    pub fn run_at_time(&self) -> Option<chrono::NaiveTime> {
        chrono::NaiveTime::parse_from_str(self.run_at.trim(), "%H:%M").ok()
    }
}

/// `https`, or plain `http` to this machine (tests and local mocks).
fn https_or_loopback(s: &str) -> bool {
    Url::parse(s).is_ok_and(|u| {
        u.scheme() == "https"
            || (u.scheme() == "http"
                && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    })
}
```

In `validate()`, before the final `Ok(())`, add:

```rust
        let g = &self.graph;
        let parts = [!g.tenant_id.is_empty(), !g.client_id.is_empty(), !g.certificate.is_empty()];
        if parts.iter().any(|p| *p) && !parts.iter().all(|p| *p) {
            return bad("graph needs tenant_id, client_id, and certificate together".into());
        }
        if !https_or_loopback(&g.authority_url) || !https_or_loopback(&g.api_url) {
            return bad("graph.authority_url and graph.api_url must be https URLs".into());
        }
        let e = &self.entra_sync;
        if e.enabled {
            if !g.is_configured() {
                return bad("entra_sync needs [graph] tenant_id, client_id, and certificate".into());
            }
            if self.auth.oidc.issuer_url.trim().is_empty() {
                return bad(
                    "entra_sync needs auth.oidc.issuer_url so synced people can sign in".into(),
                );
            }
        }
        if e.run_at_time().is_none() {
            return bad(format!("entra_sync.run_at {:?} must be HH:MM", e.run_at));
        }
        let attr_ok = e.attribute.starts_with(|c: char| c.is_ascii_alphabetic())
            && e.attribute.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !attr_ok {
            return bad(format!(
                "entra_sync.attribute {:?} must be one Entra property name",
                e.attribute
            ));
        }
```

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p pdms-core --lib config::tests`
Expected: PASS (all config tests, including the two new ones).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdms-core/src/config.rs
git commit -m "config: add [graph] and [entra_sync] sections"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **File test environment.** `graph_certificate_is_read_from_a_file` must also pass `("PDMS_GRAPH__TENANT_ID", "t")` and `("PDMS_GRAPH__CLIENT_ID", "c")`. This task's own validation rejects a partial `[graph]`, so without them the test panics.
- **Subject claim.** When `entra_sync.enabled` is true, `validate()` also rejects any `auth.oidc.subject_claim` other than `"oid"`, and the error names `subject_claim`. Synced accounts are keyed by object ID. Add this assertion to `graph_and_entra_sync_defaults_and_validation`. Do **not** require `auth.oidc.enabled`: the sync may run before SSO is switched on.


---

### Task 2: Certificate credential and PS256 client assertion

**Files:**
- Create: `crates/pdms-core/src/graph/mod.rs`, `crates/pdms-core/src/graph/error.rs`, `crates/pdms-core/src/graph/cert.rs`
- Modify: `crates/pdms-core/src/lib.rs` (add `pub mod graph;` after `pub mod error;`)
- Modify: `crates/pdms-core/Cargo.toml` (`[dependencies]`)
- Modify: `Cargo.toml` (workspace root, append profile overrides)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pdms_core::graph::GraphError` (all variants of the index contract) with `GraphError::from_response(status: reqwest::StatusCode, body: &str) -> GraphError`; `impl From<GraphError> for CoreError`; `pdms_core::graph::cert::{Credential, GeneratedCertificate, generate, thumbprint}` with `Credential::from_pem(&str) -> Result<Credential, GraphError>`, `Credential::thumbprint(&self) -> &str`, `Credential::client_assertion(&self, client_id: &str, audience: &str, now: i64) -> Result<String, GraphError>`, `Credential::public_key_der(&self) -> &[u8]`, `generate(common_name: &str, valid_days: u32) -> Result<GeneratedCertificate, GraphError>` where `GeneratedCertificate { pem: String /* certificate + key */, certificate_pem: String, thumbprint: String, not_after: chrono::DateTime<chrono::Utc> }`.

Dependency choices (justification): `ring` is already in `Cargo.lock` (0.17.14, via rustls) and signs RSA-PSS in constant time, so no new signing code and no use of the `rsa` crate's private-key operations at request time (RUSTSEC-2023-0071 concerns those). `rsa` 0.9.10 is already in the lock (via openidconnect); it is used **only** to generate the key once, offline (ring cannot generate RSA keys). `rcgen` 0.14 (newest 0.14.10, 2026-08-28; 0.14.5 is from 2025-09; default features `crypto`, `pem`, `ring`) builds the self-signed X.509 certificate; it adds `rcgen`, `yasna`, and `pem` to the lock. `time` 0.3 is already in the lock (rcgen's validity fields use `time::OffsetDateTime`).

- [ ] **Step 1: Add dependencies.** In `crates/pdms-core/Cargo.toml` `[dependencies]`, add (keep alphabetical order):

```toml
rcgen = "0.14.5"
ring = "0.17"
rsa = "0.9"
time = "0.3"
```

Append to the workspace root `Cargo.toml`:

```toml
# RSA key generation (graph::cert::generate: tests and `pdms gen-graph-cert`)
# takes many seconds unoptimized; optimize just these crates in dev/test builds.
[profile.dev.package.rsa]
opt-level = 3

[profile.dev.package.num-bigint-dig]
opt-level = 3
```

- [ ] **Step 2: Write the failing tests.** Create `crates/pdms-core/src/graph/mod.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft Graph as PDMS itself: app-only tokens from a certificate, and a
//! small client for JSON, delta queries, uploads, and downloads.

pub mod cert;
mod error;

pub use error::GraphError;
```

Create `crates/pdms-core/src/graph/cert.rs` with only the tests for now:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Certificate credentials for Microsoft Graph.

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    #[test]
    fn thumbprint_is_base64url_sha256() {
        assert_eq!(thumbprint(b"abc"), "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0");
    }

    #[test]
    fn generated_certificate_signs_verifiable_ps256_assertions() {
        let g = generate("PDMS test", 30).unwrap();
        assert!(g.pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(g.pem.contains("-----BEGIN PRIVATE KEY-----"));
        let c = Credential::from_pem(&g.pem).unwrap();
        assert_eq!(c.thumbprint(), g.thumbprint);

        let aud = "https://login.example/t/oauth2/v2.0/token";
        let jwt = c.client_assertion("client-1", aud, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "PS256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["x5t#S256"], g.thumbprint.as_str());
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], aud);
        assert_eq!(claims["iss"], "client-1");
        assert_eq!(claims["sub"], "client-1");
        assert_eq!(claims["nbf"], 1_700_000_000);
        assert_eq!(claims["exp"], 1_700_000_600);
        assert_eq!(claims["jti"].as_str().unwrap().len(), 36);

        let key = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PSS_2048_8192_SHA256,
            c.public_key_der(),
        );
        key.verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pkcs1_keys_from_openssl_are_accepted() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::pkcs8::{DecodePrivateKey, LineEnding};
        let g = generate("PDMS test", 30).unwrap();
        let key_pem = &g.pem[g.certificate_pem.len()..];
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(key_pem).unwrap();
        let pkcs1 = key.to_pkcs1_pem(LineEnding::LF).unwrap();
        let bundle = format!("{}{}", g.certificate_pem, pkcs1.as_str());
        assert_eq!(Credential::from_pem(&bundle).unwrap().thumbprint(), g.thumbprint);
    }

    #[test]
    fn missing_or_encrypted_keys_are_rejected() {
        let g = generate("PDMS test", 30).unwrap();
        let e = Credential::from_pem(&g.certificate_pem).unwrap_err().to_string();
        assert!(e.contains("PRIVATE KEY"), "{e}");
        let encrypted = format!(
            "{}-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
            g.certificate_pem
        );
        let e = Credential::from_pem(&encrypted).unwrap_err().to_string();
        assert!(e.contains("unencrypted"), "{e}");
        assert!(Credential::from_pem("not a pem").is_err());
    }
}
```

Create `crates/pdms-core/src/graph/error.rs` with only its tests for now:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Errors from Microsoft Graph.

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;

    #[test]
    fn responses_map_to_error_kinds() {
        let e = |s: u16, body: &str| GraphError::from_response(StatusCode::from_u16(s).unwrap(), body);
        assert!(matches!(e(404, ""), GraphError::NotFound));
        assert!(matches!(e(409, ""), GraphError::Conflict));
        assert!(matches!(e(412, ""), GraphError::Conflict));
        assert!(matches!(e(410, ""), GraphError::ResyncRequired));
        assert!(matches!(
            e(400, r#"{"error":{"code":"syncStateNotFound","message":"x"}}"#),
            GraphError::ResyncRequired
        ));
        assert!(matches!(
            e(403, r#"{"error":{"code":"Authorization_RequestDenied","message":"no"}}"#),
            GraphError::Auth(m) if m.contains("Authorization_RequestDenied")
        ));
        assert!(matches!(
            e(500, r#"{"error":{"code":"generalException","message":"boom"}}"#),
            GraphError::Http { status: 500, ref code, .. } if code == "generalException"
        ));
        let core: crate::CoreError = GraphError::Conflict.into();
        assert!(matches!(core, crate::CoreError::Conflict(_)));
        let core: crate::CoreError = GraphError::NotFound.into();
        assert!(matches!(core, crate::CoreError::Internal(_)));
    }
}
```

Add `pub mod graph;` to `crates/pdms-core/src/lib.rs` (after `pub mod error;`).

- [ ] **Step 3: Run them to see them fail.**

Run: `cargo test -p pdms-core --lib graph::`
Expected: FAIL to compile — `cannot find function thumbprint`, `cannot find type GraphError`.

- [ ] **Step 4: Implement `error.rs`.** Above the `#[cfg(test)]` block in `crates/pdms-core/src/graph/error.rs`:

```rust
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::CoreError;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("Microsoft Graph is busy; retry after {}s", retry_after.as_secs())]
    Throttled { retry_after: Duration },
    #[error("not found in Microsoft Graph")]
    NotFound,
    #[error("conflict in Microsoft Graph (the item exists or changed)")]
    Conflict,
    #[error("Microsoft Graph asks for a full resynchronization")]
    ResyncRequired,
    #[error("Microsoft Graph authentication: {0}")]
    Auth(String),
    #[error("Microsoft Graph error {status} {code}: {message}")]
    Http { status: u16, code: String, message: String },
    #[error("Microsoft Graph unreachable: {0}")]
    Transport(String),
}

impl GraphError {
    /// Map a non-success response (after retries) to an error.
    pub fn from_response(status: StatusCode, body: &str) -> GraphError {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let code = v
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let message: String = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(300)
            .collect();
        match status.as_u16() {
            404 => GraphError::NotFound,
            409 | 412 => GraphError::Conflict,
            410 => GraphError::ResyncRequired,
            _ if matches!(
                code.as_str(),
                "resyncRequired" | "syncStateNotFound" | "syncStateInvalid"
            ) =>
            {
                GraphError::ResyncRequired
            }
            401 | 403 => GraphError::Auth(if code.is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!("{code}: {message}")
            }),
            s => GraphError::Http {
                status: s,
                code,
                message,
            },
        }
    }
}

/// Callers handle `NotFound` and `ResyncRequired` before converting; whatever
/// is left is an internal failure (not a 404 for the person in the browser).
impl From<GraphError> for CoreError {
    fn from(e: GraphError) -> Self {
        match e {
            GraphError::Conflict => CoreError::Conflict(e.to_string()),
            other => CoreError::Internal(other.to_string()),
        }
    }
}
```

- [ ] **Step 5: Implement `cert.rs`.** Above the `#[cfg(test)]` block in `crates/pdms-core/src/graph/cert.rs`:

```rust
//! Entra ID accepts a JWT signed with the app's certificate in place of a
//! client secret: PS256, with the certificate's SHA-256 thumbprint in
//! `x5t#S256` (learn.microsoft.com, "certificate credentials").

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PSS_SHA256, RsaKeyPair};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::GraphError;

/// Lifetime of one client assertion (Entra: keep it to 5–10 minutes).
const ASSERTION_SECONDS: i64 = 600;

/// The app's certificate and private key, ready to sign assertions.
pub struct Credential {
    key: RsaKeyPair,
    thumbprint: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("thumbprint", &self.thumbprint)
            .finish_non_exhaustive()
    }
}

impl Credential {
    /// Parse a PEM bundle: the certificate and its unencrypted RSA key.
    pub fn from_pem(pem: &str) -> Result<Credential, GraphError> {
        let blocks = pem_blocks(pem)?;
        let cert = blocks
            .iter()
            .find(|(label, _)| label == "CERTIFICATE")
            .ok_or_else(|| GraphError::Auth("the certificate PEM has no CERTIFICATE block".into()))?;
        let key = match blocks.iter().find(|(label, _)| label.ends_with("PRIVATE KEY")) {
            Some((label, der)) if label == "PRIVATE KEY" => RsaKeyPair::from_pkcs8(der),
            Some((label, der)) if label == "RSA PRIVATE KEY" => RsaKeyPair::from_der(der),
            Some((label, _)) => {
                return Err(GraphError::Auth(format!(
                    "unsupported key {label:?}: use an unencrypted RSA private key"
                )));
            }
            None => {
                return Err(GraphError::Auth(
                    "the certificate PEM has no PRIVATE KEY block".into(),
                ));
            }
        }
        .map_err(|e| GraphError::Auth(format!("private key rejected: {e}")))?;
        Ok(Credential {
            key,
            thumbprint: thumbprint(&cert.1),
        })
    }

    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }

    /// A signed client assertion for `audience` (the token endpoint URL).
    pub fn client_assertion(
        &self,
        client_id: &str,
        audience: &str,
        now: i64,
    ) -> Result<String, GraphError> {
        let header = serde_json::json!({
            "alg": "PS256",
            "typ": "JWT",
            "x5t#S256": self.thumbprint,
        });
        let claims = serde_json::json!({
            "aud": audience,
            "iss": client_id,
            "sub": client_id,
            "jti": Uuid::new_v4().to_string(),
            "nbf": now,
            "iat": now,
            "exp": now + ASSERTION_SECONDS,
        });
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(&RSA_PSS_SHA256, &SystemRandom::new(), input.as_bytes(), &mut sig)
            .map_err(|_| GraphError::Auth("signing the client assertion failed".into()))?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }

    /// DER `RSAPublicKey` (for verifying signatures in tests).
    pub fn public_key_der(&self) -> &[u8] {
        self.key.public().as_ref()
    }
}

/// Base64url SHA-256 of a DER certificate: the `x5t#S256` header value.
pub fn thumbprint(der: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(der))
}

/// A new self-signed certificate for the "PDMS Service" app registration.
pub struct GeneratedCertificate {
    /// Certificate followed by its private key: what PDMS reads.
    pub pem: String,
    /// The public certificate alone: what you upload to Entra ID.
    pub certificate_pem: String,
    pub thumbprint: String,
    pub not_after: chrono::DateTime<chrono::Utc>,
}

/// Generate an RSA-2048 key and a self-signed certificate valid for `valid_days`.
pub fn generate(common_name: &str, valid_days: u32) -> Result<GeneratedCertificate, GraphError> {
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    let fail = |what: &str, e: &dyn std::fmt::Display| GraphError::Auth(format!("{what}: {e}"));
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)
        .map_err(|e| fail("generating the key", &e))?;
    let key_pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| fail("encoding the key", &e))?;
    let signer = rcgen::KeyPair::from_pkcs8_pem_and_sign_algo(&key_pem, &rcgen::PKCS_RSA_SHA256)
        .map_err(|e| fail("loading the key", &e))?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())
        .map_err(|e| fail("certificate parameters", &e))?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, common_name);
    params.distinguished_name = dn;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(i64::from(valid_days));
    let cert = params
        .self_signed(&signer)
        .map_err(|e| fail("signing the certificate", &e))?;
    let certificate_pem = cert.pem();
    Ok(GeneratedCertificate {
        pem: format!("{certificate_pem}{}", key_pem.as_str()),
        thumbprint: thumbprint(cert.der()),
        certificate_pem,
        not_after: chrono::Utc::now() + chrono::Duration::days(i64::from(valid_days)),
    })
}

/// `(label, DER)` for every PEM block in `text`.
fn pem_blocks(text: &str) -> Result<Vec<(String, Vec<u8>)>, GraphError> {
    let mut out = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in text.lines().map(str::trim) {
        if let Some(label) = line.strip_prefix("-----BEGIN ").and_then(|l| l.strip_suffix("-----")) {
            open = Some((label.to_owned(), String::new()));
        } else if let Some(label) = line.strip_prefix("-----END ").and_then(|l| l.strip_suffix("-----")) {
            let Some((begun, body)) = open.take() else {
                return Err(GraphError::Auth(format!("PEM END {label:?} without BEGIN")));
            };
            if begun != label {
                return Err(GraphError::Auth(format!("PEM block {begun:?} ends as {label:?}")));
            }
            let der = STANDARD
                .decode(body)
                .map_err(|e| GraphError::Auth(format!("PEM {label}: {e}")))?;
            out.push((label.to_owned(), der));
        } else if let Some((_, body)) = open.as_mut() {
            body.push_str(line);
        }
    }
    Ok(out)
}
```

- [ ] **Step 6: Run the tests.**

Run: `cargo test -p pdms-core --lib graph::`
Expected: PASS — `thumbprint_is_base64url_sha256`, `generated_certificate_signs_verifiable_ps256_assertions`, `pkcs1_keys_from_openssl_are_accepted`, `missing_or_encrypted_keys_are_rejected`, `responses_map_to_error_kinds` (5 passed). Key generation should take well under a second with the profile override; if it takes many seconds, the override in the root `Cargo.toml` is missing.

- [ ] **Step 7: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/pdms-core/Cargo.toml crates/pdms-core/src/lib.rs crates/pdms-core/src/graph
git commit -m "graph: certificate credential with PS256 client assertions"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Module order.** In `crates/pdms-core/src/lib.rs`, put `pub mod graph;` after `pub mod exports;`, not after `pub mod error;`. That is rustfmt's order (R1 added `pub mod branding;`). Run `cargo fmt`.
- **rcgen.** The rcgen 0.14 calls in this task could not be checked offline. If the real API differs (`KeyPair::from_pkcs8_pem_and_sign_algo`, `CertificateParams::new(..)`, `self_signed(&KeyPair)`, `cert.der()`), adapt to it, keep the behaviour, and say so in your report.


---

### Task 3: The Graph client, and a mock Graph for tests

**Files:**
- Create: `crates/pdms-core/src/graph/client.rs`
- Modify: `crates/pdms-core/src/graph/mod.rs`
- Modify: `crates/pdms-core/Cargo.toml` (`bytes`; `reqwest` with `stream`; dev-dependencies `axum`, `serde_urlencoded`, `bytes`)
- Create: `crates/pdms-core/tests/mock_graph/mod.rs` (a directory module, so Cargo does not build it as its own test binary; P3/P4 extend it)
- Create: `crates/pdms-core/tests/graph.rs`

**Interfaces:**
- Consumes: `GraphConfig` (Task 1), `Credential`, `GraphError`, `cert::generate` (Task 2).
- Produces (index contract): `pdms_core::graph::GraphClient` (`Clone`) with `new(&GraphConfig) -> Result<GraphClient, GraphError>`, `thumbprint(&self) -> &str`, `get<T: DeserializeOwned>(&self, path: &str) -> Result<T, GraphError>`, `post<B: Serialize + ?Sized, T: DeserializeOwned>(&self, path: &str, body: &B)`, `patch<B, T>(&self, path, body)` (same bounds), `delete(&self, path: &str) -> Result<(), GraphError>`, `get_delta<T: DeserializeOwned>(&self, start: &str) -> Result<DeltaPage<T>, GraphError>`, `put_bytes(&self, absolute_url: &str, bytes: bytes::Bytes, range: (u64, u64, u64)) -> Result<Option<serde_json::Value>, GraphError>`, `download(&self, path: &str) -> Result<pdms_storage::ByteStream, GraphError>`; `pdms_core::graph::DeltaPage<T> { items: Vec<T>, delta_link: String }`. Test module `mock_graph` with `MockGraph::start()`, `MockGraph::config(&str) -> GraphConfig`, `member(...)`, `add_user/update_user/remove_user/fail_next/requests/tokens_issued`, consts `TENANT`, `CLIENT`.

- [ ] **Step 1: Dependencies.** In `crates/pdms-core/Cargo.toml` `[dependencies]` add `bytes = "1"` and change the reqwest line to `reqwest = { workspace = true, features = ["stream"] }`. In `[dev-dependencies]` add:

```toml
axum.workspace = true
bytes = "1"
serde_urlencoded.workspace = true
```

- [ ] **Step 2: Write the mock Graph.** Create `crates/pdms-core/tests/mock_graph/mod.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! A small in-process Microsoft Graph for tests: the token endpoint, users
//! delta, single users, and a few generic endpoints for the client tests.
//! Bearer tokens look like `tok-<n>`; pre-authenticated URLs (`/blob/…`,
//! `/upload/…`) reject any Authorization header, as the real ones may.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use pdms_core::config::{GraphConfig, Secret};
use serde_json::{Value, json};

pub const TENANT: &str = "tenant-1";
pub const CLIENT: &str = "client-1";

#[derive(Default)]
pub struct MockState {
    pub tokens_issued: usize,
    /// Tokens numbered below this are rejected with 401 (simulates expiry).
    pub min_token: usize,
    pub token_forms: Vec<HashMap<String, String>>,
    pub requests: Vec<String>,
    pub users: BTreeMap<String, Value>,
    /// Changes since the last delta round.
    pub pending: Vec<Value>,
    round: Vec<Value>,
    pub delta_token: u64,
    pub page_size: usize,
    /// Scripted failures for the next authenticated requests: (status, Retry-After seconds).
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
    json_resp(status, json!({ "error": { "code": code, "message": code } }))
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
    s.requests.push(format!("{method} {uri}"));
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer tok-"))
        .and_then(|n| n.parse::<usize>().ok());
    let has_auth = headers.contains_key(header::AUTHORIZATION);

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
            json_resp(202, json!({ "nextExpectedRanges": [format!("{}-", end + 1)] }))
        };
    }
    let Some(api) = path.strip_prefix("/v1.0") else {
        return graph_error(404, "notFound");
    };
    if !bearer.is_some_and(|n| n >= s.min_token) {
        return graph_error(401, "InvalidAuthenticationToken");
    }
    if let Some((status, after)) = s.fail_next.pop_front() {
        let mut r = graph_error(status, "TooManyRequests");
        r.headers_mut()
            .insert(header::RETRY_AFTER, after.to_string().parse().unwrap());
        return r;
    }
    match (method, api) {
        (Method::GET, "/users/delta") => users_delta(&mut s, &base, &query),
        (Method::GET, p) if p.starts_with("/users/") => {
            match s.users.get(&p["/users/".len()..]) {
                Some(u) => json_resp(200, u.clone()),
                None => graph_error(404, "Request_ResourceNotFound"),
            }
        }
        (Method::GET, "/test/json") => json_resp(200, json!({ "ok": true })),
        (Method::GET, "/test/download") => {
            let mut r = StatusCode::FOUND.into_response();
            r.headers_mut().insert(
                header::LOCATION,
                format!("{base}/blob/report.pdf").parse().unwrap(),
            );
            r
        }
        (Method::GET, "/test/elsewhere") => {
            let mut r = StatusCode::FOUND.into_response();
            r.headers_mut().insert(
                header::LOCATION,
                "http://203.0.113.9/blob/x".parse().unwrap(),
            );
            r
        }
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
    let end = span.split_once('-').map_or(0, |(_, e)| e.parse().unwrap_or(0));
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
    let end = (offset + s.page_size.max(1)).min(s.round.len());
    let value: Vec<Value> = s.round[offset.min(end)..end].to_vec();
    let mut page = json!({ "value": value });
    if end < s.round.len() {
        page["@odata.nextLink"] = json!(format!("{base}/v1.0/users/delta?$skiptoken={end}"));
    } else {
        s.delta_token += 1;
        page["@odata.deltaLink"] =
            json!(format!("{base}/v1.0/users/delta?$deltatoken={}", s.delta_token));
    }
    json_resp(200, page)
}
```

- [ ] **Step 3: Write the failing client tests.** Create `crates/pdms-core/tests/graph.rs`:

```rust
#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! The Microsoft Graph client against an in-process mock Graph.

mod mock_graph;

use std::sync::OnceLock;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use mock_graph::*;
use pdms_core::graph::{GraphClient, GraphError, cert};
use serde_json::{Value, json};

/// One certificate per test binary (key generation is the slow part).
fn certificate() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| cert::generate("PDMS test", 30).unwrap().pem)
}

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
    assert_eq!(claims["aud"], format!("{}/{TENANT}/oauth2/v2.0/token", m.base));
    assert_eq!(claims["iss"], CLIENT);
}

#[tokio::test]
async fn an_expired_token_is_renewed_once() {
    let (m, g) = client().await;
    let _: Value = g.get("/test/json").await.unwrap();
    m.state.lock().unwrap().min_token = 2;
    let _: Value = g.get("/test/json").await.unwrap();
    assert_eq!(m.tokens_issued(), 2);
}

#[tokio::test]
async fn throttling_is_retried_then_gives_up() {
    let (m, g) = client().await;
    m.fail_next(429, 0);
    m.fail_next(503, 0);
    let v: Value = g.get("/test/json").await.unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(count(&m, "/test/json"), 3);

    for _ in 0..4 {
        m.fail_next(429, 0);
    }
    let e = g.get::<Value>("/test/json").await.unwrap_err();
    assert!(matches!(e, GraphError::Throttled { .. }), "{e}");
    assert_eq!(count(&m, "/test/json"), 3 + 4, "one try plus max_retries (3)");
}

#[tokio::test]
async fn errors_map_to_kinds() {
    let (_m, g) = client().await;
    assert!(matches!(g.get::<Value>("/nope").await, Err(GraphError::NotFound)));
    assert!(matches!(g.get::<Value>("/test/gone").await, Err(GraphError::ResyncRequired)));
    assert!(matches!(g.get::<Value>("/test/conflict").await, Err(GraphError::Conflict)));
}

#[tokio::test]
async fn delta_follows_next_links_to_the_delta_link() {
    let (m, g) = client().await;
    for i in 0..5 {
        m.add_user(member(&format!("00000000-0000-0000-0000-00000000000{i}"), "U", "u@x.org", None));
    }
    let page = g.get_delta::<Value>("/users/delta?$select=id").await.unwrap();
    assert_eq!(page.items.len(), 5);
    assert!(page.delta_link.ends_with("$deltatoken=1"), "{}", page.delta_link);
    assert_eq!(count(&m, "$skiptoken="), 2, "pages of 2");
}

#[tokio::test]
async fn links_to_other_hosts_are_refused() {
    let (m, g) = client().await;
    let e = g.get::<Value>("https://evil.example/v1.0/users").await.unwrap_err();
    assert!(matches!(e, GraphError::Transport(_)), "{e}");
    let e = g.download("/test/elsewhere").await.err().unwrap();
    assert!(matches!(e, GraphError::Transport(_)), "{e}");
    assert!(m.requests().iter().all(|r| !r.contains("evil.example")));
}

#[tokio::test]
async fn download_follows_the_redirect_without_the_token() {
    let (_m, g) = client().await;
    let mut s = g.download("/test/download").await.unwrap();
    let mut bytes = Vec::new();
    while let Some(c) = s.next().await {
        bytes.extend_from_slice(&c.unwrap());
    }
    assert_eq!(bytes, b"contents of report.pdf");
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
    let ranges: Vec<String> = m.state.lock().unwrap().uploads.iter().map(|u| u.1.clone()).collect();
    assert_eq!(ranges, vec!["bytes 0-4/10", "bytes 5-9/10"]);
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
```

- [ ] **Step 4: Run them to see them fail.**

Run: `cargo test -p pdms-core --test graph`
Expected: FAIL to compile — `unresolved import pdms_core::graph::GraphClient`.

- [ ] **Step 5: Implement the client.** Create `crates/pdms-core/src/graph/client.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft Graph over HTTPS with an app-only token. Throttling is retried;
//! the bearer token goes only to `api_url`'s origin and never follows a
//! redirect; pre-authenticated URLs are fetched without it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::{Method, StatusCode, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;
use url::Url;

use super::GraphError;
use super::cert::Credential;
use crate::config::GraphConfig;

/// Longest single wait honoured from `Retry-After`.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// Renew the token this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);
/// Loop guard when following `@odata.nextLink`.
const MAX_PAGES: usize = 10_000;

/// A Graph client acting as the PDMS app registration. Cheap to clone.
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
            .user_agent(concat!("PDMS/", env!("CARGO_PKG_VERSION")))
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
        let resp = self.send(Method::GET, path, None).await?;
        json_body(resp).await
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GraphError> {
        let v = serde_json::to_value(body).map_err(|e| GraphError::Transport(e.to_string()))?;
        json_body(self.send(Method::POST, path, Some(&v)).await?).await
    }

    pub async fn patch<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GraphError> {
        let v = serde_json::to_value(body).map_err(|e| GraphError::Transport(e.to_string()))?;
        json_body(self.send(Method::PATCH, path, Some(&v)).await?).await
    }

    pub async fn delete(&self, path: &str) -> Result<(), GraphError> {
        self.send(Method::DELETE, path, None).await?;
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
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
                .body(bytes.clone())
                .send()
                .await
                .map_err(|e| GraphError::Transport(e.to_string()))?;
            let status = resp.status();
            if status == StatusCode::ACCEPTED {
                return Ok(None);
            }
            if status.is_success() {
                return Ok(Some(json_body(resp).await?));
            }
            if is_throttle(status) {
                let wait = retry_after(&resp).unwrap_or_else(|| backoff(attempt));
                if attempt < self.inner.max_retries {
                    attempt += 1;
                    tokio::time::sleep(wait.min(MAX_WAIT)).await;
                    continue;
                }
                return Err(GraphError::Throttled { retry_after: wait });
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
            let r = self
                .inner
                .http
                .get(url)
                .send()
                .await
                .map_err(|e| GraphError::Transport(e.to_string()))?;
            if !r.status().is_success() {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                return Err(GraphError::from_response(status, &text));
            }
            r
        } else {
            first
        };
        Ok(Box::pin(
            resp.bytes_stream().map(|r| r.map_err(std::io::Error::other)),
        ))
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
            let why = body
                .get("error_description")
                .or_else(|| body.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("no details");
            return Err(GraphError::Auth(format!(
                "token request: HTTP {}: {}",
                status.as_u16(),
                why.lines().next().unwrap_or("")
            )));
        }
        let token = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| GraphError::Auth("token response without access_token".into()))?
            .to_owned();
        let lifetime = Duration::from_secs(body.get("expires_in").and_then(Value::as_u64).unwrap_or(3600));
        *cached = Some((
            token.clone(),
            Instant::now() + lifetime.saturating_sub(TOKEN_MARGIN),
        ));
        Ok(token)
    }

    async fn forget_token(&self) {
        *self.inner.token.lock().await = None;
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
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        let api_is_loopback = matches!(
            self.inner.api.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]")
        );
        let ok = url.scheme() == "https" || (url.scheme() == "http" && loopback && api_is_loopback);
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
                self.forget_token().await;
                continue;
            }
            if is_throttle(status) {
                let wait = retry_after(&resp).unwrap_or_else(|| backoff(attempt));
                if attempt < self.inner.max_retries {
                    attempt += 1;
                    tokio::time::sleep(wait.min(MAX_WAIT)).await;
                    continue;
                }
                return Err(GraphError::Throttled { retry_after: wait });
            }
            let text = resp.text().await.unwrap_or_default();
            return Err(GraphError::from_response(status, &text));
        }
    }
}

async fn json_body<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, GraphError> {
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| GraphError::Transport(e.to_string()))?;
    if bytes.is_empty() {
        serde_json::from_value(Value::Null)
    } else {
        serde_json::from_slice(&bytes)
    }
    .map_err(|e| GraphError::Transport(format!("unexpected response: {e}")))
}

fn is_throttle(s: StatusCode) -> bool {
    matches!(s.as_u16(), 429 | 503 | 504)
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let v = resp.headers().get(header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// 1, 2, 4, 8, 16, 32 seconds.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(5))
}
```

Update `crates/pdms-core/src/graph/mod.rs`:

```rust
pub mod cert;
mod client;
mod error;

pub use client::{DeltaPage, GraphClient};
pub use error::GraphError;
```

- [ ] **Step 6: Run the tests.**

Run: `cargo test -p pdms-core --test graph`
Expected: PASS — 9 tests (`token_comes_from_a_certificate_assertion_and_is_cached`, `an_expired_token_is_renewed_once`, `throttling_is_retried_then_gives_up`, `errors_map_to_kinds`, `delta_follows_next_links_to_the_delta_link`, `links_to_other_hosts_are_refused`, `download_follows_the_redirect_without_the_token`, `upload_chunks_are_sent_without_the_token`, `post_patch_and_delete_send_json`).

- [ ] **Step 7: Commit.**

```bash
git add Cargo.lock crates/pdms-core/Cargo.toml crates/pdms-core/src/graph crates/pdms-core/tests/mock_graph crates/pdms-core/tests/graph.rs
git commit -m "graph: client with token cache, retries, delta paging, uploads, downloads"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Compile fix.** Put `#[serde(bound(deserialize = "T: DeserializeOwned"))]` on `RawPage<T>`. Without it, `#[serde(default)]` on `value: Vec<T>` makes serde require `T: Default`, and `get_delta` fails with E0277.
- **`graph/mod.rs`.** Add `mod client;` and `pub use client::{DeltaPage, GraphClient};` to the existing file. Keep its SPDX header and doc comment; do not replace the whole file.
- **The other-hosts test must be able to fail.** Requests to `evil.example` never reach the mock, so asserting on the mock's `requests()` proves nothing. Start a second `MockGraph`: another port is another origin. Point the `nextLink` (and any `Location` the test uses) at it. Then assert that the call fails with `GraphError::Transport` and that the second mock received no request. Wherever a test claims a host got no bearer token, assert on what that host actually recorded.
- **Throttling test.** `throttling_is_retried_then_gives_up` also asserts that the returned `GraphError::Throttled { retry_after }` equals `Duration::ZERO` and that the call took under one second. Otherwise the test passes even when `Retry-After` is ignored.
- **No duplicated logic blocks.**
  - One private helper decides the retry wait (`Retry-After`, the backoff, `max_retries`, the 60 s cap). `send` and `put_bytes` both use it.
  - `post` and `patch` share one helper that takes the method.
  - One `is_loopback(&Url)` helper serves both `https_or_loopback` and `preauthenticated`.
  - `tests/mock_graph/mod.rs` exposes `pub fn certificate() -> &'static str` (the `OnceLock` PEM). `tests/graph.rs` and later test files reuse it instead of copying it.
  - Keep the mock's `handle` in this order: the `fail_next` block, then the `/blob/` and `/upload/` branches, then `match (method, api)`. P3 adds code at those points.
- **Jitter (spec §10).** When a response has no `Retry-After`, the exponential backoff adds random jitter (`rand` 0.8 is already a `pdms-core` dependency). It still honours the 60 s cap.
- **SPDX first.** New test files start with the two SPDX comment lines, then the `#![allow(...)]` block. That is the R1 convention (see `crates/pdms-core/tests/usernames.rs`).
- **No extra dev-dependency.** Skip the `bytes` dev-dependency; integration tests already see the regular dependencies.


---

### Task 4: `Core.graph` and `pdms gen-graph-cert`

**Files:**
- Modify: `crates/pdms-core/src/lib.rs` (struct `Core` at line ~54; `Core::new` at line ~72)
- Modify: `crates/pdms/src/main.rs` (enum `Command`, `main`, `run`)
- Test: `crates/pdms-core/tests/graph.rs` (append; add `mod common;`)

**Interfaces:**
- Consumes: `GraphClient::new` (Task 3).
- Produces: `pdms_core::Core.graph: Option<pdms_core::graph::GraphClient>` (index contract); CLI `pdms gen-graph-cert --out <file> [--days N] [--name CN]`.

- [ ] **Step 1: Write the failing test.** In `crates/pdms-core/tests/graph.rs` add `mod common;` under `mod mock_graph;`, add `use pdms_core::Core; use pdms_core::config::Secret;`, and append:

```rust
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
```

- [ ] **Step 2: Run it to see it fail.**

Run: `cargo test -p pdms-core --test graph core_has_a_graph_client`
Expected: FAIL to compile — `no field graph on type Core`.

- [ ] **Step 3: Implement.** In `crates/pdms-core/src/lib.rs`, add to `pub struct Core` after `pub ocr: ...`:

```rust
    /// Microsoft Graph as PDMS itself (`None` unless `[graph]` is configured).
    pub graph: Option<graph::GraphClient>,
```

In `Core::new`, before `Ok(Core {`, add:

```rust
        let graph = if config.graph.is_configured() {
            Some(
                graph::GraphClient::new(&config.graph)
                    .map_err(|e| CoreError::validation(format!("graph: {e}")))?,
            )
        } else {
            None
        };
```

and add `graph,` to the `Core { ... }` literal. Confirm nothing else builds a `Core` literal: `grep -rn "Core {" crates --include=*.rs | grep -v "struct\|impl\|fn "` must list only `crates/pdms-core/src/lib.rs`.

In `crates/pdms/src/main.rs`, add to `enum Command` after `GenSecret,`:

```rust
    /// Create the certificate for the Microsoft Graph app registration
    /// ("PDMS Service"). Writes certificate + private key to OUT (mode 0600)
    /// and the public certificate to OUT with the extension .crt, which you
    /// upload in Entra ID. Needs no configuration.
    GenGraphCert {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 730)]
        days: u32,
        #[arg(long, default_value = "PDMS Service")]
        name: String,
    },
```

In `main`, extend the early `match cli.command { ... }` with (the `ref` bindings keep `cli.command` usable afterwards):

```rust
        Command::GenGraphCert {
            ref out,
            days,
            ref name,
        } => return gen_graph_cert(out, days, name),
```

In `run`, change the first arm to `Command::GenSecret | Command::Healthcheck { .. } | Command::GenGraphCert { .. } => {}`. Add, after `fn read_password`:

```rust
fn gen_graph_cert(out: &std::path::Path, days: u32, name: &str) -> ExitCode {
    match write_graph_cert(out, days, name) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn write_graph_cert(out: &std::path::Path, days: u32, name: &str) -> anyhow::Result<()> {
    use std::io::Write;
    if !(30..=1095).contains(&days) {
        bail!("--days must be 30–1095");
    }
    let g = pdms_core::graph::cert::generate(name, days)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(out)
        .with_context(|| format!("creating {} (it must not exist yet)", out.display()))?
        .write_all(g.pem.as_bytes())?;
    let public = out.with_extension("crt");
    std::fs::write(&public, &g.certificate_pem)
        .with_context(|| format!("writing {}", public.display()))?;
    println!("Certificate and private key: {} (secret: give it to PDMS as PDMS_GRAPH__CERTIFICATE_FILE)", out.display());
    println!("Public certificate for Entra ID (App registrations → PDMS Service → Certificates & secrets → Upload): {}", public.display());
    println!("SHA-256 thumbprint (x5t#S256): {}", g.thumbprint);
    println!("Expires: {}", g.not_after.format("%Y-%m-%d"));
    Ok(())
}
```

- [ ] **Step 4: Run the tests and the CLI.**

Run: `cargo test -p pdms-core --test graph`
Expected: PASS (10 tests).

Run (in a scratch directory):

```bash
cargo run -q -p pdms -- gen-graph-cert --out /tmp/pdms-graph-test.pem
stat -c '%a' /tmp/pdms-graph-test.pem
head -1 /tmp/pdms-graph-test.crt
cargo run -q -p pdms -- gen-graph-cert --out /tmp/pdms-graph-test.pem; echo "exit $?"
rm /tmp/pdms-graph-test.pem /tmp/pdms-graph-test.crt
```

Expected: four lines printed (key file, public certificate, thumbprint, expiry two years out); `600`; `-----BEGIN CERTIFICATE-----`; the second run prints `error: creating /tmp/pdms-graph-test.pem (it must not exist yet): …` and `exit 1`.

- [ ] **Step 5: Commit.**

```bash
git add crates/pdms-core/src/lib.rs crates/pdms/src/main.rs crates/pdms-core/tests/graph.rs
git commit -m "core: Graph client on Core; pdms gen-graph-cert"
```

---

### Task 5: Migration 0010 and the Department mapping list

**Files:**
- Create: `migrations/0010_entra_sync.sql`
- Modify: `crates/pdms-core/src/model.rs` (add `UserSource` after `MembershipSource`)
- Create: `crates/pdms-core/src/directory/entra_values.rs`
- Modify: `crates/pdms-core/src/directory/mod.rs` (after `pub mod sync;` add `pub mod entra_values;`)
- Create: `crates/pdms-core/tests/entra_sync.rs`

**Interfaces:**
- Consumes: `Capability::ManageDirectory`, audit API.
- Produces (index contract): DB `user_source` enum, `users.source`, `users.entra_department`, `users.entra_synced_at`, `department_entra_values`, `sync_cursors (name, delta_link, last_run, last_error, last_summary jsonb)`; `pdms_core::model::UserSource { Local, Entra }`; `pdms_core::directory::entra_values::{normalize(&str) -> String, list(&PgPool) -> CoreResult<Vec<EntraValue>>, lookup(&PgPool, Option<&str>) -> CoreResult<Option<Uuid>>, upsert(&PgPool, &Ctx, value: &str, department_id: Uuid) -> CoreResult<()>, remove(&PgPool, &Ctx, value: &str) -> CoreResult<()>, unmapped(&PgPool) -> CoreResult<Vec<UnmappedValue>>}`, `EntraValue { value: String, normalized: String, department_id: Uuid, department_name: String }`, `UnmappedValue { value: String, normalized: String, users: i64 }`.

- [ ] **Step 1: Write the migration.** Create `migrations/0010_entra_sync.sql`:

```sql
-- Entra ID user sync (spec 2026-10-14, §6; D-086): who manages each account,
-- the raw Department value, Department → PDMS department mappings, and cursors
-- for change tracking.

CREATE TYPE user_source AS ENUM ('local', 'entra');

ALTER TABLE users
    ADD COLUMN source           user_source NOT NULL DEFAULT 'local',
    ADD COLUMN entra_department text,
    ADD COLUMN entra_synced_at  timestamptz;

-- Issuers compare without a trailing slash (auth::oidc::normalize_issuer).
UPDATE users SET oidc_issuer = rtrim(oidc_issuer, '/') WHERE oidc_issuer LIKE '%/';

CREATE TABLE department_entra_values (
    value_normalized text PRIMARY KEY CHECK (value_normalized <> ''),
    value_display    text NOT NULL CHECK (length(value_display) BETWEEN 1 AND 200),
    department_id    uuid NOT NULL REFERENCES departments (id),
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX department_entra_values_department_idx ON department_entra_values (department_id);

CREATE TABLE sync_cursors (
    name         text PRIMARY KEY,   -- 'entra.users'; P4 adds 'sharepoint.delta'
    delta_link   text,
    last_run     timestamptz,
    last_error   text,
    last_summary jsonb
);

GRANT SELECT, INSERT, UPDATE, DELETE ON department_entra_values TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON sync_cursors TO pdms_app;
```

Apply it to the dev database: `sqlx migrate run --source migrations` → `Applied 10/migrate entra sync`.

- [ ] **Step 2: Write the failing tests.** Create `crates/pdms-core/tests/entra_sync.rs`:

```rust
#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Entra ID user sync: Department mapping, delta sync, sign-in lookups.

mod common;
mod mock_graph;

use common::*;
use pdms_core::CoreError;
use pdms_core::directory::{self, entra_values};

#[tokio::test]
async fn department_values_map_case_and_spacing_insensitively() {
    let (core, _db) = core().await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb).await.unwrap();
    entra_values::upsert(&core.db, &system(), "R and B", rb).await.unwrap();
    assert_eq!(
        entra_values::lookup(&core.db, Some("  road AND   bridge")).await.unwrap(),
        Some(rb)
    );
    assert_eq!(entra_values::lookup(&core.db, Some("")).await.unwrap(), None);
    assert_eq!(entra_values::lookup(&core.db, None).await.unwrap(), None);
    assert_eq!(entra_values::list(&core.db).await.unwrap().len(), 2);

    entra_values::remove(&core.db, &system(), "r and b").await.unwrap();
    assert!(matches!(
        entra_values::remove(&core.db, &system(), "nothing").await,
        Err(CoreError::NotFound)
    ));
    assert!(matches!(
        entra_values::upsert(&core.db, &system(), "   ", rb).await,
        Err(CoreError::Validation(_))
    ));
    let (clerk, dept) = user_in_new_department(&core, "HR", "hr@x.org").await;
    assert!(matches!(
        entra_values::upsert(&core.db, &as_user(&core, clerk).await, "HR", dept).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        audit_actions(&core)
            .await
            .contains(&"directory.department_mapping_changed".to_owned())
    );
}
```

And in `crates/pdms-core/src/directory/entra_values.rs` (created in Step 4) the unit test for `normalize` — write the file with the test module first:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Entra ID "Department" values → PDMS departments (D-086).

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_folds_case_ampersands_and_spaces() {
        assert_eq!(normalize("Road & Bridge"), "road and bridge");
        assert_eq!(normalize("  road AND   bridge "), "road and bridge");
        assert_eq!(normalize("R&B"), "r and b");
        assert_eq!(normalize("   "), "");
    }
}
```

Add `pub mod entra_values;` to `crates/pdms-core/src/directory/mod.rs`.

- [ ] **Step 3: Run them to see them fail.**

Run: `cargo test -p pdms-core --lib directory::entra_values && cargo test -p pdms-core --test entra_sync`
Expected: FAIL to compile — `cannot find function normalize`.

- [ ] **Step 4: Implement.** In `crates/pdms-core/src/model.rs` after `impl MembershipSource { ... }`:

```rust
/// Who manages an account: created in PDMS, or by the Entra ID sync.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "user_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum UserSource {
    Local,
    Entra,
}
```

Above the test module in `crates/pdms-core/src/directory/entra_values.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::Capability;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

/// One mapping: the value as an admin typed it, and its department.
#[derive(Clone, Debug, Serialize)]
pub struct EntraValue {
    pub value: String,
    pub normalized: String,
    pub department_id: Uuid,
    pub department_name: String,
}

/// A Department value active Entra people have that maps to nothing.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UnmappedValue {
    /// As found on an account ("" when the field is blank).
    pub value: String,
    pub normalized: String,
    pub users: i64,
}

/// Lowercase, `&` → "and", collapsed whitespace: "Road & Bridge",
/// "road and bridge " and "ROAD  AND BRIDGE" are one value.
pub fn normalize(value: &str) -> String {
    value
        .replace('&', " and ")
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn list(db: &PgPool) -> CoreResult<Vec<EntraValue>> {
    Ok(sqlx::query_as!(
        EntraValue,
        r#"SELECT v.value_display AS value, v.value_normalized AS normalized,
                  v.department_id, d.name AS department_name
             FROM department_entra_values v JOIN departments d ON d.id = v.department_id
            ORDER BY d.name, v.value_display"#
    )
    .fetch_all(db)
    .await?)
}

/// The department for a raw attribute value, if it is mapped.
pub async fn lookup(db: &PgPool, value: Option<&str>) -> CoreResult<Option<Uuid>> {
    let Some(n) = value.map(normalize).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    Ok(sqlx::query_scalar!(
        "SELECT department_id FROM department_entra_values WHERE value_normalized = $1",
        n
    )
    .fetch_optional(db)
    .await?)
}

/// Map a value to a department (replacing any earlier mapping of the same value).
pub async fn upsert(db: &PgPool, ctx: &Ctx, value: &str, department_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let display = value.trim();
    let n = normalize(display);
    if n.is_empty() || display.chars().count() > 200 {
        return Err(CoreError::validation(
            "enter the Department value as it appears in Entra ID (1–200 characters)",
        ));
    }
    let mut tx = db.begin().await?;
    let dept = sqlx::query_scalar!("SELECT name FROM departments WHERE id = $1", department_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| CoreError::validation("choose a department"))?;
    let before = sqlx::query_scalar!(
        "SELECT d.name FROM department_entra_values v JOIN departments d ON d.id = v.department_id
          WHERE v.value_normalized = $1",
        n
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO department_entra_values (value_normalized, value_display, department_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (value_normalized)
         DO UPDATE SET value_display = EXCLUDED.value_display, department_id = EXCLUDED.department_id",
        n,
        display,
        department_id
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "directory.department_mapping_changed",
                ctx.actor.clone(),
                "department",
            )
            .object(department_id, Some(&dept))
            .department(Some(department_id))
            .change("department", before, dept.clone())
            .context("entra_value", display),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove(db: &PgPool, ctx: &Ctx, value: &str) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let n = normalize(value);
    let mut tx = db.begin().await?;
    let row = sqlx::query!(
        "DELETE FROM department_entra_values v USING departments d
          WHERE v.value_normalized = $1 AND d.id = v.department_id
          RETURNING v.value_display, v.department_id, d.name",
        n
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "directory.department_mapping_changed",
                ctx.actor.clone(),
                "department",
            )
            .object(row.department_id, Some(&row.name))
            .department(Some(row.department_id))
            .change("department", row.name.as_str(), AuditValue::Null)
            .context("entra_value", row.value_display),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Department values of active Entra people that map to nothing, most people first.
pub async fn unmapped(db: &PgPool) -> CoreResult<Vec<UnmappedValue>> {
    let mapped: BTreeSet<String> =
        sqlx::query_scalar!("SELECT value_normalized FROM department_entra_values")
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    let values = sqlx::query_scalar!(
        r#"SELECT coalesce(entra_department, '') AS "value!"
             FROM users WHERE source = 'entra' AND status = 'active'"#
    )
    .fetch_all(db)
    .await?;
    let mut counts: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for raw in values {
        let n = normalize(&raw);
        if mapped.contains(&n) {
            continue;
        }
        counts.entry(n).or_insert_with(|| (raw.trim().to_owned(), 0)).1 += 1;
    }
    let mut out: Vec<UnmappedValue> = counts
        .into_iter()
        .map(|(normalized, (value, users))| UnmappedValue {
            value,
            normalized,
            users,
        })
        .collect();
    out.sort_by(|a, b| b.users.cmp(&a.users).then_with(|| a.value.cmp(&b.value)));
    Ok(out)
}
```

- [ ] **Step 5: Run the tests.**

Run: `cargo test -p pdms-core --lib directory::entra_values && cargo test -p pdms-core --test entra_sync`
Expected: PASS (1 unit test, 1 integration test).

- [ ] **Step 6: Refresh offline query data and commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add migrations/0010_entra_sync.sql crates/pdms-core/src/model.rs crates/pdms-core/src/directory crates/pdms-core/tests/entra_sync.rs .sqlx
git commit -m "directory: Entra Department value mappings (migration 0010)"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Migration number.** The file is `migrations/0010_entra_sync.sql`, because R1's `0009_ui_redesign.sql` exists. In the same commit, update refinement #1 in `docs/superpowers/plans/2026-10-14-pdms-sharepoint-entra-index.md`: `0009_entra_sync.sql` becomes `0010_entra_sync.sql` (P2), P3 becomes `0011_sharepoint_files.sql`, and P4 becomes `0012_sharepoint_access.sql`. Add the index to `git add`.
- **Module order.** In `crates/pdms-core/src/directory/mod.rs`, put `pub mod entra_values;` between `mod admin;` and `pub mod sync;` (rustfmt's order).
- **No `model::UserSource`.** Do not add it: nothing reads it, and the queries use SQL literals. The SQL type `user_source` stays. In the same commit, remove `model::UserSource` from the index's "Additions" list.
- **DELETE grant.** The migration's `DELETE` grant on `department_entra_values` stays, because `remove()` needs it. Spec §6 listed only SELECT, INSERT and UPDATE. Record this in the index under "Changes recorded when P2 was written".
- **SPDX first** in `crates/pdms-core/tests/entra_sync.rs` (see Task 3's correction).


---

### Task 6: The sync itself (`run`, `run_full`, `sync_one`, status, requests)

**Files:**
- Create: `crates/pdms-core/src/directory/entra_sync.rs`
- Modify: `crates/pdms-core/src/directory/mod.rs` (add `pub mod entra_sync;`)
- Modify: `crates/pdms-core/src/auth/oidc.rs` (add `pub fn normalize_issuer` only; the rest is Task 8)
- Test: `crates/pdms-core/tests/entra_sync.rs` (append)

**Interfaces:**
- Consumes: `GraphClient::{get, get_delta}`, `GraphError::{NotFound, ResyncRequired}` (Task 3), `Core.graph` (Task 4), `entra_values::lookup` (Task 5), `directory::set_user_status`, `jobs::{enqueue, NewJob}`.
- Produces (index contract, plus additive items): `pub const SYNC_JOB: &str = "directory.entra_sync"`, `pub const TICK_JOB: &str = "directory.entra_sync_tick"`, `pub const LOCK_KEY: i64`, `pub struct SyncReport { pub created: u32, pub updated: u32, pub departed: u32, pub unmapped: u32, pub changed_departments: BTreeSet<Uuid>, pub errors: Vec<String> }` (`Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize`), `pub async fn run(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport>`, `pub async fn run_full(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport>`, `pub async fn sync_one(core: &Core, oid: &str) -> CoreResult<Option<Uuid>>`, `pub struct SyncStatus { pub last_run: Option<DateTime<Utc>>, pub last_error: Option<String>, pub last_summary: Option<SyncReport> }`, `pub async fn status(db: &PgPool) -> CoreResult<SyncStatus>`, `pub async fn request_sync(core: &Core, ctx: &Ctx, full: bool) -> CoreResult<()>`; `pdms_core::auth::oidc::normalize_issuer(&str) -> &str`.

- [ ] **Step 1: Write the failing tests.** Append to `crates/pdms-core/tests/entra_sync.rs` (add the imports at the top of the file):

```rust
use std::collections::BTreeSet;
use std::sync::OnceLock;

use mock_graph::*;
use pdms_core::Core;
use pdms_core::directory::entra_sync;
use pdms_core::graph::cert;
use serde_json::json;
use uuid::Uuid;

const ISSUER: &str = "https://login.microsoftonline.com/tenant-1/v2.0";
const OID_A: &str = "a0000000-0000-4000-8000-00000000000a";
const OID_B: &str = "b0000000-0000-4000-8000-00000000000b";
const OID_C: &str = "c0000000-0000-4000-8000-00000000000c";
const OID_D: &str = "d0000000-0000-4000-8000-00000000000d";
const OID_E: &str = "e0000000-0000-4000-8000-00000000000e";
const OID_G: &str = "90000000-0000-4000-8000-000000000009";

fn certificate() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| cert::generate("PDMS test", 30).unwrap().pem)
}

/// A core whose Graph is the mock, with the sync on.
async fn synced(m: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut cfg = (*core.config).clone();
    cfg.graph = m.config(certificate());
    cfg.entra_sync.enabled = true;
    cfg.auth.oidc.issuer_url = ISSUER.into();
    (Core::new(core.db.clone(), cfg).unwrap(), db)
}

async fn id_of(core: &Core, oid: &str) -> Option<Uuid> {
    sqlx::query_scalar("SELECT id FROM users WHERE oidc_subject = $1")
        .bind(oid)
        .fetch_optional(&core.db)
        .await
        .unwrap()
}

async fn idp_departments(core: &Core, user: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT department_id FROM department_members WHERE user_id = $1 AND source = 'idp' ORDER BY 1",
    )
    .bind(user)
    .fetch_all(&core.db)
    .await
    .unwrap()
}

async fn status_of(core: &Core, user: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

async fn saved_link(core: &Core) -> Option<String> {
    sqlx::query_scalar("SELECT delta_link FROM sync_cursors WHERE name = 'entra.users'")
        .fetch_optional(&core.db)
        .await
        .unwrap()
        .flatten()
}

#[tokio::test]
async fn first_sync_creates_members_maps_departments_and_skips_guests() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb).await.unwrap();
    m.add_user(member(OID_A, "Alice Road", "alice@county.example", Some("ROAD AND BRIDGE")));
    m.add_user(member(OID_B, "Bob Nowhere", "bob@county.example", Some("Mystery Dept")));
    m.add_user(member(OID_C, "Carl Blank", "carl@county.example", None));
    let mut guest = member(OID_G, "Gwen Guest", "gwen_partner.example#EXT#@county.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    let mut off = member(OID_D, "Dan Disabled", "dan@county.example", None);
    off["accountEnabled"] = json!(false);
    m.add_user(off);

    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.created, r.unmapped, r.departed), (3, 2, 0), "{r:?}");
    assert_eq!(r.changed_departments, BTreeSet::from([rb]));
    assert!(r.errors.is_empty(), "{:?}", r.errors);

    let a = id_of(&core, OID_A).await.unwrap();
    let (issuer, source): (String, String) =
        sqlx::query_as("SELECT oidc_issuer, source::text FROM users WHERE id = $1")
            .bind(a)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!((issuer.as_str(), source.as_str()), (ISSUER, "entra"));
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert!(id_of(&core, OID_G).await.is_none(), "guests are not synced");
    assert!(id_of(&core, OID_D).await.is_none(), "disabled accounts are not created");

    assert!(saved_link(&core).await.unwrap().ends_with("$deltatoken=1"));
    let acts = audit_actions(&core).await;
    for a in ["directory.user_created", "user.idp_memberships_synced", "directory.synced"] {
        assert!(acts.contains(&a.to_owned()), "missing {a}");
    }
    assert!(m.requests().iter().any(|r| r.starts_with("GET /v1.0/users/delta?$select=")
        && r.contains("department")
        && r.contains("accountEnabled")
        && r.contains("userType")));

    let un = entra_values::unmapped(&core.db).await.unwrap();
    let un: Vec<(&str, i64)> = un.iter().map(|u| (u.value.as_str(), u.users)).collect();
    assert_eq!(un, vec![("", 1), ("Mystery Dept", 1)]);
}

#[tokio::test]
async fn delta_moves_departments_and_departs_disabled_and_deleted_users() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge").await.unwrap();
    let ops = directory::create_department(&core.db, &system(), "OPS", "Operations").await.unwrap();
    let hr = directory::create_department(&core.db, &system(), "HR", "Human Resources").await.unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb).await.unwrap();
    entra_values::upsert(&core.db, &system(), "Operations", ops).await.unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", Some("Road & Bridge")));
    m.add_user(member(OID_B, "Bob", "bob@county.example", Some("Operations")));
    m.add_user(member(OID_C, "Carl", "carl@county.example", Some("Operations")));
    entra_sync::run(&core, &system()).await.unwrap();
    let (a, b, c) = (
        id_of(&core, OID_A).await.unwrap(),
        id_of(&core, OID_B).await.unwrap(),
        id_of(&core, OID_C).await.unwrap(),
    );
    // A manual membership is never touched by the sync.
    directory::add_department_member(&core.db, &system(), a, hr, false).await.unwrap();

    m.update_user(OID_A, json!({ "department": "Operations", "displayName": "Alice Ops" }));
    m.update_user(OID_B, json!({ "accountEnabled": false }));
    m.remove_user(OID_C, "changed");
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.departed, r.updated), (2, 1), "{r:?}");
    assert!(r.changed_departments.is_superset(&BTreeSet::from([rb, ops])));
    assert_eq!(idp_departments(&core, a).await, vec![ops]);
    let manual: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM department_members WHERE user_id = $1 AND department_id = $2 AND source = 'manual'",
    )
    .bind(a)
    .bind(hr)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(manual, 1);
    assert_eq!(status_of(&core, b).await, "departed");
    assert_eq!(status_of(&core, c).await, "departed");
    let name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(a)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(name, "Alice Ops");
    assert!(m.requests().iter().any(|r| r.contains("$deltatoken=1")), "used the saved link");

    // Replays and re-enabling change nothing: the sync never reactivates anyone.
    m.update_user(OID_A, json!({ "department": "Operations", "displayName": "Alice Ops" }));
    m.add_user(member(OID_B, "Bob", "bob@county.example", Some("Operations")));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!((r.created, r.updated, r.departed), (0, 0, 0), "{r:?}");
    assert_eq!(status_of(&core, b).await, "departed");
}

#[tokio::test]
async fn expired_delta_token_triggers_a_full_resync() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    m.state.lock().unwrap().expire_delta = true;
    m.add_user(member(OID_E, "Eve", "eve@county.example", None));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.created, 1);
    assert!(id_of(&core, OID_E).await.is_some());
    let full_rounds = m.requests().iter().filter(|r| r.starts_with("GET /v1.0/users/delta?$select=")).count();
    assert_eq!(full_rounds, 2);
}

#[tokio::test]
async fn a_failed_run_keeps_the_saved_cursor() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let before = saved_link(&core).await;
    m.fail_next(500, 0);
    assert!(entra_sync::run(&core, &system()).await.is_err());
    assert_eq!(saved_link(&core).await, before);
    let st = entra_sync::status(&core.db).await.unwrap();
    assert!(st.last_error.unwrap().contains("500"));
}

#[tokio::test]
async fn email_already_used_by_an_unlinked_account_is_reported_not_linked() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let local = directory::create_user(&core.db, &system(), "Break Glass", "alice@county.example", None)
        .await
        .unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.created, 0);
    assert_eq!(r.errors.len(), 1);
    assert!(r.errors[0].contains("alice@county.example"), "{:?}", r.errors);
    let subject: Option<String> = sqlx::query_scalar("SELECT oidc_subject FROM users WHERE id = $1")
        .bind(local)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(subject, None, "never linked by email");
    let st = entra_sync::status(&core.db).await.unwrap();
    assert_eq!(st.last_summary.unwrap().errors.len(), 1);
}

#[tokio::test]
async fn last_administrator_is_reported_not_departed() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run(&core, &system()).await.unwrap();
    let a = id_of(&core, OID_A).await.unwrap();
    let admin = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), a, admin, None).await.unwrap();
    m.update_user(OID_A, json!({ "accountEnabled": false }));
    let r = entra_sync::run(&core, &system()).await.unwrap();
    assert_eq!(r.departed, 0);
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert_eq!(status_of(&core, a).await, "active");
}

#[tokio::test]
async fn sync_one_reads_a_single_user() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge").await.unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb).await.unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", Some("Road & Bridge")));
    let mut guest = member(OID_G, "Gwen", "gwen@partner.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);
    let a = entra_sync::sync_one(&core, OID_A).await.unwrap().unwrap();
    assert_eq!(idp_departments(&core, a).await, vec![rb]);
    assert_eq!(entra_sync::sync_one(&core, OID_A).await.unwrap(), Some(a), "idempotent");
    assert_eq!(entra_sync::sync_one(&core, OID_G).await.unwrap(), None);
    assert_eq!(entra_sync::sync_one(&core, OID_E).await.unwrap(), None, "unknown in Entra");
    assert!(matches!(
        entra_sync::sync_one(&core, "../users").await,
        Err(CoreError::Validation(_))
    ));
}

#[tokio::test]
async fn runs_do_not_overlap_and_need_graph() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let mut tx = core.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(entra_sync::LOCK_KEY)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(matches!(entra_sync::run(&core, &system()).await, Err(CoreError::Conflict(_))));
    tx.rollback().await.unwrap();

    let (plain, _db2) = common::core().await;
    assert!(matches!(entra_sync::run(&plain, &system()).await, Err(CoreError::Validation(_))));
    assert!(matches!(
        entra_sync::request_sync(&plain, &system(), false).await,
        Err(CoreError::Validation(_))
    ));
    entra_sync::request_sync(&core, &system(), true).await.unwrap();
    let full: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE kind = 'directory.entra_sync'")
            .fetch_all(&core.db)
            .await
            .unwrap();
    assert_eq!(full, vec![json!({ "full": true })]);
    assert!(audit_actions(&core).await.contains(&"directory.sync_requested".to_owned()));
}
```

- [ ] **Step 2: Run them to see them fail.**

Run: `cargo test -p pdms-core --test entra_sync`
Expected: FAIL to compile — `could not find entra_sync in directory`.

- [ ] **Step 3: Add `normalize_issuer`.** In `crates/pdms-core/src/auth/oidc.rs`, after `pub fn safe_return_to`:

```rust
/// Issuer URLs are compared and stored without surrounding spaces or a trailing `/`.
pub fn normalize_issuer(issuer: &str) -> &str {
    issuer.trim().trim_end_matches('/')
}
```

- [ ] **Step 4: Implement the sync.** Create `crates/pdms-core/src/directory/entra_sync.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! People and departments from Microsoft Entra ID (spec §7.5, D-086).
//!
//! `/users/delta` gives every member account once, then only changes. Each
//! account is applied idempotently (delta can replay changes):
//! - enabled members are created or updated; their Department value picks one
//!   `idp` department membership (manual memberships are never touched);
//! - disabled or removed accounts become `departed` (sessions revoked) through
//!   `set_user_status`, which also refuses to remove the last administrator;
//! - guests are skipped unless `include_guests`; nobody is ever reactivated.
//!
//! Accounts are matched by issuer and Entra object ID only, never by email.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::Capability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::entra_values;
use crate::Core;
use crate::auth::oidc::normalize_issuer;
use crate::config::Config;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::graph::{DeltaPage, GraphClient, GraphError};
use crate::jobs::{self, NewJob};
use crate::model::UserStatus;

pub const SYNC_JOB: &str = "directory.entra_sync";
/// Runs every 15 minutes; enqueues the daily sync once `run_at` has passed.
pub const TICK_JOB: &str = "directory.entra_sync_tick";
/// Postgres advisory lock: one sync at a time ("PDMSENTR").
pub const LOCK_KEY: i64 = 0x5044_4d53_454e_5452;
const CURSOR: &str = "entra.users";
const BASE_SELECT: &str = "id,displayName,mail,userPrincipalName,accountEnabled,userType";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub created: u32,
    pub updated: u32,
    pub departed: u32,
    /// People processed this run whose Department value maps to nothing.
    pub unmapped: u32,
    /// Departments that gained or lost an Entra-managed member.
    pub changed_departments: BTreeSet<Uuid>,
    /// Accounts that need a person's attention (not linked, last admin, ...).
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SyncStatus {
    pub last_run: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_summary: Option<SyncReport>,
}

/// One entry of `/users/delta` or `/users/{id}`.
struct EntraUser {
    id: String,
    removed: bool,
    enabled: bool,
    guest: bool,
    name: Option<String>,
    email: Option<String>,
    department: Option<String>,
}

fn select_list(attribute: &str) -> String {
    if BASE_SELECT.split(',').any(|p| p == attribute) {
        BASE_SELECT.to_owned()
    } else {
        format!("{BASE_SELECT},{attribute}")
    }
}

/// Synced people sign in with OIDC; their identity uses the configured issuer.
fn entra_issuer(config: &Config) -> CoreResult<String> {
    let issuer = normalize_issuer(&config.auth.oidc.issuer_url);
    if issuer.is_empty() {
        return Err(CoreError::validation(
            "auth.oidc.issuer_url must be set so synced people can sign in",
        ));
    }
    Ok(issuer.to_owned())
}

fn graph(core: &Core) -> CoreResult<&GraphClient> {
    core.graph.as_ref().ok_or_else(|| {
        CoreError::validation(
            "Microsoft Graph is not configured ([graph] tenant_id, client_id, certificate)",
        )
    })
}

fn valid_email(e: &str) -> bool {
    e.len() <= 320
        && e.matches('@').count() == 1
        && !e.contains(char::is_whitespace)
        && e.split_once('@').is_some_and(|(l, d)| !l.is_empty() && !d.is_empty())
}

fn parse_user(v: &Value, attribute: &str) -> Option<EntraUser> {
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some(EntraUser {
        id: v.get("id")?.as_str()?.to_owned(),
        removed: v.get("@removed").is_some(),
        enabled: v.get("accountEnabled").and_then(Value::as_bool).unwrap_or(true),
        guest: v
            .get("userType")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("guest")),
        name: text("displayName").map(|n| n.chars().take(200).collect()),
        email: text("mail")
            .filter(|e| valid_email(e))
            .or_else(|| text("userPrincipalName").filter(|e| valid_email(e))),
        department: text(attribute),
    })
}

/// Incremental sync (falls back to a full one when Graph asks for it).
pub async fn run(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport> {
    run_inner(core, ctx, false).await
}

/// Read every account again, ignoring the saved cursor.
pub async fn run_full(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport> {
    run_inner(core, ctx, true).await
}

async fn run_inner(core: &Core, ctx: &Ctx, full: bool) -> CoreResult<SyncReport> {
    ctx.require(Capability::ManageDirectory)?;
    let graph = graph(core)?;
    let issuer = entra_issuer(&core.config)?;
    let mut lock = core.db.begin().await?;
    let got = sqlx::query_scalar!(r#"SELECT pg_try_advisory_xact_lock($1) AS "got!""#, LOCK_KEY)
        .fetch_one(&mut *lock)
        .await?;
    if !got {
        return Err(CoreError::Conflict("a directory sync is already running".into()));
    }
    let result = sync_all(core, ctx, graph, &issuer, full).await;
    if let Err(e) = &result {
        record_failure(&core.db, &e.to_string()).await?;
    }
    lock.rollback().await?;
    result
}

async fn sync_all(
    core: &Core,
    ctx: &Ctx,
    graph: &GraphClient,
    issuer: &str,
    full: bool,
) -> CoreResult<SyncReport> {
    let attribute = core.config.entra_sync.attribute.as_str();
    let initial = format!("/users/delta?$select={}", select_list(attribute));
    let saved = if full {
        None
    } else {
        sqlx::query_scalar!("SELECT delta_link FROM sync_cursors WHERE name = $1", CURSOR)
            .fetch_optional(&core.db)
            .await?
            .flatten()
    };
    let page: DeltaPage<Value> = match saved {
        Some(link) => match graph.get_delta(&link).await {
            Err(GraphError::ResyncRequired) => {
                tracing::warn!("Entra delta token no longer valid; reading every account again");
                graph.get_delta(&initial).await?
            }
            other => other?,
        },
        None => graph.get_delta(&initial).await?,
    };

    let mut report = SyncReport::default();
    for v in &page.items {
        match parse_user(v, attribute) {
            Some(u) => {
                apply(core, ctx, issuer, &u, &mut report).await?;
            }
            None => report.errors.push("an entry without an id was skipped".into()),
        }
    }

    let summary = serde_json::to_value(&report).map_err(CoreError::internal)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO sync_cursors (name, delta_link, last_run, last_error, last_summary)
         VALUES ($1, $2, now(), $3, $4)
         ON CONFLICT (name) DO UPDATE SET delta_link = EXCLUDED.delta_link, last_run = EXCLUDED.last_run,
                last_error = EXCLUDED.last_error, last_summary = EXCLUDED.last_summary",
        CURSOR,
        page.delta_link,
        report.errors.first().map(String::as_str),
        summary
    )
    .execute(&mut *tx)
    .await?;
    let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(Category::Admin, "directory.synced", ctx.actor.clone(), "directory")
                .context("full", full)
                .context("created", report.created)
                .context("updated", report.updated)
                .context("departed", report.departed)
                .context("unmapped", report.unmapped)
                .context("departments_changed", n(report.changed_departments.len()))
                .context("problems", n(report.errors.len())),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(report)
}

async fn record_failure(db: &PgPool, error: &str) -> CoreResult<()> {
    let error: String = error.chars().take(500).collect();
    sqlx::query!(
        "INSERT INTO sync_cursors (name, last_run, last_error) VALUES ($1, now(), $2)
         ON CONFLICT (name) DO UPDATE SET last_run = now(), last_error = EXCLUDED.last_error",
        CURSOR,
        error
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Look one account up in Entra and apply it (first sign-in). `None`: not an
/// enabled member, unknown to Entra, or blocked (see the log).
pub async fn sync_one(core: &Core, oid: &str) -> CoreResult<Option<Uuid>> {
    let oid = Uuid::parse_str(oid)
        .map_err(|_| CoreError::validation("not an Entra object ID"))?
        .hyphenated()
        .to_string();
    let graph = graph(core)?;
    let issuer = entra_issuer(&core.config)?;
    let attribute = core.config.entra_sync.attribute.as_str();
    let path = format!("/users/{oid}?$select={}", select_list(attribute));
    let v: Value = match graph.get(&path).await {
        Ok(v) => v,
        Err(GraphError::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let user = parse_user(&v, attribute)
        .ok_or_else(|| CoreError::internal("Graph returned a user without an id"))?;
    let mut report = SyncReport::default();
    let id = apply(core, &Ctx::system("entra-sync"), &issuer, &user, &mut report).await?;
    for e in &report.errors {
        tracing::warn!(oid = %oid, problem = %e, "Entra sign-in lookup");
    }
    Ok(id)
}

/// Apply one account. Returns the PDMS user ID when the person is an active member.
async fn apply(
    core: &Core,
    ctx: &Ctx,
    issuer: &str,
    u: &EntraUser,
    report: &mut SyncReport,
) -> CoreResult<Option<Uuid>> {
    let existing = sqlx::query!(
        r#"SELECT id, display_name, email, status AS "status: UserStatus"
             FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2"#,
        issuer,
        u.id
    )
    .fetch_optional(&core.db)
    .await?;

    let leaving = u.removed || !u.enabled || (u.guest && !core.config.entra_sync.include_guests);
    if leaving {
        if let Some(e) = existing.as_ref()
            && e.status == UserStatus::Active
        {
            match super::set_user_status(&core.db, ctx, e.id, UserStatus::Departed).await {
                Ok(()) => report.departed += 1,
                Err(CoreError::Conflict(why)) => report.errors.push(format!(
                    "{} <{}> is disabled in Entra but was not deactivated: {why}",
                    e.display_name, e.email
                )),
                Err(err) => return Err(err),
            }
        }
        return Ok(None);
    }

    let want = entra_values::lookup(&core.db, u.department.as_deref()).await?;
    if want.is_none() {
        report.unmapped += 1;
    }
    let mut events = Vec::new();
    let mut tx = core.db.begin().await?;
    let (id, name, created, renamed) = match existing {
        Some(e) if e.status != UserStatus::Active => {
            // Never reactivated automatically: an administrator decides.
            sqlx::query!(
                "UPDATE users SET entra_department = $2, entra_synced_at = now() WHERE id = $1",
                e.id,
                u.department
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(None);
        }
        Some(e) => {
            let name = u.name.clone().unwrap_or_else(|| e.display_name.clone());
            let email = u.email.clone().unwrap_or_else(|| e.email.clone());
            let res = sqlx::query!(
                "UPDATE users SET display_name = $2, email = $3, entra_department = $4,
                        entra_synced_at = now(), source = 'entra', updated_at = now()
                  WHERE id = $1",
                e.id,
                name,
                email,
                u.department
            )
            .execute(&mut *tx)
            .await;
            if let Err(err) = res {
                return email_conflict(err, report, &email);
            }
            let ev = AuditEvent::new(Category::Admin, "user.updated", ctx.actor.clone(), "user")
                .object(e.id, Some(&name))
                .change("display_name", e.display_name.as_str(), name.as_str())
                .change("email", e.email.as_str(), email.as_str());
            let renamed = ev.changes.is_some();
            if renamed {
                events.push(ev);
            }
            (e.id, name, false, renamed)
        }
        None => {
            let Some(email) = u.email.clone() else {
                report.errors.push(format!(
                    "Entra account {} has no usable email or user principal name",
                    u.id
                ));
                return Ok(None);
            };
            let name = u.name.clone().unwrap_or_else(|| email.clone());
            let res = sqlx::query_scalar!(
                "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject, source,
                                    entra_department, entra_synced_at)
                 VALUES ($1, $2, $3, $4, $5, 'entra', $6, now())
                 ON CONFLICT ON CONSTRAINT users_oidc_identity_key DO NOTHING
                 RETURNING id",
                Uuid::now_v7(),
                name,
                email,
                issuer,
                u.id,
                u.department
            )
            .fetch_optional(&mut *tx)
            .await;
            let id = match res {
                // Created concurrently (sign-in during a sync): the other writer won.
                Ok(None) => return Ok(None),
                Ok(Some(id)) => id,
                Err(err) => return email_conflict(err, report, &email),
            };
            events.push(
                AuditEvent::new(Category::Admin, "directory.user_created", ctx.actor.clone(), "user")
                    .object(id, Some(&name))
                    .change("display_name", AuditValue::Null, name.as_str())
                    .change("email", AuditValue::Null, email.as_str())
                    .change("oidc_subject", AuditValue::Null, u.id.as_str())
                    .context("entra_department", u.department.clone()),
            );
            (id, name, true, false)
        }
    };

    let moved = set_idp_department(&mut tx, id, want).await?;
    if let Some((before, after)) = &moved {
        report
            .changed_departments
            .extend(before.iter().chain(after.iter()).copied());
        events.push(
            AuditEvent::new(
                Category::Permission,
                "user.idp_memberships_synced",
                ctx.actor.clone(),
                "user",
            )
            .object(id, Some(&name))
            .change(
                "departments",
                department_names(&mut tx, before).await?,
                department_names(&mut tx, after).await?,
            )
            .context("entra_department", u.department.clone()),
        );
    }
    if created {
        report.created += 1;
    } else if renamed || moved.is_some() {
        report.updated += 1;
    }
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    Ok(Some(id))
}

/// An email already belongs to another account: report it, change nothing.
fn email_conflict(
    err: sqlx::Error,
    report: &mut SyncReport,
    email: &str,
) -> CoreResult<Option<Uuid>> {
    match CoreError::conflict_on_unique(err, "") {
        CoreError::Conflict(_) => {
            report.errors.push(format!(
                "{email} is already used by a PDMS account that is not this Entra account; \
                 rename or remove that account, then run a full sync"
            ));
            Ok(None)
        }
        other => Err(other),
    }
}

/// Make `want` the person's only `idp` department. Returns (before, after) when it changed.
async fn set_idp_department(
    conn: &mut PgConnection,
    user_id: Uuid,
    want: Option<Uuid>,
) -> CoreResult<Option<(BTreeSet<Uuid>, BTreeSet<Uuid>)>> {
    let have: BTreeSet<Uuid> = sqlx::query_scalar!(
        "SELECT department_id FROM department_members WHERE user_id = $1 AND source = 'idp'",
        user_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let want: BTreeSet<Uuid> = want.into_iter().collect();
    if have == want {
        return Ok(None);
    }
    for d in have.difference(&want) {
        sqlx::query!(
            "DELETE FROM department_members WHERE user_id = $1 AND department_id = $2 AND source = 'idp'",
            user_id,
            d
        )
        .execute(&mut *conn)
        .await?;
    }
    for d in want.difference(&have) {
        sqlx::query!(
            "INSERT INTO department_members (department_id, user_id, is_primary, source)
             VALUES ($1, $2, NOT EXISTS (SELECT 1 FROM department_members WHERE user_id = $2 AND is_primary), 'idp')
             ON CONFLICT (department_id, user_id) DO NOTHING",
            d,
            user_id
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(Some((have, want)))
}

async fn department_names(conn: &mut PgConnection, ids: &BTreeSet<Uuid>) -> CoreResult<Vec<String>> {
    let ids: Vec<Uuid> = ids.iter().copied().collect();
    Ok(sqlx::query_scalar!(
        "SELECT name FROM departments WHERE id = ANY($1) ORDER BY name",
        &ids[..]
    )
    .fetch_all(conn)
    .await?)
}

/// Last run, last problem, and the last report (for the admin page).
pub async fn status(db: &PgPool) -> CoreResult<SyncStatus> {
    let row = sqlx::query!(
        "SELECT last_run, last_error, last_summary FROM sync_cursors WHERE name = $1",
        CURSOR
    )
    .fetch_optional(db)
    .await?;
    Ok(match row {
        None => SyncStatus::default(),
        Some(r) => SyncStatus {
            last_run: r.last_run,
            last_error: r.last_error,
            last_summary: r.last_summary.and_then(|v| serde_json::from_value(v).ok()),
        },
    })
}

/// "Sync now" / "Full resync" from the admin page: queue a run for the worker.
pub async fn request_sync(core: &Core, ctx: &Ctx, full: bool) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    graph(core)?;
    if !core.config.entra_sync.enabled {
        return Err(CoreError::validation(
            "Entra sync is turned off ([entra_sync] enabled = false)",
        ));
    }
    let mut tx = core.db.begin().await?;
    jobs::enqueue(
        &mut *tx,
        NewJob::new(SYNC_JOB, json!({ "full": full }))
            .idempotency_key(format!("entra_sync:manual:{}", Uuid::new_v4()))
            .max_attempts(3),
    )
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "directory.sync_requested",
                ctx.actor.clone(),
                "directory",
            )
            .context("full", full),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
```

Add `pub mod entra_sync;` to `crates/pdms-core/src/directory/mod.rs`.

- [ ] **Step 5: Run the tests.**

Run: `cargo test -p pdms-core --test entra_sync`
Expected: PASS — 9 tests.

- [ ] **Step 6: Refresh offline query data and commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/directory crates/pdms-core/src/auth/oidc.rs crates/pdms-core/tests/entra_sync.rs .sqlx
git commit -m "directory: Entra user sync with delta, departures, and Department mapping"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Manual memberships (idempotency).**
  - An existing `manual` membership in the department a person's Entra value maps to satisfies the mapping.
  - The sync then adds no `idp` row; any other `idp` rows are still removed.
  - When nothing else changed, the run counts no update, writes no `user.idp_memberships_synced`, and leaves the department out of `changed_departments`. P4 reconciles on that set.
  - Test it: give a person a manual membership in the department their value maps to, run the sync twice, and assert that the second run reports `updated == 0` and adds no audit rows.
- **Counts and errors.**
  - `unmapped` counts only accounts the run actually applied, not ones skipped because of an email conflict or because they are departed.
  - `sync_cursors.last_error` is written only when a run **fails**. A successful run clears it, even when individual accounts had problems; those stay in `last_summary.errors`.
- **Audit the source change.** When the update branch changes `users.source` from `local` to `entra`, `user.updated` includes `change("source", "local", "entra")`.
- **Guests.** Skip only `userType == "Guest"`; a missing or null `userType` counts as a member, because Graph returns null for some older member accounts. Record this in the index under "Changes recorded when P2 was written": "guests are skipped by `userType = Guest`; a null `userType` counts as a member (spec §7.5 said `ne 'Member'`)".
- **Module order.** `pub mod entra_sync;` goes next to `pub mod entra_values;` in `directory/mod.rs`, in rustfmt's order.


---

### Task 7: Daily schedule, worker jobs, and `pdms sync-directory`

**Files:**
- Modify: `crates/pdms-core/src/directory/entra_sync.rs` (append `schedule_daily`, `run_job`)
- Modify: `crates/pdms-core/src/maintenance.rs` (handlers + 15-minute tick)
- Modify: `crates/pdms/src/main.rs` (`SyncDirectory` command)
- Test: `crates/pdms-core/tests/entra_sync.rs` (append)

**Interfaces:**
- Consumes: `run`, `run_full`, `SYNC_JOB`, `TICK_JOB` (Task 6).
- Produces: `pub async fn schedule_daily(core: &Core, now: DateTime<Utc>) -> CoreResult<bool>` (true when it queued today's run), `pub async fn run_job(core: &Core, payload: &Value) -> Result<(), String>` (the worker handler body; **P4 adds** "enqueue `sharepoint.reconcile_access` after a successful run" here); worker handlers for `SYNC_JOB` and `TICK_JOB`; CLI `pdms sync-directory [--full]`.

- [ ] **Step 1: Write the failing tests.** Append to `crates/pdms-core/tests/entra_sync.rs`:

```rust
#[tokio::test]
async fn the_daily_run_is_queued_once_after_run_at_local_time() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let mut cfg = (*core.config).clone();
    cfg.org.timezone = "America/Chicago".into();
    cfg.entra_sync.run_at = "02:30".into();
    let core = Core::new(core.db.clone(), cfg).unwrap();
    let at = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().unwrap();
    // 2026-10-15 07:00Z is 02:00 CDT: too early.
    assert!(!entra_sync::schedule_daily(&core, at("2026-10-15T07:00:00Z")).await.unwrap());
    // 08:00Z is 03:00 CDT: queued, once.
    assert!(entra_sync::schedule_daily(&core, at("2026-10-15T08:00:00Z")).await.unwrap());
    assert!(!entra_sync::schedule_daily(&core, at("2026-10-15T20:00:00Z")).await.unwrap());
    // The next local day queues again.
    assert!(entra_sync::schedule_daily(&core, at("2026-10-16T08:00:00Z")).await.unwrap());
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM jobs WHERE kind = 'directory.entra_sync' ORDER BY 1",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(keys, vec!["entra_sync:daily:2026-10-15", "entra_sync:daily:2026-10-16"]);

    let (off, _db2) = common::core().await;
    assert!(!entra_sync::schedule_daily(&off, at("2026-10-15T08:00:00Z")).await.unwrap());
}

#[tokio::test]
async fn the_job_runs_incremental_or_full() {
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    m.add_user(member(OID_A, "Alice", "alice@county.example", None));
    entra_sync::run_job(&core, &json!({ "full": false })).await.unwrap();
    entra_sync::run_job(&core, &json!({ "full": true })).await.unwrap();
    let full_rounds = m.requests().iter().filter(|r| r.starts_with("GET /v1.0/users/delta?$select=")).count();
    assert_eq!(full_rounds, 2, "the full job ignored the saved cursor");
}
```

- [ ] **Step 2: Run them to see them fail.**

Run: `cargo test -p pdms-core --test entra_sync daily job`
Expected: FAIL to compile — `cannot find function schedule_daily`.

- [ ] **Step 3: Implement.** Append to `crates/pdms-core/src/directory/entra_sync.rs`:

```rust
/// Queue today's run once the local time is past `[entra_sync] run_at`.
/// The idempotency key makes it once per local date, across workers.
pub async fn schedule_daily(core: &Core, now: DateTime<Utc>) -> CoreResult<bool> {
    let cfg = &core.config.entra_sync;
    let Some(at) = cfg.run_at_time() else {
        return Ok(false);
    };
    if !cfg.enabled {
        return Ok(false);
    }
    let local = now.with_timezone(&core.config.timezone());
    if local.time() < at {
        return Ok(false);
    }
    Ok(jobs::enqueue(
        &core.db,
        NewJob::new(SYNC_JOB, json!({ "full": false }))
            .idempotency_key(format!("entra_sync:daily:{}", local.date_naive()))
            .max_attempts(3),
    )
    .await?
    .is_some())
}

/// Worker handler for [`SYNC_JOB`]. A run already in progress is not an error.
pub async fn run_job(core: &Core, payload: &Value) -> Result<(), String> {
    let full = payload.get("full").and_then(Value::as_bool).unwrap_or(false);
    let ctx = Ctx::system("entra-sync");
    let result = if full {
        run_full(core, &ctx).await
    } else {
        run(core, &ctx).await
    };
    match result {
        Ok(r) => {
            tracing::info!(
                created = r.created,
                updated = r.updated,
                departed = r.departed,
                unmapped = r.unmapped,
                problems = r.errors.len(),
                "Entra directory synchronized"
            );
            Ok(())
        }
        Err(CoreError::Conflict(m)) => {
            tracing::info!("{m}; this run was skipped");
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}
```

In `crates/pdms-core/src/maintenance.rs`, change the end of `worker()` so the existing chain is bound to a variable, then add the Entra handlers. The final lines become:

```rust
        .every(API_LOG_PURGE, Duration::from_secs(24 * 3600));
    use crate::directory::entra_sync;
    let worker = worker
        .handle(entra_sync::SYNC_JOB, {
            let core = core.clone();
            move |job| {
                let core = core.clone();
                async move { entra_sync::run_job(&core, &job.payload).await }
            }
        })
        .handle(entra_sync::TICK_JOB, {
            let core = core.clone();
            move |_| {
                let core = core.clone();
                async move {
                    entra_sync::schedule_daily(&core, chrono::Utc::now())
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
            }
        });
    if core.config.entra_sync.enabled {
        worker.every(entra_sync::TICK_JOB, Duration::from_secs(15 * 60))
    } else {
        worker
    }
}
```

(The `worker` chain that previously ended the function now starts with `let worker = worker` — keep every existing `.handle`/`.every` call unchanged; only the binding and the tail change.)

In `crates/pdms/src/main.rs`, add to `enum Command` after `RunScheduler,`:

```rust
    /// Read people and departments from Entra ID now (the worker does this
    /// nightly when [entra_sync] is enabled). --full ignores the saved cursor.
    SyncDirectory {
        #[arg(long)]
        full: bool,
    },
```

and in `run` after the `Command::RunScheduler` arm:

```rust
        Command::SyncDirectory { full } => {
            use pdms_core::directory::entra_sync;
            let core = connect(config).await?;
            let ctx = Ctx::system("cli:sync-directory");
            let r = if full {
                entra_sync::run_full(&core, &ctx).await?
            } else {
                entra_sync::run(&core, &ctx).await?
            };
            println!(
                "Created {}, updated {}, departed {}; {} without a mapped department.",
                r.created, r.updated, r.departed, r.unmapped
            );
            for e in &r.errors {
                println!("  needs attention: {e}");
            }
        }
```

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p pdms-core --test entra_sync`
Expected: PASS — 11 tests. Also run `cargo run -q -p pdms -- sync-directory --help` → shows `--full`.

- [ ] **Step 5: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/directory/entra_sync.rs crates/pdms-core/src/maintenance.rs crates/pdms/src/main.rs crates/pdms-core/tests/entra_sync.rs .sqlx
git commit -m "worker: nightly Entra sync; pdms sync-directory"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Test command.** Step 2's command is `cargo test -p pdms-core --test entra_sync -- daily job`. Cargo rejects a second filter that has no `--` before it.


---

### Task 8: SSO matches by object ID only

**Files:**
- Modify: `crates/pdms-core/src/auth/oidc.rs` (`login` around lines 160–230; replace `resolve_user` at lines ~294–390)
- Test: `crates/pdms-core/tests/entra_sync.rs` (append), `crates/pdms-web/tests/oidc.rs` (append)

**Interfaces:**
- Consumes: `entra_sync::sync_one`, `normalize_issuer` (Task 6).
- Produces: `pub async fn resolve_identity(core: &Core, issuer: &str, subject: &str, email: Option<&str>, name: Option<&str>) -> CoreResult<Result<(Uuid, String), &'static str>>` in `pdms_core::auth::oidc` (deny reasons: `inactive`, `not_provisioned`, `no_email_claim`, `email_in_use`, `directory_unavailable`). `OidcProvider::resolve_user` is removed.

- [ ] **Step 1: Write the failing tests.** Append to `crates/pdms-core/tests/entra_sync.rs`:

```rust
#[tokio::test]
async fn first_sign_in_asks_entra_when_the_sync_is_on() {
    use pdms_core::auth::oidc::resolve_identity;
    let m = MockGraph::start().await;
    let (core, _db) = synced(&m).await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge").await.unwrap();
    entra_values::upsert(&core.db, &system(), "Road & Bridge", rb).await.unwrap();
    m.add_user(member(OID_A, "Alice", "alice@county.example", Some("Road & Bridge")));
    let mut guest = member(OID_G, "Gwen", "gwen@partner.example", None);
    guest["userType"] = json!("Guest");
    m.add_user(guest);

    let (id, name) = resolve_identity(&core, &format!("{ISSUER}/"), OID_A, Some("x@x.org"), Some("Token Name"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(name, "Alice", "Entra, not the token, names the person");
    assert_eq!(idp_departments(&core, id).await, vec![rb], "department is set before the first page");
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_A, None, None).await.unwrap(),
        Ok((id, "Alice".to_owned()))
    );
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_G, Some("gwen@partner.example"), None).await.unwrap(),
        Err("not_provisioned")
    );
    m.fail_next(500, 0);
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_E, Some("e@county.example"), None).await.unwrap(),
        Err("directory_unavailable")
    );
}

#[tokio::test]
async fn first_sign_in_without_the_sync_never_links_by_email() {
    use pdms_core::auth::oidc::resolve_identity;
    let (core, _db) = core().await;
    let local = directory::create_user(&core.db, &system(), "Break Glass", "admin@county.example", None)
        .await
        .unwrap();
    assert_eq!(
        resolve_identity(&core, ISSUER, OID_A, Some("Admin@County.example"), Some("Mallory")).await.unwrap(),
        Err("email_in_use")
    );
    let subject: Option<String> = sqlx::query_scalar("SELECT oidc_subject FROM users WHERE id = $1")
        .bind(local)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(subject, None);
    let (id, _) = resolve_identity(&core, ISSUER, OID_B, Some("new@county.example"), Some("New Person"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id_of(&core, OID_B).await, Some(id));
}
```

Append to `crates/pdms-web/tests/oidc.rs`:

```rust
#[tokio::test]
async fn existing_account_with_the_same_email_is_never_linked() {
    let app = app().await;
    let local = directory::create_user(&app.core.db, &system(), "Break Glass", "sso.user@org.example", None)
        .await
        .unwrap();
    let (_idp, issuer) = mock_idp(serde_json::json!({
        "sub": "pairwise-sub-2", "oid": "entra-oid-2", "aud": [CLIENT_ID],
        "email": "sso.user@org.example", "name": "Sso User"
    }))
    .await;
    let base = serve(app.core.clone(), &issuer).await;
    let r = browser().get(format!("{base}/auth/oidc/start")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let subject: Option<String> = sqlx::query_scalar("SELECT oidc_subject FROM users WHERE id = $1")
        .bind(local)
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(subject, None, "not linked by email");
    let contexts: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT context FROM audit_log WHERE action = 'auth.login_failed'")
            .fetch_all(&app.core.db)
            .await
            .unwrap();
    assert!(contexts.iter().any(|c| c["reason"] == "email_in_use"), "{contexts:?}");
}

#[tokio::test]
async fn with_entra_sync_on_token_groups_do_not_change_departments() {
    let mut cfg = config();
    cfg.entra_sync.enabled = true; // Graph is not configured: sign-in provisions as before
    let app = app_with(cfg).await;
    let ops = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    directory::create_idp_mapping(&app.core.db, &system(), "grp-ops", "", MappingTarget::Department(ops))
        .await
        .unwrap();
    let (_idp, issuer) = mock_idp(serde_json::json!({
        "sub": "pairwise-sub-3", "oid": "entra-oid-3", "aud": [CLIENT_ID],
        "email": "g@org.example", "name": "G", "groups": ["grp-ops"]
    }))
    .await;
    let base = serve(app.core.clone(), &issuer).await;
    let r = browser().get(format!("{base}/auth/oidc/start")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let members: i64 = sqlx::query_scalar("SELECT count(*) FROM department_members WHERE department_id = $1")
        .bind(ops)
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(members, 0, "departments come from the Entra Department field");
    assert!(!actions(&app.core).await.contains(&"user.idp_memberships_synced".to_owned()));
}
```

- [ ] **Step 2: Run them to see them fail.**

Run: `cargo test -p pdms-core --test entra_sync first_sign_in`
Expected: FAIL to compile — `cannot find function resolve_identity in module oidc`.

- [ ] **Step 3: Implement.** In `crates/pdms-core/src/auth/oidc.rs`:

1. Delete the whole `async fn resolve_user(...)` method (the "Find, link, or provision the local user" method, ending before `}` of `impl OidcProvider`).
2. In `login`, replace `let user = match self.resolve_user(core, &claims).await? {` with:

```rust
        let user = match resolve_identity(
            core,
            &claims.issuer,
            &claims.subject,
            claims.email.as_deref(),
            claims.name.as_deref(),
        )
        .await?
        {
```

3. In `login`, delete the two lines `let ctx = crate::context::Ctx::system("idp-sync");` and `let _ = &ctx;`, and replace `if claims.groups_overage {` with:

```rust
        if core.config.entra_sync.enabled {
            // Departments come from the Entra Department field through the
            // directory sync (D-086), never from token group claims.
        } else if claims.groups_overage {
```

4. Add these free functions after `fn decode_jwt_payload`:

```rust
/// Find or provision the account for a verified sign-in. `Ok(Err(reason))`
/// means "deny". Accounts are matched by issuer and subject (Entra object ID)
/// only, never by email (D-086).
pub async fn resolve_identity(
    core: &Core,
    issuer: &str,
    subject: &str,
    email: Option<&str>,
    name: Option<&str>,
) -> CoreResult<Result<(Uuid, String), &'static str>> {
    let issuer = normalize_issuer(issuer);
    if let Some(found) = find_identity(core, issuer, subject).await? {
        return Ok(found);
    }
    if core.config.entra_sync.enabled && core.graph.is_some() {
        // Entra decides: enabled member or not, and the department.
        return Ok(match crate::directory::entra_sync::sync_one(core, subject).await {
            Ok(Some(_)) => find_identity(core, issuer, subject)
                .await?
                .unwrap_or(Err("not_provisioned")),
            Ok(None) => Err("not_provisioned"),
            Err(e) => {
                tracing::error!(error = %e, "Entra lookup at first sign-in failed");
                Err("directory_unavailable")
            }
        });
    }
    if !core.config.auth.oidc.auto_provision {
        return Ok(Err("not_provisioned"));
    }
    let Some(email) = email else {
        return Ok(Err("no_email_claim"));
    };
    let display: String = name.unwrap_or(email).chars().take(200).collect();
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    let inserted = sqlx::query!(
        "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        display,
        email,
        issuer,
        subject,
    )
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        return match CoreError::conflict_on_unique(e, "") {
            CoreError::Conflict(_) => Ok(Err("email_in_use")),
            other => Err(other),
        };
    }
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "user.provisioned",
                Actor::system("oidc-login"),
                "user",
            )
            .object(id, Some(&display))
            .change("email", None::<String>, email.to_owned())
            .change("oidc_subject", None::<String>, subject.to_owned())
            .context("source", "oidc_first_login"),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(Ok((id, display)))
}

async fn find_identity(
    core: &Core,
    issuer: &str,
    subject: &str,
) -> CoreResult<Option<Result<(Uuid, String), &'static str>>> {
    let u = sqlx::query!(
        r#"SELECT id, display_name, status AS "status: UserStatus"
             FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2"#,
        issuer,
        subject,
    )
    .fetch_optional(&core.db)
    .await?;
    Ok(u.map(|u| {
        if u.status == UserStatus::Active {
            Ok((u.id, u.display_name))
        } else {
            Err("inactive")
        }
    }))
}
```

Run `cargo clippy -p pdms-core --all-targets -- -D warnings`; remove any import the deleted method left unused.

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p pdms-core --test entra_sync && cargo test -p pdms-web --test oidc`
Expected: PASS — 13 core tests; 5 web OIDC tests (the 3 existing ones still pass: first login still provisions with `user.provisioned`; the groups test still syncs groups because `entra_sync` is off there).

- [ ] **Step 5: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/auth/oidc.rs crates/pdms-core/tests/entra_sync.rs crates/pdms-web/tests/oidc.rs .sqlx
git commit -m "auth: match SSO by object ID only; ask Entra on first sign-in when syncing"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Where the code is.** The function is `OidcProvider::complete` (`crates/pdms-core/src/auth/oidc.rs:121–239`), not `login`:
  - the `self.resolve_user` call is at :162;
  - `let ctx = …` is at :177;
  - `if claims.groups_overage {` is at :188;
  - `let _ = &ctx;` is at :193;
  - `resolve_user` is at :302–393;
  - `decode_jwt_payload` is at :406.
- **Email collision with the sync on.**
  - When `entra_sync.enabled` and `sync_one` returns `Ok(None)`, `resolve_identity` denies with `email_in_use` if an existing account not linked to this identity already has the token's email (case-insensitive). Otherwise it denies with `not_provisioned`.
  - Keep `sync_one`'s signature, because the index contract and P4 depend on it.
  - Add a sync-on test of the email collision. Review Focus #2 promises `email_in_use`.
- **Names come from the sync.** `complete()` must not overwrite `display_name` from the token's `name` claim while `entra_sync.enabled` is true. Otherwise names flip back and forth and `user.updated` is audited every night.
- **Text that still says "matched by email".** Rewrite every place that says SSO accounts are matched or linked by email:
  - `crates/pdms-web/templates/admin/users.html:31`;
  - the doc comment at `crates/pdms-core/src/directory/admin.rs:156`;
  - `crates/pdms-web/templates/admin/idp.html:3`: "updated at each sign-in" is no longer true while the sync is on.
  Add these files to `git add`.
- **Startup warning.** Log a `tracing::warn!` once at startup (where `Core` is built) when `auth.oidc.enabled` is true, `auth.oidc.auto_provision` is false and `entra_sync.enabled` is false. Accounts are no longer linked by email, so in that setup a person who has never signed in through SSO cannot. Warn only; do not reject the configuration.


---

### Task 9: Admin page "Entra ID sync"

**Files:**
- Modify: `crates/pdms-web/src/routes/admin.rs` (append handlers after `delete_mapping`)
- Modify: `crates/pdms-web/src/lib.rs` (admin routes, after the two `/idp` routes at lines 95–96)
- Create: `crates/pdms-web/templates/admin/entra.html`
- Modify: `crates/pdms-web/templates/base.html` (admin sub-nav, after line 45)
- Create: `crates/pdms-web/tests/entra.rs`

**Interfaces:**
- Consumes: `entra_values::{list, unmapped, upsert, remove}`, `entra_sync::{status, request_sync}`, `Core.graph`.
- Produces: `GET /admin/entra`, `POST /admin/entra/values` (`value`, `department_id`), `POST /admin/entra/values/remove` (`value`), `POST /admin/entra/sync` (`full` = `true` optional). All behind the existing `require_admin` (`ManageDirectory`) layer. P4 adds a SharePoint access section to this page.

- [ ] **Step 1: Write the failing tests.** Create `crates/pdms-web/tests/entra.rs`:

```rust
#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Administration → Entra ID sync.

mod support;

use pdms_core::directory::{self, entra_values};
use support::*;

#[tokio::test]
async fn admins_map_department_values_and_see_sync_status() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let rb = directory::create_department(&app.core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;

    let page = b.get("/admin/entra").await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Microsoft Graph is not configured"), "{}", page.body);
    assert!(page.body.contains("No sync has completed yet."));

    let r = b
        .post(
            "/admin/entra/values",
            &[("_csrf", &page.csrf()), ("value", "Road & Bridge"), ("department_id", &rb.to_string())],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    let page = b.get("/admin/entra").await;
    assert!(page.body.contains("Road &amp; Bridge"), "{}", page.body);

    // Sync now is refused while Graph is not configured.
    let r = b.post("/admin/entra/sync", &[("_csrf", &page.csrf())]).await;
    assert!(r.status.is_redirection());
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'directory.entra_sync'")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(jobs, 0);

    let r = b
        .post("/admin/entra/values/remove", &[("_csrf", &page.csrf()), ("value", "road and bridge")])
        .await;
    assert!(r.status.is_redirection());
    assert!(entra_values::list(&app.core.db).await.unwrap().is_empty());
}

#[tokio::test]
async fn people_without_directory_rights_cannot_open_it() {
    let app = app().await;
    let (_, secret) = user(&app.core, "staff@x.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("staff@x.org", &secret).await;
    assert_eq!(b.get("/admin/entra").await.status, 403);
    let page = b.get("/").await;
    let r = b
        .post("/admin/entra/sync", &[("_csrf", &page.csrf()), ("full", "true")])
        .await;
    assert_eq!(r.status, 403);
}
```

- [ ] **Step 2: Run them to see them fail.**

Run: `cargo test -p pdms-web --test entra`
Expected: FAIL — `assertion left == right failed: left: 404, right: 200` (route missing).

- [ ] **Step 3: Implement.** Append to `crates/pdms-web/src/routes/admin.rs` (extend the `use pdms_core::directory::{...}` import with `entra_sync, entra_values`):

```rust
#[derive(Template)]
#[template(path = "admin/entra.html")]
struct EntraPage {
    l: Layout,
    configured: bool,
    enabled: bool,
    attribute: String,
    last_run: String,
    summary: String,
    last_error: String,
    problems: Vec<String>,
    values: Vec<entra_values::EntraValue>,
    unmapped: Vec<entra_values::UnmappedValue>,
    departments: Vec<Department>,
}

pub async fn entra(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let core = &state.core;
    let status = entra_sync::status(&core.db).await?;
    let (summary, problems) = match &status.last_summary {
        Some(r) => (
            format!(
                "{} created, {} updated, {} departed; {} without a mapped department",
                r.created, r.updated, r.departed, r.unmapped
            ),
            r.errors.clone(),
        ),
        None => ("No sync has completed yet.".to_owned(), Vec::new()),
    };
    let page = EntraPage {
        l: Layout::new(&state, &user, flash, "Entra ID sync", "admin"),
        configured: core.graph.is_some(),
        enabled: core.config.entra_sync.enabled,
        attribute: core.config.entra_sync.attribute.clone(),
        last_run: fmt_time_opt(state.tz, status.last_run),
        summary,
        last_error: status.last_error.unwrap_or_default(),
        problems,
        values: entra_values::list(&core.db).await?,
        unmapped: entra_values::unmapped(&core.db).await?,
        departments: directory::list_departments(&core.db).await?,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct EntraValueForm {
    value: String,
    department_id: String,
}

pub async fn add_entra_value(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<EntraValueForm>,
) -> AppResult<Response> {
    let dept = uuid_opt(&f.department_id)?
        .ok_or_else(|| AppError::BadRequest("choose a department".into()))?;
    let r = entra_values::upsert(&state.core.db, &user.ctx, &f.value, dept).await;
    done(&state, "/admin/entra", r, "Mapping saved.")
}

#[derive(Deserialize)]
pub struct EntraRemoveForm {
    value: String,
}

pub async fn remove_entra_value(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<EntraRemoveForm>,
) -> AppResult<Response> {
    let r = entra_values::remove(&state.core.db, &user.ctx, &f.value).await;
    done(&state, "/admin/entra", r, "Mapping removed.")
}

#[derive(Deserialize)]
pub struct EntraSyncForm {
    full: Option<String>,
}

pub async fn entra_sync_now(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<EntraSyncForm>,
) -> AppResult<Response> {
    let r = entra_sync::request_sync(&state.core, &user.ctx, checkbox(&f.full)).await;
    done(
        &state,
        "/admin/entra",
        r,
        "Sync queued. Refresh this page in a minute to see the result.",
    )
}
```

In `crates/pdms-web/src/lib.rs`, after `.route("/idp/{id}/delete", post(admin::delete_mapping))` add:

```rust
        .route("/entra", get(admin::entra))
        .route("/entra/values", post(admin::add_entra_value))
        .route("/entra/values/remove", post(admin::remove_entra_value))
        .route("/entra/sync", post(admin::entra_sync_now))
```

In `crates/pdms-web/templates/base.html`, after `<li><a href="/admin/idp">Identity provider mappings</a></li>` add `<li><a href="/admin/entra">Entra ID sync</a></li>`.

Create `crates/pdms-web/templates/admin/entra.html`:

```html
{% extends "base.html" %}
{% block content %}
<p>People and their departments come from Microsoft Entra ID. Each night PDMS reads every enabled member account: it adds new people, updates names and departments, and marks people whose account is disabled or deleted as departed (they are signed out). Nobody is reactivated automatically. The department comes from the <strong>{{ attribute }}</strong> field of the Entra account; map each value used there to a PDMS department below.</p>

<section aria-labelledby="sync-h">
  <h2 id="sync-h">Sync status</h2>
  {% if !configured %}
  <p class="muted">Microsoft Graph is not configured, so PDMS cannot read Entra ID. Set <code>[graph]</code> (see the administrator guide).</p>
  {% else if !enabled %}
  <p class="muted">Entra sync is turned off (<code>[entra_sync] enabled = false</code>).</p>
  {% endif %}
  <dl>
    <dt>Last run</dt><dd>{{ last_run }}</dd>
    <dt>Result</dt><dd>{{ summary }}</dd>
    {% if !last_error.is_empty() %}<dt>Needs attention</dt><dd>{{ last_error }}</dd>{% endif %}
  </dl>
  {% if !problems.is_empty() %}
  <ul>{% for p in problems %}<li>{{ p }}</li>{% endfor %}</ul>
  {% endif %}
  <div class="toolbar">
    <form method="post" action="/admin/entra/sync">
      <input type="hidden" name="_csrf" value="{{ l.csrf }}">
      <button type="submit" class="primary">Sync now</button>
    </form>
    <form method="post" action="/admin/entra/sync">
      <input type="hidden" name="_csrf" value="{{ l.csrf }}">
      <input type="hidden" name="full" value="true">
      <button type="submit">Full resync</button>
    </form>
  </div>
</section>

<section aria-labelledby="map-h">
  <h2 id="map-h">Department mapping</h2>
  <table>
    <thead><tr><th scope="col">Entra {{ attribute }} value</th><th scope="col">PDMS department</th><th scope="col"><span class="visually-hidden">Actions</span></th></tr></thead>
    <tbody>
    {% for v in values %}
      <tr><td>{{ v.value }}</td><td>{{ v.department_name }}</td>
      <td><form method="post" action="/admin/entra/values/remove"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><input type="hidden" name="value" value="{{ v.value }}"><button type="submit" class="danger small">Remove</button></form></td></tr>
    {% else %}<tr><td colspan="3">No mappings yet.</td></tr>{% endfor %}
    </tbody>
  </table>
  <form method="post" action="/admin/entra/values" class="grid-form">
    <input type="hidden" name="_csrf" value="{{ l.csrf }}">
    <label for="ev">Value, as it appears in Entra</label><input id="ev" name="value" required maxlength="200">
    <label for="ed">Department</label>
    <select id="ed" name="department_id" required>{% for d in departments %}<option value="{{ d.id }}">{{ d.name }}</option>{% endfor %}</select>
    <span></span><button type="submit" class="primary">Save mapping</button>
  </form>
</section>

<section aria-labelledby="unmapped-h">
  <h2 id="unmapped-h">Values with no department</h2>
  <p>These people can sign in but see only what is shared with them. Map the value above, or correct the account in Entra (it is part of account creation).</p>
  <table>
    <thead><tr><th scope="col">Value</th><th scope="col">People</th></tr></thead>
    <tbody>
    {% for u in unmapped %}
      <tr><td>{% if u.value.is_empty() %}<em>(blank)</em>{% else %}{{ u.value }}{% endif %}</td><td>{{ u.users }}</td></tr>
    {% else %}<tr><td colspan="2">Every synced person has a mapped department.</td></tr>{% endfor %}
    </tbody>
  </table>
</section>
{% endblock %}
```

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p pdms-web --test entra`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit.**

```bash
git add crates/pdms-web/src/routes/admin.rs crates/pdms-web/src/lib.rs crates/pdms-web/templates/admin/entra.html crates/pdms-web/templates/base.html crates/pdms-web/tests/entra.rs
git commit -m "web: Administration → Entra ID sync page"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **Routes.** Add the routes after the two `/idp` routes, now at `crates/pdms-web/src/lib.rs:103–104`, before the `route_layer` at :105.
- **Admin tab.** The link goes in `base.html`'s admin tabs, after base.html:52 (`<li><a href="/admin/idp">Identity provider mappings</a></li>`), inside `{% if l.can_directory %}`. `partials/nav.html` needs no change.
- **R1 markup.** `admin/entra.html` uses the R1 admin markup (exemplar: `templates/admin/branding.html`):
  - `.panel` sections with `.panel-head`;
  - tables inside `.table-wrap`;
  - `.button` classes (`button primary`, `button`, `button small danger`);
  - `<dl class="facts">`;
  - a "not configured" notice as `<p class="flash flash-error" role="alert">` and the sync-off notice as `<p class="flash flash-info" role="status">`;
  - errors as `<span class="status status-critical">`;
  - forms as `.toolbar`, `.inline-form` and `.grid-form`.

  Keep the strings the tests look for: "Microsoft Graph is not configured", "No sync has completed yet." and `Road &amp; Bridge`. Shape:

```html
{% if !configured %}<p class="flash flash-error" role="alert">Microsoft Graph is not configured, so PDMS cannot read Entra ID. Set <code>[graph]</code> (see the administrator guide).</p>
{% else if !enabled %}<p class="flash flash-info" role="status">Entra sync is turned off (<code>[entra_sync] enabled = false</code>).</p>{% endif %}
<section class="panel" aria-labelledby="sync-h">
  <div class="panel-head"><h2 id="sync-h">Sync status</h2></div>
  <dl class="facts"><dt>Last run</dt><dd>{{ last_run }}</dd><dt>Result</dt><dd>{{ summary }}</dd>
    {% if !last_error.is_empty() %}<dt>Last error</dt><dd><span class="status status-critical">{{ last_error }}</span></dd>{% endif %}</dl>
  {% if !problems.is_empty() %}<h3>Needs attention</h3><ul>{% for p in problems %}<li>{{ p }}</li>{% endfor %}</ul>{% endif %}
  <div class="toolbar">
    <form method="post" action="/admin/entra/sync"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit" class="button primary">Sync now</button></form>
    <form method="post" action="/admin/entra/sync"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><input type="hidden" name="full" value="true"><button type="submit" class="button">Full resync</button></form>
  </div>
</section>
<section class="panel" aria-labelledby="map-h">
  <h2 id="map-h">Department mapping</h2>
  <div class="table-wrap"><table>… <td><form method="post" action="/admin/entra/values/remove" class="inline-form">…<button type="submit" class="button small danger">Remove</button></form></td> …</table></div>
  <form method="post" action="/admin/entra/values" class="grid-form">… <span></span><button type="submit" class="button primary">Save mapping</button></form>
</section>
<section class="panel" aria-labelledby="unmapped-h">
  <h2 id="unmapped-h">Values with no department</h2>
  <p class="help">…</p>
  <div class="table-wrap"><table>…</table></div>
</section>
```

- **SPDX first** in `crates/pdms-web/tests/entra.rs` (see Task 3's correction).


---

### Task 10: Documentation, example configuration, and final verification

**Files:**
- Modify: `config/pdms.example.toml` (after the `[graph_calendar]` block)
- Modify: `docs/ADMIN_GUIDE.md` (new subsection after "### Mapping Entra groups", line ~164; and §13 reference if it lists sections)
- Modify: `docs/DECISIONS.md` (append D-096 before "## Open questions")
- Modify: `docs/ROADMAP.md` §0.3 (status)

**Interfaces:**
- Consumes: everything above.
- Produces: operator documentation; ROADMAP shows P2 done. (Docker Compose secret wiring and `install.sh --configure graph` are P4's installer work; until then, set `PDMS_GRAPH__CERTIFICATE_FILE` by hand as documented below.)

- [ ] **Step 1: Example configuration.** Append to `config/pdms.example.toml`:

```toml
[graph]                         # Microsoft Graph as PDMS itself (app registration "PDMS Service")
# tenant_id = ""                # Directory (tenant) ID
# client_id = ""                # Application (client) ID
# certificate: PEM certificate + private key, from `pdms gen-graph-cert`; give it as a file:
#   PDMS_GRAPH__CERTIFICATE_FILE=/run/secrets/graph_certificate
# authority_url = "https://login.microsoftonline.com"
# api_url = "https://graph.microsoft.com/v1.0"
# timeout_seconds = 60
# max_retries = 4

[entra_sync]                    # people and departments from Entra ID (needs [graph] and auth.oidc.issuer_url)
enabled = false
run_at = "02:30"                # daily, in [org] timezone
attribute = "department"        # the Entra user property that names the department
include_guests = false
```

- [ ] **Step 2: Administrator guide.** In `docs/ADMIN_GUIDE.md`, after the "### Mapping Entra groups" subsection, insert:

```markdown
### Entra ID user sync

With the sync on, people and their departments come from Entra ID instead
of token group claims. Each night (at `[entra_sync] run_at`, default 02:30
local time) PDMS reads every enabled member account:

- new people are created, so they have access before their first sign-in;
- names, emails, and departments are updated;
- people whose account is disabled or deleted in Entra are marked
  **departed** and signed out. PDMS never reactivates anyone by itself;
- guest (B2B) accounts are skipped.

The department comes from the **Department** field of the Entra account.
Under **Administration → Entra ID sync**, map each value in use ("Road &
Bridge", "R&B", …) to a PDMS department; case, spacing, and `&`/"and" do
not matter. People with a blank or unmapped value can sign in but see only
what is shared with them; the page lists those values with counts. Make the
Department field part of account creation.

Roles (administrator, legal counsel, records officer, auditor, appointments
clerk, department head) are still assigned in PDMS. Department memberships
you add by hand are never changed by the sync. If the only administrator is
disabled in Entra, PDMS keeps them active and reports it on the page.

Sign-in matches people by their Entra object ID, never by email: an Entra
account whose email equals an existing PDMS account (for example the
break-glass administrator) is not linked; the sync reports it and sign-in
is refused until you rename or remove the other account.

**Setup.**

1. Create the certificate on the PDMS server:
   `docker compose run --rm app gen-graph-cert --out /var/lib/pdms/graph_certificate`
   (or `pdms gen-graph-cert --out secrets/graph_certificate` on a host with
   the binary). It writes the secret certificate + key (mode 0600) and a
   `.crt` file with the public certificate.
2. In Entra ID, register an app **PDMS Service**, add the Microsoft Graph
   **application** permission `User.Read.All`, grant admin consent, and
   upload the `.crt` file under **Certificates & secrets**.
3. Set `[graph] tenant_id` and `client_id`, give PDMS the certificate file
   as `PDMS_GRAPH__CERTIFICATE_FILE`, set `[entra_sync] enabled = true`
   (`auth.oidc.issuer_url` must be set), restart, and press **Sync now**.
   `pdms sync-directory` runs a sync from the command line;
   `pdms sync-directory --full` reads every account again.
```

- [ ] **Step 3: Decision record.** In `docs/DECISIONS.md`, before `## Open questions`, add:

```markdown
### D-096 Graph app-only authentication with a certificate (PS256) — Accepted
PDMS signs a client assertion with its own certificate (header `alg` PS256,
`x5t#S256` SHA-256 thumbprint, five-to-ten-minute lifetime), as the
Microsoft identity platform documents; there is no client secret to leak or
rotate in config. The certificate and key are one PEM file supplied through
`PDMS_GRAPH__CERTIFICATE_FILE`; `pdms gen-graph-cert` creates it (RSA-2048,
two years). Signing uses `ring` (constant time); the `rsa` crate is used
only to generate the key once. The bearer token is sent only to
`[graph] api_url`'s origin, never through a redirect, and pre-authenticated
download and upload URLs are fetched without it. The sync never reactivates
an account and never links accounts by email (D-086).
```

- [ ] **Step 4: Roadmap.** In `docs/ROADMAP.md` §0.3, add above sub-project 1 (below P1's `1a` row):

```markdown
| 2a | **Graph client and Entra user sync** (plan P2) | Certificate app-only Graph client; nightly `/users/delta` sync from the Department field; mapping admin page; SSO by object ID only | **Done** | [P2 plan](superpowers/plans/2026-10-14-p2-graph-client-entra-sync.md) |
```

and change sub-project 2's status to **Done (SharePoint folder access follows with P4)**.

- [ ] **Step 5: Full verification.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo sqlx prepare --workspace --check -- --all-targets
SQLX_OFFLINE=true cargo test --workspace --locked
```

Expected: `fmt` prints nothing; `clippy` finishes without warnings; `prepare --check` exits 0; every test binary reports `test result: ok.` with 0 failed (the only ignored tests are the two opt-in OCR and ClamAV tests). New binaries: `graph` (10 passed), `entra_sync` (13 passed), web `entra` (2 passed), web `oidc` (5 passed).

- [ ] **Step 6: Commit.**

```bash
git add config/pdms.example.toml docs/ADMIN_GUIDE.md docs/DECISIONS.md docs/ROADMAP.md
git commit -m "docs: Entra ID user sync and Graph certificate setup"
```

#### Pre-flight corrections (2026-10-21; these override the task text above)

- **ADMIN_GUIDE.**
  - Put the new Entra section after `docs/ADMIN_GUIDE.md:266`, at the end of the groups-mapping section, before "## 4. Documents" at :268.
  - Rewrite §2 (:66–99) so it no longer says SSO links a pre-created account by email (:89–92).
  - Say that the groups-claim step (:69) is not needed while the sync is on.
  - State the consequence: with SSO on, `auto_provision = false` and the sync off, only people already linked to their Entra account can sign in.
  - Fix :261–266 ("At every sign-in, PDMS adds and removes…") for the sync-on case.
- **DECISIONS.**
  - Append **D-096** after D-095, before "## Open questions" at `docs/DECISIONS.md:765`; D-090 to D-095 are taken.
  - Change D-086's status, "(design, not built)" at :654, to say it was built in P2.
- **ROADMAP.**
  - In `docs/ROADMAP.md` §0.3 the rows are now 1a, 1b, 1, 2, 3, 4, 5a, 5b. Mark row 2 (Entra user sync) **Done** and add any P2 note next to it rather than as a new "2a" row.
  - Update the date at :14.
  - Update the Known gaps "Identity" row (:244, which says departed staff are not disabled automatically) and §5 item 3 (:265).
- **Plan index.**
  - Change the "Produced by P2" CLI row: `gen-graph-cert` writes the PEM bundle to `--out` and the public certificate to `<out>.crt`, and prints that path and the thumbprint.
  - Renumber the remaining migration references for P3 and P4 (index :152 "Migration 0011" becomes 0012).
  - Add a note that P4's planned decisions D-091 to D-097 must be renumbered from D-097 when P4 is executed.
  - Leave the P3 and P4 plan bodies alone; their own pre-flight will renumber them.
  - Make sure "Changes recorded when P2 was written" lists every pre-flight deviation:
    - DELETE granted on `department_entra_values`;
    - a null `userType` counts as a member;
    - `model::UserSource` dropped;
    - a manual membership satisfies the mapping;
    - the sync requires `subject_claim = "oid"`;
    - the `email_in_use` check when the sync is on;
    - names are not taken from the token while the sync is on.

