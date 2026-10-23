// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Setting Microsoft sign-in up from Administration → Microsoft 365 →
//! Sign-in (S1): the saved values, a test that signs nobody in, and the rule
//! that a change can't lock everyone out (D-115).

use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::Capability;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::oidc::{CALLBACK_PATH, OidcProvider};
use crate::Core;
use crate::config::{OidcConfig, Secret};
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::settings::{self, Change, NewValue};

const ENTRA_PREFIX: &str = "https://login.microsoftonline.com/";
const ENTRA_SUFFIX: &str = "/v2.0";

/// `01234567-89ab-cdef-0123-456789abcdef`.
pub fn is_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The issuer for what the page's tenant field holds: an Entra tenant ID
/// becomes `https://login.microsoftonline.com/<tenant>/v2.0`; a full issuer
/// URL (another OpenID Connect provider) is kept as it is.
pub fn issuer_for(input: &str) -> CoreResult<String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(String::new());
    }
    if is_guid(input) {
        return Ok(format!(
            "{ENTRA_PREFIX}{}{ENTRA_SUFFIX}",
            input.to_ascii_lowercase()
        ));
    }
    if crate::config::https_or_loopback(input) {
        return Ok(input.to_owned());
    }
    Err(CoreError::validation(
        "Enter the directory (tenant) ID, which looks like 01234567-89ab-cdef-0123-456789abcdef, or an https issuer URL.",
    ))
}

/// The tenant ID in an Entra issuer URL, for showing in the tenant field.
pub fn tenant_of(issuer: &str) -> Option<&str> {
    issuer
        .strip_prefix(ENTRA_PREFIX)?
        .strip_suffix(ENTRA_SUFFIX)
        .filter(|t| is_guid(t))
}

/// The redirect URI to register in the app registration.
pub fn redirect_uri(core: &Core) -> String {
    core.config()
        .server
        .public_url
        .join(CALLBACK_PATH)
        .map(|u| u.to_string())
        .unwrap_or_default()
}

/// Identifies the settings a provider was built from, so the web server
/// rebuilds it only when one of them changed. The secret is hashed.
pub fn fingerprint(cfg: &OidcConfig, public_url: &url::Url) -> String {
    let mut h = Sha256::new();
    for part in [
        if cfg.enabled { "on" } else { "off" },
        &cfg.issuer_url,
        &cfg.client_id,
        cfg.client_secret.expose(),
        &cfg.scopes.join(" "),
        &cfg.display_name,
        public_url.as_str(),
    ] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    hex::encode(h.finalize())
}

/// What the Sign-in page saves.
#[derive(Clone, Debug, Default)]
pub struct SignInForm {
    pub enabled: bool,
    /// The tenant ID, or an issuer URL.
    pub tenant: String,
    pub client_id: String,
    /// Empty keeps the saved secret.
    pub client_secret: String,
    pub display_name: String,
    pub auto_provision: bool,
}

/// The sign-in settings `form` would give, keeping what the configuration
/// file sets and the saved secret when none was typed.
pub fn candidate(core: &Core, form: &SignInForm) -> CoreResult<OidcConfig> {
    let file = core.file_config();
    let mut cfg = core.config().auth.oidc.clone();
    let open = |key: &str| !file.locked.contains(key);
    if open("auth.oidc.enabled") {
        cfg.enabled = form.enabled;
    }
    if open("auth.oidc.issuer_url") {
        cfg.issuer_url = issuer_for(&form.tenant)?;
    }
    if open("auth.oidc.client_id") {
        cfg.client_id = form.client_id.trim().to_owned();
    }
    if open("auth.oidc.client_secret") && !form.client_secret.trim().is_empty() {
        cfg.client_secret = Secret::new(form.client_secret.trim());
    }
    if open("auth.oidc.display_name") && !form.display_name.trim().is_empty() {
        cfg.display_name = form.display_name.trim().chars().take(60).collect();
    }
    if open("auth.oidc.auto_provision") {
        cfg.auto_provision = form.auto_provision;
    }
    Ok(cfg)
}

/// The result of a test: each step, and whether all passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignInTest {
    pub ok: bool,
    pub lines: Vec<(bool, String)>,
}

