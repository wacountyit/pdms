// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → Microsoft 365 → Sign-in: Microsoft sign-in for staff
//! (S1, D-115). Saving rebuilds the sign-in provider at once, without a
//! restart.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::auth::oidc_setup::{self, SignInForm, SignInTest};
use serde::Deserialize;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done};

const PATH: &str = "/admin/sign-in";

#[derive(Template)]
#[template(path = "admin/sign_in.html")]
struct SignInPage {
    l: Layout,
    enabled: bool,
    /// The form's "on" box (what a test tried, or the saved value).
    on: bool,
    /// Working: on, and the provider was built.
    working: bool,
    error: String,
    tenant: String,
    client_id: String,
    secret_saved: bool,
    display_name: String,
    auto_provision: bool,
    redirect_uri: String,
    local_enabled: bool,
    enabled_locked: bool,
    issuer_locked: bool,
    client_locked: bool,
    secret_locked: bool,
    display_locked: bool,
    provision_locked: bool,
    test: Option<SignInTest>,
}

#[derive(Deserialize)]
pub struct Fields {
    enabled: Option<String>,
    #[serde(default)]
    tenant: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    display_name: String,
    auto_provision: Option<String>,
}

impl Fields {
    fn form(&self) -> SignInForm {
        SignInForm {
            enabled: self.enabled.as_deref() == Some("true"),
            tenant: self.tenant.clone(),
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            display_name: self.display_name.clone(),
            auto_provision: self.auto_provision.as_deref() == Some("true"),
        }
    }
}

pub async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    render(&state, &user, flash, None).await
}

/// `typed`: what a **Test** tried (the secret is never sent back), with its result.
async fn render(
    state: &AppState,
    user: &CurrentUser,
    flash: FlashMsg,
    typed: Option<(SignInForm, SignInTest)>,
) -> AppResult<Html<String>> {
    user.require(state, Capability::ManageSettings, PATH)
        .await?;
    let config = state.core.config();
    let file = state.core.file_config();
    let o = &config.auth.oidc;
    let saved_tenant =
        oidc_setup::tenant_of(&o.issuer_url).map_or_else(|| o.issuer_url.clone(), str::to_owned);
    let (form, test) = match typed {
        Some((f, t)) => (f, Some(t)),
        None => (
            SignInForm {
                enabled: o.enabled,
                tenant: saved_tenant,
                client_id: o.client_id.clone(),
                client_secret: String::new(),
                display_name: o.display_name.clone(),
                auto_provision: o.auto_provision,
            },
            None,
        ),
    };
    let page = SignInPage {
        l: Layout::new(state, user, flash, "Microsoft sign-in", "admin").admin_tab("sign-in"),
        enabled: o.enabled,
        on: form.enabled,
        working: state.oidc().is_some(),
        error: state.oidc_error().unwrap_or_default(),
        tenant: form.tenant,
        client_id: form.client_id,
        secret_saved: !o.client_secret.is_empty(),
        display_name: form.display_name,
        auto_provision: form.auto_provision,
        redirect_uri: oidc_setup::redirect_uri(&state.core),
        local_enabled: config.auth.local.enabled,
        enabled_locked: file.locked.contains("auth.oidc.enabled"),
        issuer_locked: file.locked.contains("auth.oidc.issuer_url"),
        client_locked: file.locked.contains("auth.oidc.client_id"),
        secret_locked: file.locked.contains("auth.oidc.client_secret"),
        display_locked: file.locked.contains("auth.oidc.display_name"),
        provision_locked: file.locked.contains("auth.oidc.auto_provision"),
        test,
    };
    Ok(Html(page.render()?))
}

pub async fn save(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<Fields>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let r = oidc_setup::save(&state.core, &user.ctx, &f.form()).await;
    if r.is_ok() {
        // This process at once; the other one through the settings notice.
        state.refresh_oidc(false).await;
        if let Some(e) = state.oidc_error() {
            return done(
                &state,
                PATH,
                Err::<(), _>(pdms_core::CoreError::Conflict(format!(
                    "Saved, but Microsoft sign-in is unavailable: {e}"
                ))),
                "",
            );
        }
    }
    done(
        &state,
        PATH,
        r,
        "Saved. The change applies to the next sign-in.",
    )
}

pub async fn test(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<Fields>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let mut form = f.form();
    let t = oidc_setup::test(&state.core, &user.ctx, &form).await?;
    form.client_secret.clear();
    Ok(render(&state, &user, FlashMsg(None), Some((form, t)))
        .await?
        .into_response())
}
