// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Email, OCR and virus-scanning settings in Administration (S1 Phase 5):
//! each test against a real or fake service, and saving rebuilds the
//! service at once.

mod common;

use common::*;
use pdms_core::config::{Secret, SmtpSecurity};
use pdms_core::notifications::mail_setup::{self, EmailForm};
use pdms_core::ocr::setup::{self as ocr_setup, OcrForm};
use pdms_core::scanning::{self, ScanForm};
use pdms_core::{Config, Core, CoreError};
use testcontainers::GenericImage;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.file_config()).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

fn email_form(port: u16) -> EmailForm {
    EmailForm {
        on: true,
        host: "127.0.0.1".into(),
        port,
        security: SmtpSecurity::None,
        username: String::new(),
        password: String::new(),
        from: "Records System <pdms@example.org>".into(),
        reply_to: String::new(),
    }
}

#[tokio::test]
async fn a_test_email_is_sent_with_the_typed_settings_before_saving() {
    let c = GenericImage::new("axllent/mailpit", "latest")
        .with_exposed_port(1025.tcp())
        .with_exposed_port(8025.tcp())
        .with_wait_for(WaitFor::message_on_stdout("accessible via"))
        .start()
        .await
        .expect("start mailpit");
    let smtp = c.get_host_port_ipv4(1025).await.unwrap();
    let api = c.get_host_port_ipv4(8025).await.unwrap();
    let (core, _db) = core().await;

    let check = mail_setup::send_test(&core, &system(), &email_form(smtp), "clerk@example.org")
        .await
        .unwrap();
    assert!(check.ok, "{check:?}");
    assert!(!core.mailer().enabled(), "nothing saved");
    let body = reqwest::get(format!("http://127.0.0.1:{api}/api/v1/messages"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total"], 1, "{body}");
    assert_eq!(v["messages"][0]["To"][0]["Address"], "clerk@example.org");
    // The audit log has the result, not the address.
    assert!(
        audit_actions(&core)
            .await
            .contains(&"settings.tested".to_owned())
    );
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log a WHERE a::text LIKE '%clerk@example.org%'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(leaked, 0);

    // Saved, the mailer is rebuilt at once.
    mail_setup::save(&core, &system(), &email_form(smtp))
        .await
        .unwrap();
    assert!(core.mailer().enabled());
    assert_eq!(core.config().email.smtp_port, smtp);
}

#[tokio::test]
async fn a_failed_test_email_says_why_and_an_empty_password_keeps_the_saved_one() {
    let (core, _db) = core().await;
    // Nothing listens on port 9.
    let check = mail_setup::send_test(&core, &system(), &email_form(9), "clerk@example.org")
        .await
        .unwrap();
    assert!(!check.ok);
    assert!(check.lines[0].1.contains("wasn't sent"), "{check:?}");
    let err = mail_setup::send_test(&core, &system(), &email_form(9), "")
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "{err:?}");

    let mut f = email_form(587);
    f.username = "pdms@example.org".into();
    f.password = "mail-pass-77".into();
    mail_setup::save(&core, &system(), &f).await.unwrap();
    f.password.clear();
    f.on = false;
    mail_setup::save(&core, &system(), &f).await.unwrap();
    assert_eq!(core.config().email.smtp_password.expose(), "mail-pass-77");
    assert!(!core.mailer().enabled(), "turned off");
}

#[tokio::test]
async fn email_settings_in_the_configuration_file_are_left_alone() {
    let (core, _db) = core_with(|c| {
        c.email.smtp_host = "mail.example.org".into();
        c.email.smtp_password = Secret::new("from-file");
        c.locked.insert("email.smtp_host".into());
        c.locked.insert("email.smtp_password".into());
    })
    .await;
    let mut f = email_form(25);
    f.password = "typed".into();
    mail_setup::save(&core, &system(), &f).await.unwrap();
    assert_eq!(core.config().email.smtp_host, "mail.example.org");
    assert_eq!(core.config().email.smtp_password.expose(), "from-file");
    assert_eq!(core.config().email.smtp_port, 25);
}

/// An OCR service that only answers `/healthz`.
async fn fake_ocr() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn ocr_is_turned_on_and_off_and_tested() {
    let url = fake_ocr().await;
    let (core, _db) = core_with(|c| c.ocr.sidecar_url = url.clone()).await;
    assert!(ocr_setup::test(&core, &system()).await.unwrap().ok);

    let off = OcrForm {
        text_extraction: true,
        ocr: false,
        suggest_dates: false,
    };
    ocr_setup::save(&core, &system(), off).await.unwrap();
    assert!(core.ocr().is_none(), "applied at once");
    assert!(!core.config().ocr.suggest_dates);
    // On again: the Compose address, where the container answers.
    ocr_setup::save(&core, &system(), OcrForm { ocr: true, ..off })
        .await
        .unwrap();
    assert!(core.ocr().is_some());
    assert_eq!(core.config().ocr.sidecar_url, ocr_setup::DEFAULT_URL);
    let check = ocr_setup::test(&core, &system()).await.unwrap();
    assert!(!check.ok);
    assert!(check.lines[0].1.contains("--configure ocr"), "{check:?}");
}

/// A clamd that reports the EICAR file (`eicar`), or finds nothing.
async fn fake_clamd(eicar: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut cmd = [0u8; 10];
                s.read_exact(&mut cmd).await.unwrap();
                loop {
                    let mut len = [0u8; 4];
                    s.read_exact(&mut len).await.unwrap();
                    let n = u32::from_be_bytes(len) as usize;
                    if n == 0 {
                        break;
                    }
                    let mut chunk = vec![0u8; n];
                    s.read_exact(&mut chunk).await.unwrap();
                }
                let reply: &[u8] = if eicar {
                    b"stream: Eicar-Test-Signature FOUND\0"
                } else {
                    b"stream: OK\0"
                };
                s.write_all(reply).await.unwrap();
            });
        }
    });
    addr.to_string()
}

