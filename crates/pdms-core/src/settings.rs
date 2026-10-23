// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Settings in Administration (S1): the settings an administrator can change
//! at run time, where each value comes from, and how a change is saved and
//! applied.
//!
//! A setting set in `config/pdms.toml` or a `PDMS_*` variable is *locked*: it
//! wins, and the admin page shows it read-only (D-111). Otherwise the value
//! stored in the database applies (`app_settings`; secrets encrypted in
//! `app_secrets`), and otherwise the built-in default. Every save checks the
//! merged configuration, builds the services from it before anything is
//! stored, writes its audit event in the same transaction, and tells the
//! other processes (`NOTIFY pdms_settings` and `settings_version`).

use std::collections::BTreeMap;

use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::Capability;
use serde_json::Value;
use sqlx::PgPool;

use crate::config::Config;
use crate::context::Ctx;
use crate::crypto::Keys;
use crate::error::{CoreError, CoreResult};
use crate::{Core, Live};

/// The admin page a setting belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    Microsoft365,
    SignIn,
    EntraSync,
    SharePoint,
    Email,
    Ocr,
    VirusScanning,
}

/// What kind of value a setting holds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Text,
    Number,
    /// One of the listed values.
    Choice(&'static [&'static str]),
    /// Never shown once stored; encrypted at rest.
    Secret,
}

#[derive(Copy, Clone, Debug)]
pub struct SettingDef {
    /// The configuration path, for example `email.smtp_host`.
    pub key: &'static str,
    pub kind: Kind,
    pub area: Area,
}

const fn def(key: &'static str, kind: Kind, area: Area) -> SettingDef {
    SettingDef { key, kind, area }
}

/// Every setting the admin pages manage (spec §2). The Microsoft Graph
/// certificate is not here: it comes from `graph_certificates` (Phase 2).
pub const SETTINGS: &[SettingDef] = &[
    def("graph.tenant_id", Kind::Text, Area::Microsoft365),
    def("graph.client_id", Kind::Text, Area::Microsoft365),
    def("sharepoint.site_url", Kind::Text, Area::SharePoint),
    def("sharepoint.library", Kind::Text, Area::SharePoint),
    def(
        "sharepoint.delta_interval_minutes",
        Kind::Number,
        Area::SharePoint,
    ),
    def(
        "storage.backend",
        Kind::Choice(&["local", "sharepoint"]),
        Area::SharePoint,
    ),
    def("entra_sync.enabled", Kind::Bool, Area::EntraSync),
    def("entra_sync.run_at", Kind::Text, Area::EntraSync),
    def("entra_sync.attribute", Kind::Text, Area::EntraSync),
    def("entra_sync.include_guests", Kind::Bool, Area::EntraSync),
    def("auth.oidc.enabled", Kind::Bool, Area::SignIn),
    def("auth.oidc.issuer_url", Kind::Text, Area::SignIn),
    def("auth.oidc.client_id", Kind::Text, Area::SignIn),
    def("auth.oidc.client_secret", Kind::Secret, Area::SignIn),
    def("auth.oidc.display_name", Kind::Text, Area::SignIn),
    def("auth.oidc.auto_provision", Kind::Bool, Area::SignIn),
    def(
        "email.transport",
        Kind::Choice(&["smtp", "disabled"]),
        Area::Email,
    ),
    def("email.smtp_host", Kind::Text, Area::Email),
    def("email.smtp_port", Kind::Number, Area::Email),
    def(
        "email.smtp_security",
        Kind::Choice(&["starttls", "tls", "none"]),
        Area::Email,
    ),
    def("email.smtp_username", Kind::Text, Area::Email),
    def("email.smtp_password", Kind::Secret, Area::Email),
    def("email.from", Kind::Text, Area::Email),
    def("email.reply_to", Kind::Text, Area::Email),
    def("ocr.text_extraction", Kind::Bool, Area::Ocr),
    def("ocr.sidecar_url", Kind::Text, Area::Ocr),
    def("ocr.suggest_dates", Kind::Bool, Area::Ocr),
    def("storage.clamav_addr", Kind::Text, Area::VirusScanning),
    def("storage.clamav_required", Kind::Bool, Area::VirusScanning),
];

pub fn find(key: &str) -> Option<&'static SettingDef> {
    SETTINGS.iter().find(|d| d.key == key)
}

