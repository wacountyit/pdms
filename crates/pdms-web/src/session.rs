// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The signed-in user, resolved fresh from the database on every request.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::{Capability, Principal};
use pdms_core::auth::session::SessionUser;
use pdms_core::auth::{principal, session};
use pdms_core::{ClientInfo, CoreError, Ctx};

use crate::error::AppError;
use crate::state::AppState;

pub struct CurrentUser {
    pub session: SessionUser,
    pub ctx: Ctx,
    pub csrf: String,
    /// Unacknowledged in-app notifications (for the navigation badge).
    pub unread: i64,
    /// For the sidebar: the department's name, "N departments" when more
    /// than one, or empty when none.
    pub department_label: String,
    /// May create documents in at least one department (top bar button).
    pub can_create: bool,
    /// This request's path and query, for "return here" links (the
    /// color-scheme switch in the sidebar).
    pub path: String,
}

impl CurrentUser {
    pub fn principal(&self) -> &Principal {
        // Always a user context; constructed below.
        self.ctx
            .principal()
            .unwrap_or_else(|| unreachable!("CurrentUser has a principal"))
    }

    pub fn can(&self, cap: Capability) -> bool {
        self.ctx.can(cap)
    }

    /// Require a capability; a denial is recorded in the audit log.
    pub async fn require(
        &self,
        state: &AppState,
        cap: Capability,
        resource: &str,
    ) -> Result<(), AppError> {
        if self.can(cap) {
            return Ok(());
        }
        let event = AuditEvent::new(
            Category::Access,
            "access.denied",
            self.ctx.actor.clone(),
            "route",
        )
        .outcome(Outcome::Denied)
        .object(resource, None)
        .context("required_capability", format!("{cap:?}"));
        if let Err(e) = pdms_audit::record(&state.core.db, vec![event]).await {
            tracing::error!(error = %e, "failed to record access denial");
        }
        Err(AppError::Core(CoreError::Forbidden(None)))
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // OriginalUri keeps the prefix that nested routers strip.
        let uri = parts
            .extensions
            .get::<axum::extract::OriginalUri>()
            .map_or_else(|| parts.uri.clone(), |o| o.0.clone());
        let return_to = uri
            .path_and_query()
            .map_or("/".to_owned(), |p| p.as_str().to_owned());
        let login = || AppError::Login {
            return_to: pdms_core::auth::oidc::safe_return_to(&return_to),
        };

        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(state.cookies.session)
            .map(|c| c.value().to_owned())
            .ok_or_else(login)?;
        let s = session::lookup(&state.core.db, &state.core.config().auth, &token)
            .await?
            .ok_or_else(login)?;
        let principal = principal::load(&state.core.db, s.user_id).await?;
        if !principal.active {
            return Err(login());
        }
        let client = parts
            .extensions
            .get::<ClientInfo>()
            .cloned()
            .unwrap_or_default();
        let ctx = Ctx::user(principal, &s.display_name, s.auth_method, &client);
        let unread =
            pdms_core::notifications::inbox::unread_count(&state.core.db, s.user_id).await?;
        let departments = sqlx::query_scalar!(
            "SELECT d.name FROM department_members m JOIN departments d ON d.id = m.department_id
              WHERE m.user_id = $1 ORDER BY m.is_primary DESC, d.name",
            s.user_id
        )
        .fetch_all(&state.core.db)
        .await
        .map_err(CoreError::from)?;
        let department_label = match departments.as_slice() {
            [] => String::new(),
            [only] => only.clone(),
            many => format!("{} departments", many.len()),
        };
        let can_create = pdms_core::documents::access::can_create_any(&state.core, &ctx).await?;
        Ok(CurrentUser {
            path: return_to,
            unread,
            department_label,
            can_create,
            csrf: state.core.keys.csrf_token(&token),
            session: s,
            ctx,
        })
    }
}

/// The caller's client info (IP, user agent, request ID).
pub struct Client(pub ClientInfo);

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Client(
            parts
                .extensions
                .get::<ClientInfo>()
                .cloned()
                .unwrap_or_default(),
        ))
    }
}