/// Check `form` without saving it or signing anyone in (requires
/// `ManageSettings`): read the provider's sign-in configuration, then build
/// the request PDMS would send and check its client ID and redirect URI.
pub async fn test(core: &Core, ctx: &Ctx, form: &SignInForm) -> CoreResult<SignInTest> {
    ctx.require(Capability::ManageSettings)?;
    let t = match candidate(core, form) {
        Ok(cfg) => probe(core, &cfg).await,
        Err(e) => SignInTest {
            ok: false,
            lines: vec![(false, e.to_string())],
        },
    };
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Admin,
                "settings.tested",
                ctx.actor.clone(),
                "settings",
            )
            .outcome(if t.ok {
                Outcome::Success
            } else {
                Outcome::Failure
            })
            .context("area", "sign_in"),
        ],
    )
    .await?;
    Ok(t)
}

async fn probe(core: &Core, cfg: &OidcConfig) -> SignInTest {
    let mut lines = Vec::new();
    let fail = |mut lines: Vec<(bool, String)>, line: String| {
        lines.push((false, line));
        SignInTest { ok: false, lines }
    };
    if cfg.issuer_url.is_empty() {
        return fail(lines, "Enter the directory (tenant) ID.".into());
    }
    if cfg.client_id.is_empty() {
        return fail(lines, "Enter the application (client) ID.".into());
    }
    if cfg.client_secret.is_empty() {
        return fail(
            lines,
            "Enter the client secret (Certificates & secrets → Client secrets in the app registration).".into(),
        );
    }
    let public_url = core.config().server.public_url.clone();
    let provider = match OidcProvider::discover(cfg, &public_url).await {
        Ok(p) => p,
        Err(e) => {
            return fail(
                lines,
                format!(
                    "PDMS couldn't read the sign-in configuration from {}: {e}. Check the tenant ID, and that this server can reach it.",
                    cfg.issuer_url
                ),
            );
        }
    };
    lines.push((true, "Read the sign-in configuration.".into()));
    let (url, _) = provider.begin("/");
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    let redirect = redirect_uri(core);
    if param("client_id").as_deref() != Some(cfg.client_id.as_str()) {
        return fail(lines, "The sign-in request has the wrong client ID.".into());
    }
    if param("redirect_uri").as_deref() != Some(redirect.as_str()) {
        return fail(
            lines,
            "The sign-in request has the wrong redirect URI.".into(),
        );
    }
    lines.push((
        true,
        format!(
            "The sign-in request uses this client ID and the redirect URI {redirect}. Microsoft checks both when someone signs in: use Try sign-in after saving."
        ),
    ));
    SignInTest { ok: true, lines }
}

/// Save `form` (requires `ManageSettings`). With local sign-in off, the
/// change is refused unless Microsoft sign-in stays on and passes the test,
/// so a mistake can't lock everyone out.
pub async fn save(core: &Core, ctx: &Ctx, form: &SignInForm) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let cfg = candidate(core, form)?;
    if !core.config().auth.local.enabled {
        if !cfg.enabled {
            return Err(CoreError::validation(
                "Local sign-in is off ([auth.local] enabled = false in the configuration file), so Microsoft sign-in can't be turned off: nobody could sign in.",
            ));
        }
        let t = probe(core, &cfg).await;
        if !t.ok {
            let why = t
                .lines
                .iter()
                .find(|(ok, _)| !ok)
                .map_or("the test failed", |(_, l)| l.as_str())
                .to_owned();
            return Err(CoreError::validation(format!(
                "Local sign-in is off, so only a change that passes the test can be saved: {why}"
            )));
        }
    }
    let file = core.file_config();
    let current = core.config().auth.oidc.clone();
    let mut changes = Vec::new();
    let mut put = |key: &str, value: Value| {
        if !file.locked.contains(key) {
            changes.push(Change {
                key: key.into(),
                value: NewValue::Set(value),
            });
        }
    };
    put("auth.oidc.enabled", Value::Bool(cfg.enabled));
    put(
        "auth.oidc.issuer_url",
        Value::String(cfg.issuer_url.clone()),
    );
    put("auth.oidc.client_id", Value::String(cfg.client_id.clone()));
    put(
        "auth.oidc.display_name",
        Value::String(cfg.display_name.clone()),
    );
    put("auth.oidc.auto_provision", Value::Bool(cfg.auto_provision));
    if cfg.client_secret.expose() != current.client_secret.expose() {
        put(
            "auth.oidc.client_secret",
            Value::String(cfg.client_secret.expose().to_owned()),
        );
    }
    settings::save(core, ctx, changes).await
}
