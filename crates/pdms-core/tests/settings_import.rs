// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! `pdms settings import` (S1 Phase 6): what an earlier installer wrote into
//! its managed blocks moves into the database, and nothing else does.

mod common;

use common::*;
use pdms_core::config::{Secret, SmtpSecurity, StorageBackend};
use pdms_core::graph::certs;
use pdms_core::settings_import::{self, managed_blocks};
use pdms_core::{Config, Core};

/// What an earlier installer wrote, plus one hand-written setting.
const CONFIG: &str = r#"[org]
name = "Example PDMS"

[notifications]
send_hour = 7

# >>> install.sh:sso (./install.sh --configure sso rewrites this block)
[auth.oidc]
enabled = true
issuer_url = "https://login.microsoftonline.com/01234567-89ab-cdef-0123-456789abcdef/v2.0"
client_id = "11234567-89ab-cdef-0123-456789abcdef"
display_name = "Microsoft"
# <<< install.sh:sso

# >>> install.sh:smtp (./install.sh --configure smtp rewrites this block)
[email]
transport = "smtp"
from = "Example PDMS <pdms@example.org>"
smtp_host = "smtp.example.org"
smtp_port = 587
smtp_security = "starttls"
smtp_username = "pdms@example.org"
reply_to = "records@example.org"
# <<< install.sh:smtp

# >>> install.sh:ocr (./install.sh --configure ocr rewrites this block)
[ocr]
sidecar_url = "http://ocr:8080"
# <<< install.sh:ocr

# >>> install.sh:storage (./install.sh --configure sharepoint or clamav rewrites this block)
[storage]
clamav_addr = "clamav:3310"
# <<< install.sh:storage

[entra_sync]
run_at = "04:00"
"#;

/// A `Core` whose file configuration is what `CONFIG` and its secret files
/// give, as an earlier install had it.
async fn installed(certificate: Option<&str>) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut c: Config = (*core.file_config()).clone();
    c.auth.oidc.enabled = true;
    c.auth.oidc.issuer_url =
        "https://login.microsoftonline.com/01234567-89ab-cdef-0123-456789abcdef/v2.0".into();
    c.auth.oidc.client_id = "11234567-89ab-cdef-0123-456789abcdef".into();
    c.auth.oidc.client_secret = Secret::new("oidc-secret-1");
    c.email.transport = pdms_core::config::EmailTransport::Smtp;
    c.email.from = "Example PDMS <pdms@example.org>".into();
    c.email.smtp_host = "smtp.example.org".into();
    c.email.smtp_security = SmtpSecurity::Starttls;
    c.email.smtp_username = "pdms@example.org".into();
    c.email.smtp_password = Secret::new("smtp-secret-2");
    c.email.reply_to = "records@example.org".into();
    c.ocr.sidecar_url = "http://ocr:8080".into();
    c.storage.clamav_addr = "clamav:3310".into();
    c.entra_sync.run_at = "04:00".into();
    for k in [
        "auth.oidc.enabled",
        "auth.oidc.issuer_url",
        "auth.oidc.client_id",
        "auth.oidc.display_name",
        "auth.oidc.client_secret",
        "email.transport",
        "email.from",
        "email.smtp_host",
        "email.smtp_port",
        "email.smtp_security",
        "email.smtp_username",
        "email.smtp_password",
        "email.reply_to",
        "ocr.sidecar_url",
        "storage.clamav_addr",
        "entra_sync.run_at",
    ] {
        c.locked.insert(k.into());
    }
    if let Some(pem) = certificate {
        c.graph.tenant_id = "01234567-89ab-cdef-0123-456789abcdef".into();
        c.graph.client_id = "11234567-89ab-cdef-0123-456789abcdef".into();
        c.graph.certificate = Secret::new(pem);
    }
    (Core::new(core.db.clone(), c).unwrap(), db)
}

#[test]
fn managed_blocks_are_found_by_their_markers() {
    let blocks = managed_blocks(CONFIG);
    let names: Vec<&str> = blocks.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["sso", "smtp", "ocr", "storage"]);
    assert!(blocks[2].1.contains("sidecar_url"));
    assert!(
        !blocks[0].1.contains("install.sh"),
        "the markers are not part of a block"
    );
}

