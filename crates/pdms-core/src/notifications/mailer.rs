// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Outgoing email. `SmtpMailer` for production (works with Exchange Online and
//! SMTP relays), `FileMailer` for development, `MemoryMailer` for tests.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{EmailConfig, EmailTransport, SmtpSecurity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingEmail {
    pub to_name: String,
    pub to_address: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    /// Deterministic per delivery, so a resend after a crash carries the same ID.
    pub message_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid address: {0}")]
    Address(String),
    #[error("could not build message: {0}")]
    Build(String),
    #[error("SMTP error: {0}")]
    Transport(String),
    #[error("email is disabled")]
    Disabled,
}

pub trait Mailer: Send + Sync + fmt::Debug {
    /// Whether sending is configured; when not, deliveries are skipped, not failed.
    fn enabled(&self) -> bool {
        true
    }
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> BoxFuture<'a, Result<(), MailError>>;
}

fn build(
    from: &Mailbox,
    reply_to: Option<&Mailbox>,
    e: &OutgoingEmail,
) -> Result<Message, MailError> {
    let to: Mailbox = format!(
        "{} <{}>",
        e.to_name.replace(['<', '>', '"'], ""),
        e.to_address
    )
    .parse()
    .or_else(|_| e.to_address.parse())
    .map_err(|_| MailError::Address(e.to_address.clone()))?;
    let mut b = Message::builder()
        .from(from.clone())
        .to(to)
        .subject(&e.subject)
        .message_id(Some(e.message_id.clone()))
        .raw_header(lettre::message::header::HeaderValue::new(
            lettre::message::header::HeaderName::new_from_ascii_str("Auto-Submitted"),
            "auto-generated".to_owned(),
        ));
    if let Some(r) = reply_to {
        b = b.reply_to(r.clone());
    }
    b.multipart(
        MultiPart::alternative()
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(e.text.clone()),
            )
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(e.html.clone()),
            ),
    )
    .map_err(|err| MailError::Build(err.to_string()))
}

pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    reply_to: Option<Mailbox>,
}

impl fmt::Debug for SmtpMailer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpMailer")
            .field("from", &self.from.to_string())
            .finish_non_exhaustive()
    }
}

impl SmtpMailer {
    pub fn new(cfg: &EmailConfig) -> Result<Self, MailError> {
        let builder = match cfg.smtp_security {
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)
            }
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host),
            SmtpSecurity::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &cfg.smtp_host,
            )),
        }
        .map_err(|e| MailError::Transport(e.to_string()))?;
        let mut builder = builder
            .port(cfg.smtp_port)
            .timeout(Some(std::time::Duration::from_secs(30)));
        if !cfg.smtp_username.is_empty() {
            builder = builder.credentials(Credentials::new(
                cfg.smtp_username.clone(),
                cfg.smtp_password.expose().to_owned(),
            ));
        }
        let from = cfg
            .from
            .parse()
            .map_err(|_| MailError::Address(cfg.from.clone()))?;
        let reply_to = (!cfg.reply_to.is_empty())
            .then(|| cfg.reply_to.parse())
            .transpose()
            .map_err(|_| MailError::Address(cfg.reply_to.clone()))?;
        Ok(SmtpMailer {
            transport: builder.build(),
            from,
            reply_to,
        })
    }
}

impl Mailer for SmtpMailer {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let msg = build(&self.from, self.reply_to.as_ref(), email)?;
            self.transport
                .send(msg)
                .await
                .map(|_| ())
                .map_err(|e| MailError::Transport(e.to_string()))
        })
    }
}

/// Writes RFC 5322 messages to a directory (one `.eml` per delivery).
#[derive(Debug)]
pub struct FileMailer {
    pub dir: PathBuf,
    from: Mailbox,
}

impl FileMailer {
    pub fn new(dir: PathBuf, from: &str) -> Result<Self, MailError> {
        let from = if from.is_empty() {
            "PDMS <pdms@localhost>"
        } else {
            from
        };
        Ok(FileMailer {
            dir,
            from: from
                .parse()
                .map_err(|_| MailError::Address(from.to_owned()))?,
        })
    }
}

impl Mailer for FileMailer {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let msg = build(&self.from, None, email)?;
            tokio::fs::create_dir_all(&self.dir)
                .await
                .map_err(|e| MailError::Transport(e.to_string()))?;
            let name: String = email
                .message_id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            tokio::fs::write(self.dir.join(format!("{name}.eml")), msg.formatted())
                .await
                .map_err(|e| MailError::Transport(e.to_string()))
        })
    }
}

/// Email turned off: deliveries are recorded as skipped.
#[derive(Debug)]
pub struct DisabledMailer;

impl Mailer for DisabledMailer {
    fn enabled(&self) -> bool {
        false
    }
    fn send<'a>(&'a self, _email: &'a OutgoingEmail) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Err(MailError::Disabled) })
    }
}

/// Records messages in memory. `fail_next` makes the next N sends fail and
/// `delay` holds each send (both for tests of retries and crashes).
#[derive(Debug, Default, Clone)]
pub struct MemoryMailer {
    pub sent: Arc<Mutex<Vec<OutgoingEmail>>>,
    pub fail_next: Arc<Mutex<u32>>,
    pub delay: Option<std::time::Duration>,
}

impl MemoryMailer {
    pub fn messages(&self) -> Vec<OutgoingEmail> {
        self.sent.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

impl Mailer for MemoryMailer {
    fn send<'a>(&'a self, email: &'a OutgoingEmail) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            if let Some(d) = self.delay {
                tokio::time::sleep(d).await;
            }
            if let Ok(mut n) = self.fail_next.lock()
                && *n > 0
            {
                *n -= 1;
                return Err(MailError::Transport("simulated failure".into()));
            }
            if let Ok(mut v) = self.sent.lock() {
                v.push(email.clone());
            }
            Ok(())
        })
    }
}

/// Build the configured mailer.
pub fn from_config(cfg: &EmailConfig) -> Result<Arc<dyn Mailer>, MailError> {
    Ok(match cfg.transport {
        EmailTransport::Disabled => Arc::new(DisabledMailer),
        EmailTransport::Smtp => Arc::new(SmtpMailer::new(cfg)?),
        EmailTransport::File => Arc::new(FileMailer::new(cfg.file_dir.clone(), &cfg.from)?),
    })
}
