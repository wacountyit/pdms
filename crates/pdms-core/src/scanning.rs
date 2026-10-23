// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → Virus scanning (S1): uploads are scanned by ClamAV in its
//! own container, which the installer includes; this turns its use on and
//! off and tests it with the EICAR test file.

use pdms_authz::Capability;
use pdms_storage::scan::{ClamdScanner, ScanVerdict, VirusScanner};
use serde_json::Value;

use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;
use crate::settings::{self, Check};

/// The ClamAV container's address in the Compose setup.
pub const DEFAULT_ADDR: &str = "clamav:3310";

/// The EICAR test file: harmless, and found by every virus scanner. Kept in
/// two halves so this source file isn't itself reported as a virus.
fn eicar() -> Vec<u8> {
    [
        r"X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR",
        "-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*",
    ]
    .concat()
    .into_bytes()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScanForm {
    pub on: bool,
    /// Refuse uploads while the scanner can't be reached.
    pub required: bool,
}

fn address(core: &Core) -> String {
    let addr = core.config().storage.clamav_addr.trim().to_owned();
    if addr.is_empty() {
        DEFAULT_ADDR.to_owned()
    } else {
        addr
    }
}

/// Save `form` (requires `ManageSettings`); the scanner is rebuilt at once.
pub async fn save(core: &Core, ctx: &Ctx, form: ScanForm) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let addr = if form.on {
        address(core)
    } else {
        String::new()
    };
    let changes = [
        ("storage.clamav_addr", Value::String(addr)),
        ("storage.clamav_required", Value::Bool(form.required)),
    ]
    .into_iter()
    .filter_map(|(key, value)| settings::unless_locked(core, key, value))
    .collect();
    settings::save(core, ctx, changes).await
}

/// Scan the EICAR test file and expect it to be found (requires
/// `ManageSettings`).
pub async fn test(core: &Core, ctx: &Ctx) -> CoreResult<Check> {
    ctx.require(Capability::ManageSettings)?;
    let addr = address(core);
    let check = scan_eicar(core, &addr).await;
    settings::record_test(core, ctx, "virus_scanning", check.ok).await?;
    Ok(check)
}

async fn scan_eicar(core: &Core, addr: &str) -> Check {
    let dir = core.config().storage.spool_path.clone();
    let path = dir.join(format!("eicar-test-{}", uuid::Uuid::new_v4()));
    if let Err(e) = async {
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(&path, eicar()).await
    }
    .await
    {
        return Check::failed(vec![], format!("PDMS couldn't write the test file: {e}."));
    }
    let scanner = ClamdScanner {
        addr: addr.to_owned(),
        timeout: std::time::Duration::from_secs(30),
    };
    let verdict = scanner.scan(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    match verdict {
        Ok(ScanVerdict::Infected(name)) => Check::passed(vec![(
            true,
            format!("ClamAV at {addr} found the EICAR test file ({name}): scanning works."),
        )]),
        Ok(ScanVerdict::Clean) => Check::failed(
            vec![(true, format!("Reached ClamAV at {addr}."))],
            "ClamAV didn't find the EICAR test file. Its virus definitions may not have loaded yet; try again in a few minutes.",
        ),
        Err(e) => Check::failed(
            vec![],
            format!(
                "PDMS can't use ClamAV at {addr} ({e}). If its container isn't running, re-run ./install.sh --configure clamav and include it."
            ),
        ),
    }
}
