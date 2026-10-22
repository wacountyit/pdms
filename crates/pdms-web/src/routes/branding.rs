// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → Branding: county name, accent color and seal.

use askama::Template;
use axum::extract::{Extension, Multipart, State};
use axum::response::{Html, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::branding::{self, seal};
use pdms_core::{CoreError, CoreResult};
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::error::{AppError, AppResult};
use crate::middleware::DeferredCsrf;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::upload;
use crate::views::{FlashMsg, Layout, redirect_with};

const PATH: &str = "/admin/branding";

#[derive(Template)]
#[template(path = "admin/branding.html")]
struct BrandingPage {
    l: Layout,
    org_name: String,
    config_name: String,
    accent: String,
    seal_version: Option<String>,
    seal_info: Option<(i32, i32)>,
    confirm_reset: bool,
}

async fn page(
    state: &AppState,
    user: &CurrentUser,
    flash: FlashMsg,
    confirm_reset: bool,
) -> AppResult<Html<String>> {
    user.require(state, Capability::ManageSettings, PATH)
        .await?;
    let b = branding::load(&state.core.db).await?;
    let title = if confirm_reset {
        "Reset branding"
    } else {
        "Branding"
    };
    let page = BrandingPage {
        l: Layout::new(state, user, flash, title, "admin").admin_tab("branding"),
        org_name: b.org_name.clone().unwrap_or_default(),
        config_name: state.core.config.org.name.clone(),
        accent: b.accent_or_default().to_owned(),
        seal_version: state.branding().seal_version,
        seal_info: b.seal.as_ref().map(|s| (s.width, s.height)),
        confirm_reset,
    };
    Ok(Html(page.render()?))
}

pub async fn show(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    page(&state, &user, flash, false).await
}

pub async fn reset_confirm(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    page(&state, &user, flash, true).await
}

/// Like `views::done`, but refreshes the in-memory branding before redirecting.
async fn finish<T>(
    state: &AppState,
    result: CoreResult<T>,
    ok_message: &str,
) -> AppResult<Response> {
    match result {
        Ok(_) => {
            state.reload_branding().await;
            Ok(redirect_with(state, PATH, true, ok_message))
        }
        Err(CoreError::Validation(m)) => Ok(redirect_with(state, PATH, false, &m)),
        Err(e) => Err(AppError::Core(e)),
    }
}

#[derive(Deserialize)]
pub struct SaveForm {
    #[serde(default)]
    org_name: String,
    #[serde(default)]
    accent: String,
    #[serde(default)]
    accent_text: String,
}

pub async fn save(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<SaveForm>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let typed = f.accent_text.trim();
    let accent = if typed.is_empty() {
        f.accent.trim()
    } else {
        typed
    };
    let r = branding::update(&state.core, &user.ctx, &f.org_name, accent).await;
    finish(&state, r, "Branding saved.").await
}

pub async fn upload_seal(
    State(state): State<AppState>,
    user: CurrentUser,
    deferred: Option<Extension<DeferredCsrf>>,
    multipart: Multipart,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let form = upload::read(&state, deferred.map(|e| e.0), multipart).await?;
    let Some(up) = form.file else {
        return Ok(redirect_with(
            &state,
            PATH,
            false,
            "Choose an image to upload.",
        ));
    };
    // Read at most one byte over the cap; the sniffer reports "too large".
    let mut bytes = Vec::new();
    let file = tokio::fs::File::open(&up.file.path)
        .await
        .map_err(|e| AppError::Core(CoreError::internal(e)))?;
    file.take(seal::MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| AppError::Core(CoreError::internal(e)))?;
    let r = branding::set_seal(&state.core, &user.ctx, &bytes).await;
    finish(&state, r, "Seal uploaded.").await
}

pub async fn remove_seal(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let r = branding::remove_seal(&state.core, &user.ctx).await;
    finish(&state, r, "Seal removed.").await
}

pub async fn reset(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let r = branding::reset(&state.core, &user.ctx).await;
    finish(&state, r, "Branding reset to defaults.").await
}
