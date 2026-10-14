// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Shared page layout, flash messages, and formatting helpers.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{DateTime, Utc};
use pdms_authz::Capability;
use pdms_core::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::session::CurrentUser;
use crate::state::AppState;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flash {
    pub ok: bool,
    pub message: String,
}

/// The flash message for this request, if any (set by the flash middleware).
pub struct FlashMsg(pub Option<Flash>);

impl<S: Send + Sync> FromRequestParts<S> for FlashMsg {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(FlashMsg(parts.extensions.get::<Flash>().cloned()))
    }
}

/// Data every page's base template needs.
pub struct Layout {
    pub org_name: String,
    pub title: String,
    pub user_name: String,
    pub csrf: String,
    pub flash: Option<Flash>,
    pub section: &'static str,
    /// Any administration area is available.
    pub can_admin: bool,
    pub can_directory: bool,
    pub can_schema: bool,
    pub can_tags: bool,
    pub can_retention: bool,
    pub can_restore: bool,
    pub can_audit: bool,
    pub can_settings: bool,
    pub can_integrations: bool,
    /// Records request packages: records officers build them, auditors review them.
    pub can_exports: bool,
    pub unread: i64,
}

impl Layout {
    /// Where the "Administration" link goes: the first area the user can manage.
    pub fn admin_home(&self) -> &'static str {
        if self.can_directory {
            "/admin/users"
        } else if self.can_schema {
            "/admin/document-types"
        } else if self.can_retention {
            "/admin/retention"
        } else if self.can_integrations {
            "/admin/webhooks"
        } else {
            "/admin/tags"
        }
    }
}

impl Layout {
    pub fn new(
        state: &AppState,
        user: &CurrentUser,
        flash: FlashMsg,
        title: &str,
        section: &'static str,
    ) -> Self {
        Layout {
            org_name: state.org_name().to_owned(),
            title: title.to_owned(),
            user_name: user.session.display_name.clone(),
            csrf: user.csrf.clone(),
            flash: flash.0,
            section,
            can_admin: [
                Capability::ManageSettings,
                Capability::ManageDirectory,
                Capability::ManageDocumentSchema,
                Capability::ManageTags,
                Capability::ManageRetention,
                Capability::RestoreDeletedDocuments,
                Capability::ManageIntegrations,
            ]
            .into_iter()
            .any(|c| user.can(c)),
            can_directory: user.can(Capability::ManageDirectory),
            can_schema: user.can(Capability::ManageDocumentSchema),
            can_tags: user.can(Capability::ManageTags),
            can_retention: user.can(Capability::ManageRetention),
            can_restore: user.can(Capability::RestoreDeletedDocuments),
            can_audit: user.can(Capability::ViewAudit),
            can_settings: user.can(Capability::ManageSettings),
            can_integrations: user.can(Capability::ManageIntegrations),
            can_exports: user.can(Capability::BuildExportPackages)
                || user.can(Capability::ViewAudit),
            unread: user.unread,
        }
    }
}

/// Redirect (303) and show `message` on the next page.
pub fn redirect_with(state: &AppState, to: &str, ok: bool, message: &str) -> Response {
    let value = serde_json::to_string(&Flash {
        ok,
        message: message.to_owned(),
    })
    .unwrap_or_default();
    let cookie = Cookie::build((state.cookies.flash, value))
        .path("/")
        .http_only(true)
        .secure(state.secure())
        .same_site(SameSite::Lax)
        .max_age(time_duration(60));
    let jar = PrivateCookieJar::new(state.cookie_key.clone()).add(cookie);
    (jar, Redirect::to(to)).into_response()
}

pub(crate) fn time_duration(secs: i64) -> time::Duration {
    time::Duration::seconds(secs)
}

/// Post/Redirect/Get: on success flash `ok_message`; on a user-correctable
/// error flash the error and go back to the same place.
pub fn done<T>(
    state: &AppState,
    to: &str,
    result: CoreResult<T>,
    ok_message: &str,
) -> AppResult<Response> {
    match result {
        Ok(_) => Ok(redirect_with(state, to, true, ok_message)),
        Err(CoreError::Validation(m) | CoreError::Conflict(m)) => {
            Ok(redirect_with(state, to, false, &m))
        }
        Err(CoreError::NotFound) => Ok(redirect_with(
            state,
            to,
            false,
            "That record no longer exists.",
        )),
        Err(e) => Err(AppError::Core(e)),
    }
}

pub fn fmt_time(tz: chrono_tz::Tz, t: DateTime<Utc>) -> String {
    t.with_timezone(&tz).format("%Y-%m-%d %H:%M %Z").to_string()
}

pub fn fmt_time_opt(tz: chrono_tz::Tz, t: Option<DateTime<Utc>>) -> String {
    t.map_or_else(|| "—".to_owned(), |t| fmt_time(tz, t))
}

pub fn fmt_date(d: Option<chrono::NaiveDate>) -> String {
    d.map_or_else(String::new, |d| d.format("%b %-d, %Y").to_string())
}

pub fn iso_date(d: Option<chrono::NaiveDate>) -> String {
    d.map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string())
}

pub fn human_size(bytes: Option<i64>) -> String {
    let Some(b) = bytes else { return String::new() };
    let b = b as f64;
    if b < 1024.0 {
        format!("{b} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    }
}

/// CSS class for a tag's color (rules served by `/tag-colors.css`).
pub fn tag_class(id: uuid::Uuid) -> String {
    format!("tag tag-{}", id.simple())
}
