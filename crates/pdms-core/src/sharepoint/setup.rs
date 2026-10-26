// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Setting SharePoint storage up from Administration → SharePoint (S1,
//! D-114): the site settings, a read-only test, and turning storage on and
//! off, with the rules that keep files readable.
//!
//! - Storage can be turned on only while no document file is stored on this
//!   server: those files can't be read once SharePoint is on (D-102).
//! - The site and library can't change while SharePoint holds PDMS files,
//!   because PDMS tracks them by ID in that library.
//! - Turning storage off asks for the site address, typed, as confirmation.

use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::Capability;
use serde_json::Value;

use super::{Probe, SharePoint};
use crate::Core;
use crate::config::StorageBackend;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::settings::{self, Change, NewValue};

/// What Administration → SharePoint shows about storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageState {
    pub on: bool,
    /// Document files stored on this server (or in S3), not in SharePoint.
    pub local_files: i64,
    /// Document files PDMS stored in SharePoint.
    pub sharepoint_files: i64,
}

pub async fn state(core: &Core) -> CoreResult<StorageState> {
    let local_files = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM blobs WHERE storage_backend <> 'sharepoint'"#
    )
    .fetch_one(&core.db)
    .await?;
    let sharepoint_files = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM sharepoint_files"#)
        .fetch_one(&core.db)
        .await?;
    Ok(StorageState {
        on: core.config().storage.backend == StorageBackend::SharePoint,
        local_files,
        sharepoint_files,
    })
}

fn same_site(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

/// The site address and library `site_url` and `library` would give,
/// keeping what the configuration file sets: the page shows those fields
/// disabled, and a browser doesn't send a disabled field.
pub fn candidate(core: &Core, site_url: &str, library: &str) -> (String, String) {
    let file = core.file_config();
    let config = core.config();
    let pick = |key: &str, typed: &str, current: &str| {
        if file.locked.contains(key) {
            current.trim().to_owned()
        } else {
            typed.trim().to_owned()
        }
    };
    (
        pick("sharepoint.site_url", site_url, &config.sharepoint.site_url)
            .trim_end_matches('/')
            .to_owned(),
        pick("sharepoint.library", library, &config.sharepoint.library),
    )
}

/// Save the site address, library and check interval (requires
/// `ManageSettings`). The site and library can't change once SharePoint
/// holds PDMS files.
pub async fn save_site(
    core: &Core,
    ctx: &Ctx,
    site_url: &str,
    library: &str,
    interval_minutes: u64,
) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let config = core.config();
    let file = core.file_config();
    let (site_url, library) = candidate(core, site_url, library);
    let moved = !same_site(&site_url, &config.sharepoint.site_url)
        || !library.eq_ignore_ascii_case(&config.sharepoint.library);
    if moved && state(core).await?.sharepoint_files > 0 {
        return Err(CoreError::Conflict(
            "SharePoint already holds PDMS files in this site and library, and PDMS tracks them there, so the site and library can't change."
                .into(),
        ));
    }
    let mut changes = Vec::new();
    let mut put = |key: &str, value: Value| {
        if !file.locked.contains(key) {
            changes.push(Change {
                key: key.into(),
                value: NewValue::Set(value),
            });
        }
    };
    put("sharepoint.site_url", Value::String(site_url));
    put("sharepoint.library", Value::String(library));
    put(
        "sharepoint.delta_interval_minutes",
        Value::from(interval_minutes.clamp(1, 1440)),
    );
    settings::save(core, ctx, changes).await
}

/// Check `site_url` and `library` (or what the configuration file sets)
/// through the Microsoft 365 connection, changing nothing (requires
/// `ManageSettings`).
pub async fn test(core: &Core, ctx: &Ctx, site_url: &str, library: &str) -> CoreResult<Probe> {
    ctx.require(Capability::ManageSettings)?;
    let (site_url, library) = candidate(core, site_url, library);
    let probe = probe(core, &site_url, &library).await;
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Admin,
                "settings.tested",
                ctx.actor.clone(),
                "settings",
            )
            .outcome(if probe.ok {
                Outcome::Success
            } else {
                Outcome::Failure
            })
            .context("area", "sharepoint"),
        ],
    )
    .await?;
    Ok(probe)
}

async fn probe(core: &Core, site_url: &str, library: &str) -> Probe {
    let fail = |line: &str| Probe {
        ok: false,
        lines: vec![(false, line.to_owned())],
    };
    let Some(graph) = core.graph() else {
        return fail(
            "Set up the Microsoft 365 connection first (Administration → Microsoft 365 → Connection).",
        );
    };
    let mut cfg = core.config().sharepoint.clone();
    cfg.site_url = site_url.trim().trim_end_matches('/').to_owned();
    cfg.library = library.trim().to_owned();
    match SharePoint::new(graph, &cfg) {
        Ok(sp) => sp.probe().await,
        Err(e) => fail(&e.to_string()),
    }
}

/// Turn SharePoint storage on (requires `ManageSettings`): only while no
/// document file is stored on this server, and only when the saved site
/// passes the test. New uploads go to SharePoint from then on.
pub async fn turn_on(core: &Core, ctx: &Ctx) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    if core.file_config().locked.contains("storage.backend") {
        return Err(CoreError::validation(
            "[storage] backend is set in the configuration file, so it can't be changed here.",
        ));
    }
    let s = state(core).await?;
    if s.on {
        return Ok(());
    }
    if s.local_files > 0 {
        return Err(CoreError::Conflict(format!(
            "{} document file(s) are stored on this server. PDMS can't read them once SharePoint storage is on, so it can only be turned on before documents are added (on a test server, reset it first: SHAREPOINT_SETUP.md).",
            s.local_files
        )));
    }
    let config = core.config();
    if config.sharepoint.site_url.trim().is_empty() {
        return Err(CoreError::validation("Save the site address first."));
    }
    let p = probe(
        core,
        &config.sharepoint.site_url,
        &config.sharepoint.library,
    )
    .await;
    if !p.ok {
        let why = p
            .lines
            .iter()
            .find(|(ok, _)| !ok)
            .map_or("the test failed", |(_, l)| l.as_str())
            .to_owned();
        return Err(CoreError::Conflict(format!(
            "SharePoint storage stays off: {why}"
        )));
    }
    set_backend(core, ctx, "sharepoint").await
}

/// Turn SharePoint storage off (requires `ManageSettings`). `typed_site`
/// must be the site address: files already in SharePoint stay there, but
/// PDMS can no longer open them.
pub async fn turn_off(core: &Core, ctx: &Ctx, typed_site: &str) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    if core.file_config().locked.contains("storage.backend") {
        return Err(CoreError::validation(
            "[storage] backend is set in the configuration file, so it can't be changed here.",
        ));
    }
    let config = core.config();
    if config.storage.backend != StorageBackend::SharePoint {
        return Ok(());
    }
    if !same_site(typed_site, &config.sharepoint.site_url) {
        return Err(CoreError::validation(
            "Type the site address exactly as it is shown to turn SharePoint storage off.",
        ));
    }
    set_backend(core, ctx, "local").await
}

async fn set_backend(core: &Core, ctx: &Ctx, backend: &str) -> CoreResult<()> {
    settings::save(
        core,
        ctx,
        vec![Change {
            key: "storage.backend".into(),
            value: NewValue::Set(Value::String(backend.into())),
        }],
    )
    .await
}
