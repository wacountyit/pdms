// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The Settings panel on Administration → Text extraction (OCR) (S1). The OCR
//! service runs in its own container, which the installer includes; this
//! turns its use on and off.

use pdms_authz::Capability;
use serde_json::Value;

use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::settings::{self, Check};

/// The OCR container's address in the Compose setup.
pub const DEFAULT_URL: &str = "http://ocr:8080";

#[derive(Clone, Copy, Debug, Default)]
pub struct OcrForm {
    pub text_extraction: bool,
    pub ocr: bool,
    pub suggest_dates: bool,
}

/// The sidecar address to use when OCR is on: the saved one, or the default.
fn address(core: &Core) -> String {
    let url = core.config().ocr.sidecar_url.trim().to_owned();
    if url.is_empty() {
        DEFAULT_URL.to_owned()
    } else {
        url
    }
}

/// Save `form` (requires `ManageSettings`); the OCR client is rebuilt at once.
pub async fn save(core: &Core, ctx: &Ctx, form: OcrForm) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let url = if form.ocr {
        address(core)
    } else {
        String::new()
    };
    let changes = [
        ("ocr.text_extraction", Value::Bool(form.text_extraction)),
        ("ocr.sidecar_url", Value::String(url)),
        ("ocr.suggest_dates", Value::Bool(form.suggest_dates)),
    ]
    .into_iter()
    .filter_map(|(key, value)| settings::unless_locked(core, key, value))
    .collect();
    settings::save(core, ctx, changes).await
}

/// Check that the OCR service answers (requires `ManageSettings`).
pub async fn test(core: &Core, ctx: &Ctx) -> CoreResult<Check> {
    ctx.require(Capability::ManageSettings)?;
    let url = address(core);
    let check = reach(&url).await;
    settings::record_test(core, ctx, "ocr", check.ok).await?;
    Ok(check)
}

async fn reach(url: &str) -> Check {
    let missing = format!(
        "PDMS can't reach the OCR service at {url}. If its container isn't running, re-run ./install.sh --configure ocr and include it."
    );
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(h) => h,
        Err(e) => return Check::failed(vec![], CoreError::internal(e).to_string()),
    };
    match http
        .get(format!("{}/healthz", url.trim_end_matches('/')))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            Check::passed(vec![(true, format!("The OCR service at {url} answers."))])
        }
        Ok(r) => Check::failed(
            vec![],
            format!("The OCR service at {url} answered {}.", r.status()),
        ),
        Err(_) => Check::failed(vec![], missing),
    }
}