#[tokio::test]
async fn the_virus_scanner_test_expects_the_eicar_file_to_be_found() {
    let addr = fake_clamd(true).await;
    let (core, _db) = core_with(|c| c.storage.clamav_addr = addr.clone()).await;
    let check = scanning::test(&core, &system()).await.unwrap();
    assert!(check.ok, "{check:?}");
    assert!(check.lines[0].1.contains("Eicar-Test-Signature"));
    let spool: Vec<_> = std::fs::read_dir(&core.config().storage.spool_path)
        .map(|d| d.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        !spool
            .iter()
            .any(|e| e.file_name().to_string_lossy().starts_with("eicar-test")),
        "the test file is removed"
    );

    let blind = fake_clamd(false).await;
    let (core, _db) = core_with(|c| c.storage.clamav_addr = blind.clone()).await;
    let check = scanning::test(&core, &system()).await.unwrap();
    assert!(!check.ok);
    assert!(check.lines[1].1.contains("didn't find"), "{check:?}");
}

#[tokio::test]
async fn virus_scanning_is_turned_on_and_off_at_once() {
    let (core, _db) = core().await;
    assert!(core.scanner().is_none());
    scanning::save(
        &core,
        &system(),
        ScanForm {
            on: true,
            required: false,
        },
    )
    .await
    .unwrap();
    assert!(core.scanner().is_some());
    assert_eq!(core.config().storage.clamav_addr, scanning::DEFAULT_ADDR);
    assert!(!core.config().storage.clamav_required);
    let check = scanning::test(&core, &system()).await.unwrap();
    assert!(!check.ok, "no ClamAV container here");
    assert!(check.lines[0].1.contains("--configure clamav"), "{check:?}");
    scanning::save(
        &core,
        &system(),
        ScanForm {
            on: false,
            required: true,
        },
    )
    .await
    .unwrap();
    assert!(core.scanner().is_none());
}

#[tokio::test]
async fn only_settings_managers_change_these_settings() {
    let (core, _db) = core().await;
    let (clerk, _) = user_in_new_department(&core, "RB", "clerk@x.org").await;
    let ctx = as_user(&core, clerk).await;
    for err in [
        mail_setup::save(&core, &ctx, &email_form(25))
            .await
            .unwrap_err(),
        mail_setup::send_test(&core, &ctx, &email_form(25), "a@example.org")
            .await
            .unwrap_err(),
        ocr_setup::save(&core, &ctx, OcrForm::default())
            .await
            .unwrap_err(),
        ocr_setup::test(&core, &ctx).await.unwrap_err(),
        scanning::save(&core, &ctx, ScanForm::default())
            .await
            .unwrap_err(),
        scanning::test(&core, &ctx).await.unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::Forbidden(_)), "{err:?}");
    }
}
