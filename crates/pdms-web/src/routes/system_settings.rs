// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → Email and → Virus scanning, and the Settings panel on
//! → Text extraction (OCR) (S1). Each page has a **Test**; a save rebuilds
//! the service at once.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::config::{EmailTransport, SmtpSecurity};
use pdms_core::notifications::mail_setup::{self, EmailForm};
use pdms_core::ocr::setup::{self as ocr_setup, OcrForm};
use pdms_core::scanning::{self, ScanForm};
use pdms_core::settings::Check;
use serde::Deserialize;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done};

fn ticked(v: &Option<String>) -> bool {
    v.as_deref() == Some("true")
}

// ---------------------------------------------------------------------------
// Email
// ---------------------------------------------------------------------------

const EMAIL: &str = "/admin/email";

#[derive(Template)]
#[template(path = "admin/email.html")]
struct EmailPage {
    l: Layout,
    /// Saved: email is sent.
    sending: bool,
    /// The development transport, set in the configuration file.
    file_transport: bool,
    on: bool,
    host: String,
    port: u16,
    security: &'static str,
    username: String,
    password_saved: bool,
    from: String,
    reply_to: String,
    test_to: String,
    locked: Vec<&'static str>,
    check: Option<Check>,
}

impl EmailPage {
    fn is_locked(&self, key: &str) -> bool {
        self.locked.contains(&key)
    }
}

const EMAIL_KEYS: [&str; 8] = [
    "email.transport",
    "email.smtp_host",
    "email.smtp_port",
    "email.smtp_security",
    "email.smtp_username",
    "email.smtp_password",
    "email.from",
    "email.reply_to",
];

fn locked_of(state: &AppState, keys: &[&'static str]) -> Vec<&'static str> {
    let file = state.core.file_config();
    keys.iter()
        .copied()
        .filter(|k| file.locked.contains(*k))
        .collect()
}

#[derive(Deserialize)]
pub struct EmailFields {
    on: Option<String>,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: String,
    #[serde(default)]
    security: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    reply_to: String,
    #[serde(default)]
    test_to: String,
}

impl EmailFields {
    fn form(&self) -> EmailForm {
        EmailForm {
            on: ticked(&self.on),
            host: self.host.clone(),
            port: self.port.trim().parse().unwrap_or(587),
            security: match self.security.as_str() {
                "tls" => SmtpSecurity::Tls,
                "none" => SmtpSecurity::None,
                _ => SmtpSecurity::Starttls,
            },
            username: self.username.clone(),
            password: self.password.clone(),
            from: self.from.clone(),
            reply_to: self.reply_to.clone(),
        }
    }
}

fn security_name(s: SmtpSecurity) -> &'static str {
    match s {
        SmtpSecurity::Starttls => "starttls",
        SmtpSecurity::Tls => "tls",
        SmtpSecurity::None => "none",
    }
}

async fn email_render(
    state: &AppState,
    user: &CurrentUser,
    flash: FlashMsg,
    typed: Option<(EmailForm, String, Check)>,
) -> AppResult<Html<String>> {
    user.require(state, Capability::ManageSettings, EMAIL)
        .await?;
    let e = state.core.config().email.clone();
    let (form, test_to, check) = match typed {
        Some((f, to, c)) => (f, to, Some(c)),
        None => (
            EmailForm {
                on: e.transport == EmailTransport::Smtp,
                host: e.smtp_host.clone(),
                port: e.smtp_port,
                security: e.smtp_security,
                username: e.smtp_username.clone(),
                password: String::new(),
                from: e.from.clone(),
                reply_to: e.reply_to.clone(),
            },
            user.session.email.clone(),
            None,
        ),
    };
    let page = EmailPage {
        l: Layout::new(state, user, flash, "Email", "admin").admin_tab("email"),
        sending: e.transport == EmailTransport::Smtp,
        file_transport: e.transport == EmailTransport::File,
        on: form.on,
        host: form.host,
        port: form.port,
        security: security_name(form.security),
        username: form.username,
        password_saved: !e.smtp_password.is_empty(),
        from: form.from,
        reply_to: form.reply_to,
        test_to,
        locked: locked_of(state, &EMAIL_KEYS),
        check,
    };
    Ok(Html(page.render()?))
}

pub async fn email_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    email_render(&state, &user, flash, None).await
}