/// The environment variable that sets `key` (`PDMS_EMAIL__SMTP_HOST`).
pub fn env_name(key: &str) -> String {
    format!("PDMS_{}", key.to_ascii_uppercase().replace('.', "__"))
}

/// The value of `key` in `config`, as JSON.
pub fn get(config: &Config, key: &str) -> CoreResult<Value> {
    let all = serde_json::to_value(config).map_err(CoreError::internal)?;
    all.pointer(&pointer(key))
        .cloned()
        .ok_or_else(|| CoreError::internal(format!("unknown setting {key}")))
}

/// `config` with `key` set to `value`. The value must have the setting's
/// type (the configuration's own deserialization checks it).
pub fn set(config: &Config, key: &str, value: Value) -> CoreResult<Config> {
    let mut all = serde_json::to_value(config).map_err(CoreError::internal)?;
    let slot = all
        .pointer_mut(&pointer(key))
        .ok_or_else(|| CoreError::internal(format!("unknown setting {key}")))?;
    *slot = value;
    let locked = config.locked.clone();
    let mut next: Config =
        serde_json::from_value(all).map_err(|e| CoreError::validation(format!("{key}: {e}")))?;
    next.locked = locked;
    Ok(next)
}

fn pointer(key: &str) -> String {
    format!("/{}", key.replace('.', "/"))
}

/// Where the effective value of a setting comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `config/pdms.toml` or a `PDMS_*` variable: read-only on the page.
    File,
    Database,
    Default,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::File => "file",
            Source::Database => "database",
            Source::Default => "default",
        }
    }
}

/// Values stored in the database: the non-secret ones from `app_settings`
/// and the decrypted secrets from `app_secrets` (registry keys only).
pub async fn load_overrides(db: &PgPool, keys: &Keys) -> CoreResult<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    let plain = sqlx::query!("SELECT key, value FROM app_settings")
        .fetch_all(db)
        .await?;
    for r in plain {
        if find(&r.key).is_some_and(|d| d.kind != Kind::Secret) {
            out.insert(r.key, r.value);
        }
    }
    let secrets = sqlx::query!("SELECT key, ciphertext FROM app_secrets")
        .fetch_all(db)
        .await?;
    for r in secrets {
        if find(&r.key).is_some_and(|d| d.kind == Kind::Secret) {
            let plain = keys.decrypt_setting(&r.key, &r.ciphertext).map_err(|_| {
                CoreError::internal(format!("setting {} cannot be decrypted", r.key))
            })?;
            let text = String::from_utf8(plain).map_err(CoreError::internal)?;
            out.insert(r.key, Value::String(text));
        }
    }
    if let Some(pem) = crate::graph::certs::effective_pem(db, keys).await? {
        out.insert(CERTIFICATE.to_owned(), Value::String(pem));
    }
    Ok(out)
}

/// Not a registry key: the certificate made in PDMS (D-113), applied by
/// [`effective`] only when the file has none and the IDs are set.
const CERTIFICATE: &str = "graph.certificate";

/// The file configuration with the stored values applied to every setting
/// the file doesn't lock, checked as a whole.
pub fn effective(file: &Config, overrides: &BTreeMap<String, Value>) -> CoreResult<Config> {
    let mut config = file.clone();
    for (key, value) in overrides {
        if config.locked.contains(key) || key == CERTIFICATE {
            continue;
        }
        config = set(&config, key, value.clone())?;
    }
    // The certificate made in PDMS signs in only once both IDs are set, so
    // making one before the IDs are known leaves Microsoft Graph off rather
    // than half-configured. A certificate in the file wins.
    if let Some(pem) = overrides.get(CERTIFICATE)
        && config.graph.certificate.is_empty()
        && !config.graph.tenant_id.is_empty()
        && !config.graph.client_id.is_empty()
    {
        config = set(&config, CERTIFICATE, pem.clone())?;
    }
    config
        .validate()
        .map_err(|e| CoreError::validation(e.to_string()))?;
    Ok(config)
}

/// Where each setting's effective value comes from.
pub async fn sources(core: &Core) -> CoreResult<BTreeMap<&'static str, Source>> {
    let stored = load_overrides(&core.db, &core.keys).await?;
    let file = core.file_config();
    Ok(SETTINGS
        .iter()
        .map(|d| {
            let src = if file.locked.contains(d.key) {
                Source::File
            } else if stored.contains_key(d.key) {
                Source::Database
            } else {
                Source::Default
            };
            (d.key, src)
        })
        .collect())
}

