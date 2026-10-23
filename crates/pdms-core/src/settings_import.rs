// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! `pdms settings import` (S1, spec §9, D-116): copy the settings an earlier
//! installer wrote into its managed blocks of `config/pdms.toml` (and their
//! secret files) into the database, so that once the installer removes
//! those blocks they can be changed in Administration.

use std::collections::BTreeSet;

use figment::Figment;
use figment::providers::{Format, Toml};
use serde_json::Value;

use crate::config::Config;
use crate::context::Ctx;
use crate::error::CoreResult;
use crate::settings::{self, Change, Kind, NewValue};
use crate::{Core, graph};

/// The installer's blocks that held settings now managed in Administration.
pub const MOVED_BLOCKS: [&str; 6] = ["sso", "smtp", "sharepoint", "ocr", "storage", "clamav"];

/// Which secret file each block's secret came from.
const BLOCK_SECRETS: [(&str, &str); 2] = [
    ("sso", "auth.oidc.client_secret"),
    ("smtp", "email.smtp_password"),
];

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Settings copied into the database.
    pub imported: Vec<String>,
    /// Already in the database, so left alone.
    pub already_stored: Vec<String>,
    /// The thumbprint of the certificate file, imported as the active certificate.
    pub certificate: Option<String>,
    /// Set in the file outside the installer's blocks: they stay there.
    pub stay_in_file: Vec<String>,
}

/// The text of each managed block `NAME` in `text` (between the
/// `# >>> install.sh:NAME` and `# <<< install.sh:NAME` lines).
pub fn managed_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# >>> install.sh:") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            open = Some((name, String::new()));
        } else if line.starts_with("# <<< install.sh:") {
            out.extend(open.take());
        } else if let Some((_, body)) = open.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// The settings the moved blocks in `text` set, and which blocks are there.
fn block_settings(text: &str) -> (BTreeSet<&'static str>, BTreeSet<String>) {
    let blocks: Vec<(String, String)> = managed_blocks(text)
        .into_iter()
        .filter(|(name, _)| MOVED_BLOCKS.contains(&name.as_str()))
        .collect();
    let joined: String = blocks.iter().map(|(_, b)| b.as_str()).collect();
    let fig = Figment::from(Toml::string(&joined));
    let keys = settings::SETTINGS
        .iter()
        .filter(|d| fig.find_value(d.key).is_ok())
        .map(|d| d.key)
        .collect();
    (keys, blocks.into_iter().map(|(n, _)| n).collect())
}

/// Copy what the moved blocks of `config_text` set into the database, with
/// the values the configuration currently has (the system actor, audited as
/// `settings.changed`). Values already stored are left alone, so running it
/// again changes nothing.
pub async fn import(core: &Core, config_text: &str) -> CoreResult<ImportReport> {
    let ctx = Ctx::system("settings-import");
    let file = core.file_config();
    let defaults = Config::default();
    let stored = settings::load_overrides(&core.db, &core.keys).await?;
    let (keys, blocks) = block_settings(config_text);
    let mut report = ImportReport::default();
    let mut changes = Vec::new();
    let mut take = |key: &str, value: Value, report: &mut ImportReport| {
        if stored.contains_key(key) {
            report.already_stored.push(key.to_owned());
        } else {
            report.imported.push(key.to_owned());
            changes.push(Change {
                key: key.to_owned(),
                value: NewValue::Set(value),
            });
        }
    };
    for key in &keys {
        let Some(d) = settings::find(key) else {
            continue;
        };
        if d.kind == Kind::Secret {
            continue;
        }
        let value = settings::get(&file, key)?;
        // A default written out by the installer ("not set up") needs no copy.
        if value == settings::get(&defaults, key)? {
            continue;
        }
        take(key, value, &mut report);
    }
    for (block, key) in BLOCK_SECRETS {
        let value = settings::get(&file, key)?;
        if blocks.contains(block) && value.as_str().is_some_and(|s| !s.is_empty()) {
            take(key, value, &mut report);
        }
    }
    report.stay_in_file = file
        .locked
        .iter()
        .filter(|k| !keys.contains(k.as_str()) && !BLOCK_SECRETS.iter().any(|(_, s)| s == k))
        .filter(|k| settings::find(k).is_some_and(|d| d.kind != Kind::Secret))
        .cloned()
        .collect();
    settings::save_for_import(core, &ctx, changes).await?;

    // The certificate file is the one PDMS signs in with (the file wins), so
    // it becomes the active certificate, also when certificates were made on
    // the Connection page: once the file is emptied, nothing changes.
    let pem = file.graph.certificate.expose().trim();
    if !pem.is_empty() {
        let thumbprint = graph::cert::Credential::from_pem(pem)
            .map_err(|e| {
                crate::CoreError::validation(format!("the certificate file can't be read: {e}"))
            })?
            .thumbprint()
            .to_owned();
        let known = graph::certs::list(&core.db).await?;
        match known.iter().find(|c| c.thumbprint_sha256 == thumbprint) {
            Some(c) if c.state == "active" => {}
            Some(c) => graph::certs::activate(core, &ctx, c.id).await?,
            None => {
                let c = graph::certs::import_pem(core, &ctx, pem).await?;
                if c.state != "active" {
                    graph::certs::activate(core, &ctx, c.id).await?;
                }
                report.certificate = Some(c.thumbprint_sha256);
            }
        }
    }
    Ok(report)
}