pub async fn email_save(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<EmailFields>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, EMAIL)
        .await?;
    let r = mail_setup::save(&state.core, &user.ctx, &f.form()).await;
    done(
        &state,
        EMAIL,
        r,
        "Saved. Email uses these settings from now on.",
    )
}

/// Send a test email with the form's values, without saving them.
pub async fn email_test(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<EmailFields>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, EMAIL)
        .await?;
    let mut form = f.form();
    let check = match mail_setup::send_test(&state.core, &user.ctx, &form, &f.test_to).await {
        Ok(c) => c,
        Err(pdms_core::CoreError::Validation(m)) => Check::failed(vec![], m),
        Err(e) => return Err(e.into()),
    };
    form.password.clear();
    Ok(email_render(
        &state,
        &user,
        FlashMsg(None),
        Some((form, f.test_to, check)),
    )
    .await?
    .into_response())
}

// ---------------------------------------------------------------------------
// Virus scanning
// ---------------------------------------------------------------------------

const SCAN: &str = "/admin/virus-scanning";

#[derive(Template)]
#[template(path = "admin/virus_scanning.html")]
struct ScanPage {
    l: Layout,
    on: bool,
    addr: String,
    required: bool,
    addr_locked: bool,
    required_locked: bool,
    check: Option<Check>,
}

async fn scan_render(
    state: &AppState,
    user: &CurrentUser,
    flash: FlashMsg,
    check: Option<Check>,
) -> AppResult<Html<String>> {
    user.require(state, Capability::ManageSettings, SCAN)
        .await?;
    let s = &state.core.config().storage;
    let file = state.core.file_config();
    let page = ScanPage {
        l: Layout::new(state, user, flash, "Virus scanning", "admin").admin_tab("virus-scanning"),
        on: !s.clamav_addr.is_empty(),
        addr: if s.clamav_addr.is_empty() {
            scanning::DEFAULT_ADDR.to_owned()
        } else {
            s.clamav_addr.clone()
        },
        required: s.clamav_required,
        addr_locked: file.locked.contains("storage.clamav_addr"),
        required_locked: file.locked.contains("storage.clamav_required"),
        check,
    };
    Ok(Html(page.render()?))
}

pub async fn scan_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    scan_render(&state, &user, flash, None).await
}

#[derive(Deserialize)]
pub struct ScanFields {
    on: Option<String>,
    required: Option<String>,
}

pub async fn scan_save(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<ScanFields>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, SCAN)
        .await?;
    let form = ScanForm {
        on: ticked(&f.on),
        required: ticked(&f.required),
    };
    let r = scanning::save(&state.core, &user.ctx, form).await;
    done(
        &state,
        SCAN,
        r,
        "Saved. Uploads use these settings from now on.",
    )
}

pub async fn scan_test(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, SCAN)
        .await?;
    let check = scanning::test(&state.core, &user.ctx).await?;
    Ok(scan_render(&state, &user, FlashMsg(None), Some(check))
        .await?
        .into_response())
}

// ---------------------------------------------------------------------------
// Text extraction (OCR): the Settings panel
// ---------------------------------------------------------------------------

const OCR: &str = "/admin/ocr";

#[derive(Deserialize)]
pub struct OcrFields {
    text_extraction: Option<String>,
    ocr: Option<String>,
    suggest_dates: Option<String>,
}

pub async fn ocr_save(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<OcrFields>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, OCR)
        .await?;
    let form = OcrForm {
        text_extraction: ticked(&f.text_extraction),
        ocr: ticked(&f.ocr),
        suggest_dates: ticked(&f.suggest_dates),
    };
    let r = ocr_setup::save(&state.core, &user.ctx, form).await;
    done(
        &state,
        OCR,
        r,
        "Saved. Files uploaded from now on are read with these settings.",
    )
}

pub async fn ocr_test(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, OCR)
        .await?;
    let check = ocr_setup::test(&state.core, &user.ctx).await?;
    Ok(
        crate::routes::collab::ocr_render(&state, &user, FlashMsg(None), Some(check))
            .await?
            .into_response(),
    )
}
