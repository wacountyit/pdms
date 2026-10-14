#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The SMTP mailer against a real SMTP server (Mailpit).

use pdms_core::config::{EmailConfig, EmailTransport, SmtpSecurity};
use pdms_core::notifications::mailer::{Mailer, OutgoingEmail, SmtpMailer};
use testcontainers::GenericImage;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;

#[tokio::test]
async fn smtp_mailer_delivers_to_a_real_server() {
    let c = GenericImage::new("axllent/mailpit", "latest")
        .with_exposed_port(1025.tcp())
        .with_exposed_port(8025.tcp())
        .with_wait_for(WaitFor::message_on_stdout("accessible via"))
        .start()
        .await
        .expect("start mailpit");
    let smtp = c.get_host_port_ipv4(1025).await.unwrap();
    let api = c.get_host_port_ipv4(8025).await.unwrap();
    let cfg = EmailConfig {
        transport: EmailTransport::Smtp,
        from: "Records System <pdms@example.org>".into(),
        smtp_host: "127.0.0.1".into(),
        smtp_port: smtp,
        smtp_security: SmtpSecurity::None,
        ..EmailConfig::default()
    };
    let mailer = SmtpMailer::new(&cfg).unwrap();
    mailer
        .send(&OutgoingEmail {
            to_name: "Alice Ops".into(),
            to_address: "alice@example.org".into(),
            subject: "[Test] Review due in 30 days: PAG-2026-000001 Facilities Maintenance".into(),
            text: "Plain text body".into(),
            html: "<p>HTML body</p>".into(),
            message_id: "<delivery-123@pdms.example.org>".into(),
        })
        .await
        .unwrap();

    let body = reqwest::get(format!("http://127.0.0.1:{api}/api/v1/messages"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let m = &v["messages"][0];
    assert_eq!(v["total"], 1, "{body}");
    assert_eq!(
        m["Subject"],
        "[Test] Review due in 30 days: PAG-2026-000001 Facilities Maintenance"
    );
    assert_eq!(m["To"][0]["Address"], "alice@example.org");
    assert_eq!(m["MessageID"], "delivery-123@pdms.example.org");
    let id = m["ID"].as_str().unwrap();
    let headers = reqwest::get(format!(
        "http://127.0.0.1:{api}/api/v1/message/{id}/headers"
    ))
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    assert!(
        headers.contains("Auto-Submitted") && headers.contains("auto-generated"),
        "{headers}"
    );
}
