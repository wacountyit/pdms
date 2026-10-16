// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! API tokens (each user's own, and the administrators' view), webhooks, and
//! the API request log.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::api_tokens::{self, ApiToken, LoggedRequest};
use pdms_core::directory;
use pdms_core::integrations::webhooks::{self, Delivery, Webhook, WebhookSpec};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, fmt_time_opt, redirect_with};

pub struct TokenRow {
    pub id: Uuid,
    pub name: String,
    pub user: String,
    pub prefix: String,
    pub scopes: String,
    pub created: String,
    pub expires: String,
    pub last_used: String,
    pub state: &'static str,
    pub live: bool,
}

fn token_row(state: &AppState, t: ApiToken) -> TokenRow {
    let now = chrono::Utc::now();
    TokenRow {
        id: t.id,
        live: t.is_live(now),
        state: if t.revoked_at.is_some() {
            "Revoked"
        } else if t.expires_at <= now {
            "Expired"
        } else {
            "Active"
        },
        name: t.name,
        user: t.user_name,
        prefix: format!("{}…", t.token_prefix),
        scopes: t.scopes.join(", "),
        created: fmt_time(state.tz, t.created_at),
        expires: fmt_time(state.tz, t.expires_at),
        last_used: fmt_time_opt(state.tz, t.last_used_at),
    }
}

#[derive(Template)]
#[template(path = "account_tokens.html")]
struct TokensPage {
    l: Layout,
    tokens: Vec<TokenRow>,
    scopes: Vec<(&'static str, &'static str)>,
    max_days: i64,
    enabled: bool,
    departments: Vec<String>,
    roles: Vec<String>,
}

pub async fn my_tokens(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let tokens = api_tokens::list_mine(&state.core, &user.ctx).await?;
    let me = directory::get_user(&state.core.db, user.session.user_id).await?;
    let page = TokensPage {
        l: Layout::new(&state, &user, flash, "API tokens", "account"),
        tokens: tokens.into_iter().map(|t| token_row(&state, t)).collect(),
        scopes: api_tokens::SCOPES.to_vec(),
        max_days: state.core.config.api.max_token_days,
        enabled: state.core.config.api.enabled,
        departments: me
            .memberships
            .iter()
            .map(|m| m.department_name.clone())
            .collect(),
        roles: me
            .roles
            .iter()
            .map(|r| match &r.department_name {
                Some(d) => format!("{} ({d})", r.role_name),
                None => r.role_name.clone(),
            })
            .collect(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct TokenForm {
    name: String,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default)]
    days: String,
}

pub async fn create_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<TokenForm>,
) -> AppResult<Response> {
    let days = f.days.trim().parse::<i64>().unwrap_or(90);
    match api_tokens::create(&state.core, &user.ctx, &f.name, &f.scope, days).await {
        Ok((_, secret)) => Ok(redirect_with(
            &state,
            "/account/tokens",
            true,
            &format!("Token created. Copy it now; it is shown only once: {secret}"),
        )),
        Err(e) => done::<()>(&state, "/account/tokens", Err(e), ""),
    }
}

pub async fn revoke_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = api_tokens::revoke(&state.core, &user.ctx, id).await;
    done(
        &state,
        "/account/tokens",
        r,
        "Token revoked. It stops working immediately.",
    )
}

pub async fn admin_revoke_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = api_tokens::revoke(&state.core, &user.ctx, id).await;
    done(
        &state,
        "/admin/api",
        r,
        "Token revoked. It stops working immediately.",
    )
}

#[derive(Template)]
#[template(path = "admin/api.html")]
struct ApiAdminPage {
    l: Layout,
    tokens: Vec<TokenRow>,
    requests: Vec<LoggedRequest>,
    times: Vec<String>,
    enabled: bool,
}

pub async fn admin_api(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageIntegrations, "/admin/api")
        .await?;
    let tokens = api_tokens::list_all(&state.core, &user.ctx).await?;
    let requests = api_tokens::recent_requests(&state.core, &user.ctx, 200).await?;
    let page = ApiAdminPage {
        l: Layout::new(&state, &user, flash, "API tokens and request log", "admin"),
        tokens: tokens.into_iter().map(|t| token_row(&state, t)).collect(),
        times: requests
            .iter()
            .map(|r| fmt_time(state.tz, r.created_at))
            .collect(),
        requests,
        enabled: state.core.config.api.enabled,
    };
    Ok(Html(page.render()?))
}

// ---------------------------------------------------------------------------
// Webhooks
// ---------------------------------------------------------------------------

pub struct DeliveryRow {
    pub id: Uuid,
    pub when: String,
    pub target: String,
    pub event: String,
    pub audit_seq: i64,
    pub status: String,
    pub attempts: i32,
    pub detail: String,
}

fn delivery_row(state: &AppState, d: Delivery) -> DeliveryRow {
    DeliveryRow {
        id: d.id,
        when: fmt_time(state.tz, d.created_at),
        target: d.target,
        event: d.event,
        audit_seq: d.audit_seq,
        detail: match (d.last_status_code, d.last_error) {
            (Some(c), Some(e)) => format!("HTTP {c}: {e}"),
            (Some(c), None) => format!("HTTP {c}"),
            (None, Some(e)) => e,
            (None, None) => String::new(),
        },
        status: d.status,
        attempts: d.attempts,
    }
}

