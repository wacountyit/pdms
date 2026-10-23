// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Setting email up from Administration → Email (S1): the SMTP settings and a
//! test email sent with what the page holds, before it is saved.

use pdms_authz::Capability;
use serde_json::Value;

use super::mailer::{Mailer, OutgoingEmail, SmtpMailer};
use crate::Core;
use crate::config::{EmailConfig, EmailTransport, Secret, SmtpSecurity};
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::settings::{self, Check};

/// What the Email page saves.
#[derive(Clone, Debug)]
pub struct EmailForm {
    pub on: bool,
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    /// Empty: an unauthenticated relay.
    pub username: String,
    /// Empty keeps the saved password.
    pub password: String,
    pub from: String,
    pub reply_to: String,
}

fn security_key(s: SmtpSecurity) -> &'static str {
    match s {
        SmtpSecurity::Starttls => "starttls",
        SmtpSecurity::Tls => "tls",
        SmtpSecurity::None => "none",
    }
}

/// The email settings `form` would give, keeping what the configuration
/// file sets and the saved password when none was typed.
pub fn candidate(core: &Core, form: &EmailForm) -> EmailConfig {
    let file = core.file_config();
    let open = |key: &str| !file.locked.contains(key);
    let mut cfg = core.config().email.clone();
    if open("email.transport") {
        cfg.transport = if form.on {
            EmailTransport::Smtp
        } else {
            EmailTransport::Disabled
        };
    }
    if open("email.smtp_host") {
        cfg.smtp_host = form.host.trim().to_owned();
    }
    if open("email.smtp_port") {
        cfg.smtp_port = form.port;
    }
    if open("email.smtp_security") {
        cfg.smtp_security = form.security;
    }
    if open("email.smtp_username") {
        cfg.smtp_username = form.username.trim().to_owned();
    }
    if open("email.smtp_password") && !form.password.is_empty() {
        cfg.smtp_password = Secret::new(form.password.clone());
    }
    if open("email.from") {
        cfg.from = form.from.trim().to_owned();
    }
    if open("email.reply_to") {
        cfg.reply_to = form.reply_to.trim().to_owned();
    }
    cfg
}

/// Save `form` (requires `ManageSettings`); the mailer is rebuilt at once.
pub async fn save(core: &Core, ctx: &Ctx, form: &EmailForm) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let cfg = candidate(core, form);
    let current = core.config().email.clone();
    let transport = match cfg.transport {
        EmailTransport::Smtp => "smtp",
        EmailTransport::Disabled => "disabled",
        // Only the configuration file sets "file" (development), and then
        // the key is locked and skipped below.
        EmailTransport::File => "disabled",
    };
    let mut changes: Vec<_> = [
        ("email.transport", Value::String(transport.into())),
        ("email.smtp_host", Value::String(cfg.smtp_host.clone())),
        ("email.smtp_port", Value::from(cfg.smtp_port)),
        (
            "email.smtp_security",
            Value::String(security_key(cfg.smtp_security).into()),
        ),
        (
            "email.smtp_username",
            Value::String(cfg.smtp_username.clone()),
        ),
        ("email.from", Value::String(cfg.from.clone())),
        ("email.reply_to", Value::String(cfg.reply_to.clone())),
    ]
    .into_iter()
    .filter_map(|(key, value)| settings::unless_locked(core, key, value))
    .collect();
    if cfg.smtp_password.expose() != current.smtp_password.expose() {
        changes.extend(settings::unless_locked(
            core,
            "email.smtp_password",
            Value::String(cfg.smtp_password.expose().to_owned()),
        ));
    }
    settings::save(core, ctx, changes).await
}

/// Send one test email to `to` with the settings in `form`, without saving
/// them (requires `ManageSettings`). The audit log records the result, not
/// the address.
pub async fn send_test(core: &Core, ctx: &Ctx, form: &EmailForm, to: &str) -> CoreResult<Check> {
    ctx.require(Capability::ManageSettings)?;
    let to = to.trim();
    if to.is_empty() || !to.contains('@') {
        return Err(CoreError::validation(
            "Enter the address to send the test email to.",
        ));
    }
    let cfg = candidate(core, form);
    let check = try_send(core, &cfg, to).await;
    settings::record_test(core, ctx, "email", check.ok).await?;
    Ok(check)
}

async fn try_send(core: &Core, cfg: &EmailConfig, to: &str) -> Check {
    if cfg.smtp_host.is_empty() {
        return Check::failed(vec![], "Enter the SMTP server.");
    }
    if cfg.from.is_empty() {
        return Check::failed(vec![], "Enter the From address.");
    }
    let mailer = match SmtpMailer::new(cfg) {
        Ok(m) => m,
        Err(e) => return Check::failed(vec![], e.to_string()),
    };
    let org = core.config().org.name.clone();
    let email = OutgoingEmail {
        to_name: String::new(),
        to_address: to.to_owned(),
        subject: format!("{org}: PDMS test email"),
        text: format!(
            "This is a test email from PDMS ({org}). The email settings work: reminders and notices will reach people.\n"
        ),
        html: format!(
            "<p>This is a test email from PDMS ({}). The email settings work: reminders and notices will reach people.</p>",
            html_escape(&org)
        ),
        message_id: format!("<test-{}@pdms>", uuid::Uuid::new_v4()),
    };
    match mailer.send(&email).await {
        Ok(()) => Check::passed(vec![(
            true,
            format!(
                "The server {}:{} accepted the test email to {to}. Check that it arrives, including in the junk folder.",
                cfg.smtp_host, cfg.smtp_port
            ),
        )]),
        Err(e) => Check::failed(
            vec![],
            format!(
                "The test email wasn't sent: {e}. Check the server, port, security and sign-in."
            ),
        ),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