#[tokio::test]
async fn import_copies_what_the_blocks_set_and_nothing_else() {
    let (core, _db) = installed(None).await;
    let r = settings_import::import(&core, CONFIG).await.unwrap();
    for k in [
        "auth.oidc.enabled",
        "auth.oidc.issuer_url",
        "auth.oidc.client_id",
        "auth.oidc.client_secret",
        "email.transport",
        "email.smtp_host",
        "email.smtp_password",
        "email.reply_to",
        "ocr.sidecar_url",
        "storage.clamav_addr",
    ] {
        assert!(r.imported.contains(&k.to_owned()), "{k}: {r:?}");
    }
    // Defaults the installer wrote out are not copied; hand-written values stay.
    assert!(!r.imported.contains(&"auth.oidc.display_name".to_owned()));
    assert!(!r.imported.contains(&"email.smtp_port".to_owned()));
    assert!(!r.imported.contains(&"entra_sync.run_at".to_owned()));
    assert!(
        r.stay_in_file.contains(&"entra_sync.run_at".to_owned()),
        "{r:?}"
    );
    assert!(r.certificate.is_none());

    // Secrets arrive encrypted.
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT ciphertext FROM app_secrets WHERE key = 'auth.oidc.client_secret'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("oidc-secret-1"));
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log a WHERE a::text LIKE '%secret-1%' OR a::text LIKE '%secret-2%'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(leaked, 0);

    // Once the installer removes the blocks, the stored values apply.
    let mut without: Config = (*core.file_config()).clone();
    without = Config {
        locked: ["entra_sync.run_at".to_owned()].into(),
        ..without
    };
    without.auth.oidc = Default::default();
    without.email = Default::default();
    without.ocr = Default::default();
    without.storage.clamav_addr = String::new();
    let after = Core::new(core.db.clone(), without).unwrap();
    assert!(after.reload().await.unwrap());
    let c = after.config();
    assert!(c.auth.oidc.enabled);
    assert_eq!(c.auth.oidc.client_secret.expose(), "oidc-secret-1");
    assert_eq!(c.email.smtp_host, "smtp.example.org");
    assert_eq!(c.email.smtp_password.expose(), "smtp-secret-2");
    assert_eq!(c.ocr.sidecar_url, "http://ocr:8080");
    assert_eq!(c.storage.clamav_addr, "clamav:3310");

    // A second run changes nothing.
    let again = settings_import::import(&core, CONFIG).await.unwrap();
    assert!(again.imported.is_empty(), "{again:?}");
    assert!(again.already_stored.contains(&"email.smtp_host".to_owned()));
}

#[tokio::test]
async fn the_certificate_file_becomes_the_active_certificate() {
    let pem = pdms_core::graph::cert::generate("PDMS Service", 30).unwrap();
    let (core, _db) = installed(Some(&pem.pem)).await;
    let r = settings_import::import(&core, CONFIG).await.unwrap();
    assert_eq!(r.certificate.as_deref(), Some(pem.thumbprint.as_str()));
    let list = certs::list(&core.db).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].state, "active");
    let days = (list[0].not_after - pem.not_after).num_days().abs();
    assert!(days <= 1, "expiry read from the certificate");
    // Again: still one.
    settings_import::import(&core, CONFIG).await.unwrap();
    assert_eq!(certs::list(&core.db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_install_with_sharepoint_imports_the_storage_switch() {
    let text = r#"
# >>> install.sh:sharepoint (./install.sh --configure sharepoint rewrites this block)
[graph]
tenant_id = "01234567-89ab-cdef-0123-456789abcdef"
client_id = "11234567-89ab-cdef-0123-456789abcdef"

[sharepoint]
site_url = "https://contoso.sharepoint.com/sites/PDMS"
library = "Documents"
# <<< install.sh:sharepoint

# >>> install.sh:storage (./install.sh --configure sharepoint or clamav rewrites this block)
[storage]
backend = "sharepoint"
clamav_addr = ""
# <<< install.sh:storage
"#;
    let pem = pdms_core::graph::cert::generate("PDMS Service", 30).unwrap();
    let (core, _db) = core().await;
    let mut c: Config = (*core.file_config()).clone();
    c.graph.tenant_id = "01234567-89ab-cdef-0123-456789abcdef".into();
    c.graph.client_id = "11234567-89ab-cdef-0123-456789abcdef".into();
    c.graph.certificate = Secret::new(pem.pem.clone());
    c.sharepoint.site_url = "https://contoso.sharepoint.com/sites/PDMS".into();
    c.storage.backend = StorageBackend::SharePoint;
    for k in [
        "graph.tenant_id",
        "graph.client_id",
        "sharepoint.site_url",
        "sharepoint.library",
        "storage.backend",
        "storage.clamav_addr",
    ] {
        c.locked.insert(k.into());
    }
    let core = Core::new(core.db.clone(), c).unwrap();
    let r = settings_import::import(&core, text).await.unwrap();
    for k in [
        "graph.tenant_id",
        "graph.client_id",
        "sharepoint.site_url",
        "storage.backend",
    ] {
        assert!(r.imported.contains(&k.to_owned()), "{k}: {r:?}");
    }
    assert!(
        !r.imported.contains(&"storage.clamav_addr".to_owned()),
        "empty is the default"
    );
    assert!(r.certificate.is_some());
}

#[tokio::test]
async fn the_certificate_in_use_stays_in_use_when_others_were_made_on_the_page() {
    let pem = pdms_core::graph::cert::generate("PDMS Service", 30).unwrap();
    let (core, _db) = installed(Some(&pem.pem)).await;
    let made = certs::generate(&core, &system()).await.unwrap();
    assert_eq!(made.state, "active");
    settings_import::import(&core, CONFIG).await.unwrap();
    let list = certs::list(&core.db).await.unwrap();
    let active: Vec<_> = list.iter().filter(|c| c.state == "active").collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].thumbprint_sha256, pem.thumbprint);
}