#[derive(Template)]
#[template(path = "admin/webhooks.html")]
struct WebhooksPage {
    l: Layout,
    hooks: Vec<Webhook>,
    deliveries: Vec<DeliveryRow>,
    users: Vec<(Uuid, String)>,
    enabled: bool,
    providers: String,
    back_to: String,
}

async fn active_users(state: &AppState) -> AppResult<Vec<(Uuid, String)>> {
    Ok(directory::list_users(&state.core.db, "", 5000)
        .await?
        .into_iter()
        .filter(|u| u.status == pdms_core::model::UserStatus::Active)
        .map(|u| (u.id, format!("{} ({})", u.display_name, u.email)))
        .collect())
}

pub async fn webhooks_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageIntegrations, "/admin/webhooks")
        .await?;
    let page = WebhooksPage {
        l: Layout::new(&state, &user, flash, "Webhooks", "admin"),
        hooks: webhooks::list(&state.core, &user.ctx).await?,
        deliveries: webhooks::deliveries(&state.core, &user.ctx, None, 100)
            .await?
            .into_iter()
            .map(|d| delivery_row(&state, d))
            .collect(),
        users: active_users(&state).await?,
        enabled: state.core.config.integrations.webhooks_enabled,
        providers: state.core.config.integrations.providers.join(", "),
        back_to: "/admin/webhooks".into(),
    };
    Ok(Html(page.render()?))
}

#[derive(Template)]
#[template(path = "admin/webhook.html")]
struct WebhookPage {
    l: Layout,
    w: Webhook,
    events: String,
    deliveries: Vec<DeliveryRow>,
    users: Vec<(Uuid, String)>,
    back_to: String,
}

pub async fn webhook_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageIntegrations, "/admin/webhooks")
        .await?;
    let w = webhooks::get(&state.core, &user.ctx, id).await?;
    let page = WebhookPage {
        l: Layout::new(
            &state,
            &user,
            flash,
            &format!("Webhook: {}", w.name),
            "admin",
        ),
        events: w.events.join("\n"),
        deliveries: webhooks::deliveries(&state.core, &user.ctx, Some(id), 200)
            .await?
            .into_iter()
            .map(|d| delivery_row(&state, d))
            .collect(),
        users: active_users(&state).await?,
        back_to: format!("/admin/webhooks/{id}"),
        w,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct WebhookForm {
    name: String,
    url: String,
    #[serde(default)]
    events: String,
    acts_as_user_id: String,
    is_active: Option<String>,
}

impl WebhookForm {
    fn spec(self) -> pdms_core::CoreResult<WebhookSpec> {
        Ok(WebhookSpec {
            name: self.name,
            url: self.url,
            events: self
                .events
                .split([',', '\n', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            acts_as_user_id: self
                .acts_as_user_id
                .parse()
                .map_err(|_| pdms_core::CoreError::validation("choose the service user"))?,
            is_active: self.is_active.as_deref() != Some("false"),
        })
    }
}

pub async fn create_webhook(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<WebhookForm>,
) -> AppResult<Response> {
    let r = async { webhooks::create(&state.core, &user.ctx, f.spec()?).await }.await;
    match r {
        Ok((id, secret)) => Ok(redirect_with(
            &state,
            &format!("/admin/webhooks/{id}"),
            true,
            &format!(
                "Webhook created. Its signing secret is shown only once; give it to the receiving system: {secret}"
            ),
        )),
        Err(e) => done::<()>(&state, "/admin/webhooks", Err(e), ""),
    }
}

pub async fn update_webhook(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<WebhookForm>,
) -> AppResult<Response> {
    let r = async { webhooks::update(&state.core, &user.ctx, id, f.spec()?).await }.await;
    done(
        &state,
        &format!("/admin/webhooks/{id}"),
        r,
        "Webhook saved.",
    )
}

pub async fn rotate_webhook(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    match webhooks::rotate_secret(&state.core, &user.ctx, id).await {
        Ok(secret) => Ok(redirect_with(
            &state,
            &format!("/admin/webhooks/{id}"),
            true,
            &format!(
                "New signing secret (shown only once; the old one stops working now): {secret}"
            ),
        )),
        Err(e) => done::<()>(&state, &format!("/admin/webhooks/{id}"), Err(e), ""),
    }
}

#[derive(Deserialize)]
pub struct BackForm {
    #[serde(default)]
    back: String,
}

pub async fn redeliver(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<BackForm>,
) -> AppResult<Response> {
    let back = if f.back.starts_with("/admin/webhooks") && !f.back.contains("//") {
        f.back
    } else {
        "/admin/webhooks".to_owned()
    };
    let r = webhooks::redeliver(&state.core, &user.ctx, id).await;
    done(&state, &back, r, "Queued for delivery.")
}
