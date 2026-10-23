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
    pub can_sharepoint: bool,
    /// Records request packages: records officers build them, auditors review them.
    pub can_exports: bool,
    pub unread: i64,
    /// `/theme.css?v=` (changes with every branding save).
    pub theme_version: String,
    /// `/branding/seal?v=`; `None` shows the generic mark.
    pub seal_version: Option<String>,
    /// Under the user's name in the sidebar; may be empty.
    pub user_department: String,
    /// Shows the top bar's "New document" button.
    pub can_create: bool,
    /// Shows the administration tab row.
    pub admin_tabs: bool,
    /// `dark`, `light` or `system`: the person's color scheme (R2 spec §7),
    /// written on `<html data-theme>`.
    pub color_scheme: String,
    /// This page's path, where the sidebar's color-scheme switch returns.
    pub path: String,
    /// The current administration tab (its path under `/admin/`), marked
    /// with `aria-current`; empty outside the admin pages.
    pub admin_page: &'static str,
}

/// Up to two uppercase initials: the first letters of the first two words,
/// or the first two letters of a single word.
fn initials_of(name: &str) -> String {
    let mut words = name.split_whitespace();
    let letters: String = match (words.next(), words.next()) {
        (Some(a), Some(b)) => a.chars().take(1).chain(b.chars().take(1)).collect(),
        (Some(a), None) => a.chars().take(2).collect(),
        _ => String::new(),
    };
    letters.to_uppercase()
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
        } else if self.can_sharepoint {
            "/admin/sharepoint"
        } else {
            "/admin/tags"
        }
    }

    /// For the initials circle in the sidebar.
    pub fn initials(&self) -> String {
        initials_of(&self.user_name)
    }
}

/// One page in the administration menu.
pub struct AdminLink {
    /// Its path under `/admin/`, also the `Layout::admin_page` key.
    pub key: &'static str,
    pub label: &'static str,
}

impl Layout {
    /// The administration menu in its groups (R2 spec §5.1): only the pages
    /// this person may open, and only groups with at least one page.
    pub fn admin_groups(&self) -> Vec<(&'static str, Vec<AdminLink>)> {
        let link = |key, label| AdminLink { key, label };
        let mut people = Vec::new();
        if self.can_directory {
            people.extend([
                link("users", "Users"),
                link("departments", "Departments"),
                link("groups", "Groups"),
                link("roles", "Roles"),
                link("grants", "Access grants"),
                link("idp", "Identity provider mappings"),
                link("entra", "Entra ID sync"),
            ]);
        }
        let mut records = Vec::new();
        if self.can_schema {
            records.extend([
                link("document-types", "Document types"),
                link("fields", "Custom fields"),
            ]);
        }
        if self.can_tags {
            records.push(link("tags", "Tags"));
        }
        if self.can_retention {
            records.push(link("retention", "Retention"));
        }
        if self.can_restore {
            records.push(link("deleted", "Deleted documents"));
        }
        let mut notices = Vec::new();
        if self.can_schema {
            notices.push(link("notification-rules", "Notification rules"));
        }
        if self.can_settings {
            notices.push(link("notifications", "Delivery log"));
        }
        let mut integrations = Vec::new();
        if self.can_integrations {
            integrations.extend([link("webhooks", "Webhooks"), link("api", "API")]);
        }
        if self.can_sharepoint {
            integrations.push(link("sharepoint", "SharePoint"));
        }
        let mut system = Vec::new();
        if self.can_settings {
            system.push(link("branding", "Branding"));
        }
        if self.can_integrations {
            system.push(link("ocr", "Text extraction (OCR)"));
        }
        [
            ("People and access", people),
            ("Documents and records", records),
            ("Notifications", notices),
            ("Integrations", integrations),
            ("System", system),
        ]
        .into_iter()
        .filter(|g| !g.1.is_empty())
        .collect()
    }

    /// The current admin page's menu label, for the narrow-screen menu.
    pub fn admin_page_label(&self) -> &'static str {
        self.admin_groups()
            .into_iter()
            .flat_map(|g| g.1)
            .find(|l| l.key == self.admin_page)
            .map_or("Menu", |l| l.label)
    }

    /// Marks `page` (its path under `/admin/`) as the current admin tab.
    pub fn admin_tab(mut self, page: &'static str) -> Self {
        self.admin_page = page;
        self
    }

    pub fn new(
        state: &AppState,
        user: &CurrentUser,
        flash: FlashMsg,
        title: &str,
        section: &'static str,
    ) -> Self {
        let branding = state.branding();
        Layout {
            org_name: branding.org_name,
            theme_version: branding.version,
            seal_version: branding.seal_version,
            user_department: user.department_label.clone(),
            can_create: user.can_create,
            admin_tabs: section == "admin",
            admin_page: "",
            color_scheme: user.session.color_scheme.clone(),
            path: user.path.clone(),
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
                Capability::ManageSharePoint,
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
            can_sharepoint: user.can(Capability::ManageSharePoint),
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
        // Unavailable (SharePoint down): nothing changed; say so on the page
        // instead of an error page.
        Err(CoreError::Validation(m) | CoreError::Conflict(m) | CoreError::Unavailable(m)) => {
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

/// 1263 -> "1,263".
pub fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::initials_of;

    #[test]
    fn initials_take_two_words_or_two_letters() {
        assert_eq!(initials_of("Ada Lovelace"), "AL");
        assert_eq!(initials_of("  ada   byron lovelace "), "AB");
        assert_eq!(initials_of("ada@example.org"), "AD");
        assert_eq!(initials_of("Ödön von Horváth"), "ÖV");
        assert_eq!(initials_of("Q"), "Q");
        assert_eq!(initials_of("   "), "");
    }
}
