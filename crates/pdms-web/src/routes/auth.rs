// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::net::{IpAddr, Ipv4Addr};

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::{CookieJar, PrivateCookieJar};
use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_core::auth::local::{self, LoginError, LoginFailure};
use pdms_core::auth::oidc::{FlowState, safe_return_to};
use pdms_core::auth::session;
use pdms_core::crypto::new_token;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::error::{AppError, AppResult};
use crate::session::{Client, CurrentUser};
use crate::state::AppState;
use crate::views::time_duration;

/// Contents of the encrypted login cookie.
#[derive(Serialize, Deserialize, Default)]
struct LoginCookie {
    csrf: String,
    oidc: Option<FlowState>,
}

fn read_login_cookie(state: &AppState, jar: &PrivateCookieJar) -> LoginCookie {
    jar.get(state.cookies.login)
        .and_then(|c| serde_json::from_str(c.value()).ok())
        .unwrap_or_default()
}

fn login_cookie(state: &AppState, value: &LoginCookie) -> Cookie<'static> {
    Cookie::build((
        state.cookies.login,
        serde_json::to_string(value).unwrap_or_default(),
    ))
    .path("/")
    .http_only(true)
    .secure(state.secure())
    // Lax so the cookie comes back on the IdP's top-level redirect.
    .same_site(SameSite::Lax)
    .max_age(time_duration(600))
    .build()
}

/// A removal cookie with the same attributes the original was set with
/// (browsers ignore `__Host-` cookies without `Secure`).
pub fn removal(state: &AppState, name: &'static str) -> Cookie<'static> {
    Cookie::build(name)
        .path("/")
        .secure(state.secure())
        .http_only(true)
        .build()
}

pub fn session_cookie(state: &AppState, token: String) -> Cookie<'static> {
    Cookie::build((state.cookies.session, token))
        .path("/")
        .http_only(true)
        .secure(state.secure())
        .same_site(SameSite::Lax)
        .max_age(time_duration(
            state.core.config().auth.session_absolute_hours * 3600,
        ))
        .build()
}

fn rate_limit(state: &AppState, client: &pdms_core::ClientInfo) -> AppResult<()> {
    let key = client.ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    state
        .login_limiter
        .check_key(&key)
        .map_err(|_| AppError::RateLimited)
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage<'a> {
    org_name: &'a str,
    theme_version: &'a str,
    seal_version: Option<&'a str>,
    csrf: &'a str,
    next: &'a str,
    next_encoded: String,
    local_enabled: bool,
    sso_label: &'a str,
    totp_required: bool,
    error: Option<&'a str>,
    notice: Option<&'static str>,
}

#[derive(Deserialize)]
pub struct NextQuery {
    next: Option<String>,
    #[serde(default)]
    notice: String,
}

/// The fixed text for a `notice` code; unknown codes show nothing, and the
/// request's own text is never echoed.
fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "sso_off" => Some("Microsoft sign-in isn't set up yet. Contact your administrator."),
        _ => None,
    }
}

fn render_login(
    state: &AppState,
    jar: PrivateCookieJar,
    next: &str,
    error: Option<&str>,
    notice: Option<&'static str>,
    status: StatusCode,
) -> AppResult<Response> {
    let mut cookie = read_login_cookie(state, &jar);
    if cookie.csrf.is_empty() {
        cookie.csrf = new_token();
    }
    let next = safe_return_to(next);
    let branding = state.branding();
    let sso_label = state.core.config().auth.oidc.display_name.clone();
    let page = LoginPage {
        org_name: &branding.org_name,
        theme_version: &branding.version,
        seal_version: branding.seal_version.as_deref(),
        csrf: &cookie.csrf,
        next: &next,
        next_encoded: url::form_urlencoded::byte_serialize(next.as_bytes()).collect(),
        local_enabled: state.core.config().auth.local.enabled,
        sso_label: &sso_label,
        totp_required: state.core.config().auth.local.require_totp,
        error,
        notice,
    };
    let html = page.render()?;
    let jar = jar.add(login_cookie(state, &cookie));
    Ok((status, jar, Html(html)).into_response())
}

pub async fn login_page(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    plain: CookieJar,
    Query(q): Query<NextQuery>,
) -> AppResult<Response> {
    let next = q.next.unwrap_or_else(|| "/".into());
    // Already signed in? Go straight on.
    if let Some(token) = plain.get(state.cookies.session)
        && session::lookup(&state.core.db, &state.core.config().auth, token.value())
            .await?
            .is_some()
    {
        return Ok(Redirect::to(&safe_return_to(&next)).into_response());
    }
    render_login(
        &state,
        jar,
        &next,
        None,
        notice_text(&q.notice),
        StatusCode::OK,
    )
}

#[derive(Deserialize)]
pub struct LocalLoginForm {
    username: String,
    password: String,
    #[serde(default)]
    code: String,
    #[serde(rename = "_csrf")]
    csrf: String,
    #[serde(default)]
    next: String,
}