/// A change to one setting.
#[derive(Clone, Debug)]
pub struct Change {
    pub key: String,
    pub value: NewValue,
}

#[derive(Clone, Debug)]
pub enum NewValue {
    Set(Value),
    /// Remove the stored value: the default applies again.
    Clear,
}

/// Save `changes` (requires `ManageSettings`) and apply them. Refused, with
/// nothing stored, when a setting is unknown or locked, when the merged
/// configuration is invalid, or when its services can't be built.
pub async fn save(core: &Core, ctx: &Ctx, changes: Vec<Change>) -> CoreResult<()> {
    store(core, ctx, changes, false).await
}

/// Like [`save`], but also for settings the configuration file sets: their
/// stored values apply once the file stops setting them. Used by
/// `pdms settings import` (spec §9).
pub(crate) async fn save_for_import(
    core: &Core,
    ctx: &Ctx,
    changes: Vec<Change>,
) -> CoreResult<()> {
    store(core, ctx, changes, true).await
}

async fn store(core: &Core, ctx: &Ctx, changes: Vec<Change>, file_set_ok: bool) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    if changes.is_empty() {
        return Ok(());
    }
    let file = core.file_config();
    let mut stored = load_overrides(&core.db, &core.keys).await?;
    let before = stored.clone();
    for c in &changes {
        let d = find(&c.key)
            .ok_or_else(|| CoreError::validation(format!("unknown setting {}", c.key)))?;
        if file.locked.contains(d.key) && !file_set_ok {
            return Err(CoreError::validation(format!(
                "{} is set in the configuration file, so it can't be changed here",
                d.key
            )));
        }
        match &c.value {
            NewValue::Set(v) => {
                stored.insert(d.key.to_owned(), v.clone());
            }
            NewValue::Clear => {
                stored.remove(d.key);
            }
        }
    }
    // Check the whole result, and build its services, before storing anything.
    let next = effective(&file, &stored)?;
    Live::build(next, 0)?;

    let mut tx = core.db.begin().await?;
    let mut event = AuditEvent::new(
        Category::Admin,
        "settings.changed",
        ctx.actor.clone(),
        "settings",
    );
    for c in &changes {
        let Some(d) = find(&c.key) else { continue };
        let secret = d.kind == Kind::Secret;
        match &c.value {
            NewValue::Set(v) if secret => {
                let text = v.as_str().unwrap_or_default();
                let ciphertext = core
                    .keys
                    .encrypt_setting(d.key, text.as_bytes())
                    .map_err(CoreError::internal)?;
                sqlx::query!(
                    "INSERT INTO app_secrets (key, ciphertext, updated_by) VALUES ($1, $2, $3)
                     ON CONFLICT (key) DO UPDATE
                        SET ciphertext = EXCLUDED.ciphertext, updated_by = EXCLUDED.updated_by,
                            updated_at = now()",
                    d.key,
                    ciphertext,
                    ctx.user_id()
                )
                .execute(&mut *tx)
                .await?;
            }
            NewValue::Set(v) => {
                sqlx::query!(
                    "INSERT INTO app_settings (key, value, updated_by) VALUES ($1, $2, $3)
                     ON CONFLICT (key) DO UPDATE
                        SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by,
                            updated_at = now()",
                    d.key,
                    v,
                    ctx.user_id()
                )
                .execute(&mut *tx)
                .await?;
            }
            NewValue::Clear if secret => {
                sqlx::query!("DELETE FROM app_secrets WHERE key = $1", d.key)
                    .execute(&mut *tx)
                    .await?;
            }
            NewValue::Clear => {
                sqlx::query!("DELETE FROM app_settings WHERE key = $1", d.key)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        event = if secret {
            let what = match c.value {
                NewValue::Set(_) => "replaced",
                NewValue::Clear => "cleared",
            };
            event.context(d.key, what)
        } else {
            let old = before.get(d.key).map_or(AuditValue::Null, json_audit);
            let new = match &c.value {
                NewValue::Set(v) => json_audit(v),
                NewValue::Clear => AuditValue::Null,
            };
            event.change(d.key, old, new)
        };
    }
    bump_version(&mut tx).await?;
    pdms_audit::append(&mut tx, vec![event]).await?;
    tx.commit().await?;
    core.reload().await?;
    Ok(())
}

fn json_audit(v: &Value) -> AuditValue {
    AuditValue::from_json(v).unwrap_or(AuditValue::Null)
}

/// Bump `settings_version` and tell the other processes (the notification
/// is delivered when the transaction commits).
pub(crate) async fn bump_version(conn: &mut sqlx::PgConnection) -> CoreResult<i64> {
    let v = sqlx::query_scalar!(
        "UPDATE settings_version SET version = version + 1 WHERE id RETURNING version"
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query("SELECT pg_notify('pdms_settings', $1)")
        .bind(v.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(v)
}

/// The settings version in the database.
pub async fn current_version(db: &PgPool) -> CoreResult<i64> {
    Ok(
        sqlx::query_scalar!("SELECT version FROM settings_version WHERE id")
            .fetch_one(db)
            .await?,
    )
}

/// Keep `core`'s settings current: reload when another process changes
/// them (`NOTIFY pdms_settings`), and check every 60 seconds in case a
/// notification was missed. The receiver sees each new version.
pub fn watch(core: Core) -> tokio::sync::watch::Receiver<i64> {
    let (tx, rx) = tokio::sync::watch::channel(core.live().version);
    tokio::spawn(async move {
        let mut listener = match sqlx::postgres::PgListener::connect_with(&core.db).await {
            Ok(mut l) => match l.listen("pdms_settings").await {
                Ok(()) => Some(l),
                Err(e) => {
                    tracing::warn!(error = %e, "settings: LISTEN failed; checking every minute");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "settings: no listener; checking every minute");
                None
            }
        };
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            match listener.as_mut() {
                Some(l) => {
                    tokio::select! {
                        n = l.recv() => {
                            if let Err(e) = n {
                                tracing::warn!(error = %e, "settings: listener lost; checking every minute");
                                listener = None;
                            }
                        }
                        _ = tick.tick() => {}
                    }
                }
                None => {
                    tick.tick().await;
                }
            }
            match core.reload().await {
                Ok(true) => {
                    if tx.send(core.live().version).is_err() {
                        return;
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(error = %e, "settings: reload failed; keeping the current settings")
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_round_trips_through_the_config() {
        let config = Config::default();
        for d in SETTINGS {
            let v = get(&config, d.key).unwrap();
            let back = set(&config, d.key, v.clone()).unwrap();
            assert_eq!(get(&back, d.key).unwrap(), v, "{}", d.key);
        }
    }

    #[test]
    fn a_wrong_type_is_refused() {
        let config = Config::default();
        assert!(set(&config, "email.smtp_port", Value::String("x".into())).is_err());
        assert!(set(&config, "storage.backend", Value::String("floppy".into())).is_err());
    }

    #[test]
    fn env_names_follow_the_config_paths() {
        assert_eq!(
            env_name("auth.oidc.client_secret"),
            "PDMS_AUTH__OIDC__CLIENT_SECRET"
        );
    }
}

/// What a settings page's **Test** found: each step, and whether all passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Check {
    pub ok: bool,
    pub lines: Vec<(bool, String)>,
}

impl Check {
    pub fn passed(lines: Vec<(bool, String)>) -> Check {
        Check { ok: true, lines }
    }

    /// The steps so far, then the one that failed.
    pub fn failed(mut lines: Vec<(bool, String)>, why: impl Into<String>) -> Check {
        lines.push((false, why.into()));
        Check { ok: false, lines }
    }
}

/// Audit a settings test (`settings.tested`): the area and the result,
/// never the values tried.
pub async fn record_test(core: &Core, ctx: &Ctx, area: &str, ok: bool) -> CoreResult<()> {
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Admin,
                "settings.tested",
                ctx.actor.clone(),
                "settings",
            )
            .outcome(if ok {
                pdms_audit::Outcome::Success
            } else {
                pdms_audit::Outcome::Failure
            })
            .context("area", area.to_owned()),
        ],
    )
    .await?;
    Ok(())
}

/// A `Set` change for `key`, or nothing when the configuration file sets it.
pub fn unless_locked(core: &Core, key: &str, value: Value) -> Option<Change> {
    (!core.file_config().locked.contains(key)).then(|| Change {
        key: key.into(),
        value: NewValue::Set(value),
    })
}