pub async fn login_local(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Client(client): Client,
    Form(form): Form<LocalLoginForm>,
) -> AppResult<Response> {
    let expected = read_login_cookie(&state, &jar).csrf;
    if expected.is_empty() || !bool::from(expected.as_bytes().ct_eq(form.csrf.as_bytes())) {
        return Err(AppError::Csrf);
    }
    rate_limit(&state, &client)?;
    match local::login(
        &state.core,
        &form.username,
        &form.password,
        &form.code,
        &client,
    )
    .await
    {
        Ok(ok) => {
            let jar = jar.remove(removal(&state, state.cookies.login));
            let plain = CookieJar::new().add(session_cookie(&state, ok.token));
            Ok((jar, plain, Redirect::to(&safe_return_to(&form.next))).into_response())
        }
        Err(LoginError::Failed(reason)) => {
            // One message for every reason, so nothing reveals which part was wrong.
            let msg = if reason == LoginFailure::Disabled {
                "Emergency sign-in is disabled."
            } else if state.core.config().auth.local.require_totp {
                "Username, password, or authenticator code is incorrect."
            } else {
                "Username or password is incorrect."
            };
            render_login(
                &state,
                jar,
                &form.next,
                Some(msg),
                None,
                StatusCode::UNAUTHORIZED,
            )
        }
        Err(LoginError::Core(e)) => Err(e.into()),
    }
}

pub async fn oidc_start(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Client(client): Client,
    Query(q): Query<NextQuery>,
) -> AppResult<Response> {
    let Some(oidc) = state.oidc() else {
        // The button is always shown; explain instead of a 404, and keep
        // where the person was going (only a safe local path survives).
        let next = safe_return_to(q.next.as_deref().unwrap_or("/"));
        let next: String = url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
        return Ok(Redirect::to(&format!("/login?notice=sso_off&next={next}")).into_response());
    };
    rate_limit(&state, &client)?;
    let (url, flow) = oidc.begin(q.next.as_deref().unwrap_or("/"));
    let mut cookie = read_login_cookie(&state, &jar);
    cookie.oidc = Some(flow);
    let jar = jar.add(login_cookie(&state, &cookie));
    Ok((jar, Redirect::to(url.as_str())).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Client(client): Client,
    Query(q): Query<CallbackQuery>,
) -> AppResult<Response> {
    let Some(oidc) = state.oidc() else {
        return Err(pdms_core::CoreError::NotFound.into());
    };
    let mut cookie = read_login_cookie(&state, &jar);
    let flow = cookie.oidc.take();
    let jar = jar.add(login_cookie(&state, &cookie));
    let failed = |jar| {
        render_login(
            &state,
            jar,
            "/",
            Some("Single sign-on failed. Try again, or contact your administrator."),
            None,
            StatusCode::UNAUTHORIZED,
        )
    };

    if let Some(err) = q.error {
        tracing::warn!(error = %err, description = ?q.error_description, "IdP returned an error");
        let actor = pdms_audit::Actor {
            user_id: None,
            display: None,
            auth_method: pdms_audit::AuthMethod::Oidc,
            ip: client.ip,
            user_agent: client.user_agent.clone(),
            api_token_id: None,
            request_id: client.request_id.clone(),
        };
        pdms_audit::record(
            &state.core.db,
            vec![
                AuditEvent::new(Category::Auth, "auth.login_failed", actor, "user")
                    .outcome(Outcome::Failure)
                    .context("reason", "idp_error")
                    .context("detail", err.chars().take(200).collect::<String>()),
            ],
        )
        .await?;
        return failed(jar);
    }
    let (Some(flow), Some(code), Some(returned_state)) = (flow, q.code, q.state) else {
        return failed(jar);
    };
    let return_to = flow.return_to.clone();
    match oidc
        .complete(&state.core, flow, &returned_state, &code, &client)
        .await
    {
        Ok(ok) => {
            let jar = jar.remove(removal(&state, state.cookies.login));
            let plain = CookieJar::new().add(session_cookie(&state, ok.token));
            Ok((jar, plain, Redirect::to(&return_to)).into_response())
        }
        Err(pdms_core::CoreError::Unauthenticated | pdms_core::CoreError::Forbidden(_)) => {
            failed(jar)
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn logout(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    let mut tx = state
        .core
        .db
        .begin()
        .await
        .map_err(pdms_core::CoreError::from)?;
    session::revoke(&mut tx, user.session.session_id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Auth,
                "auth.logout",
                user.ctx.actor.clone(),
                "user",
            )
            .object(user.session.user_id, Some(&user.session.display_name)),
        ],
    )
    .await?;
    tx.commit().await.map_err(pdms_core::CoreError::from)?;
    let jar = CookieJar::new().remove(removal(&state, state.cookies.session));
    Ok((jar, Redirect::to("/login")).into_response())
}
