// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use askama::Template;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use pdms_core::CoreError;

/// Errors from handlers. Internal details are logged, never shown.
#[derive(Debug)]
pub enum AppError {
    Core(CoreError),
    /// Not signed in: redirect to the login page.
    Login {
        return_to: String,
    },
    Csrf,
    RateLimited,
    BadRequest(String),
    Render(askama::Error),
}

impl From<CoreError> for AppError {
    fn from(e: CoreError) -> Self {
        AppError::Core(e)
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Render(e)
    }
}

impl From<pdms_audit::AuditError> for AppError {
    fn from(e: pdms_audit::AuditError) -> Self {
        AppError::Core(e.into())
    }
}

static ORG_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub(crate) fn set_org_name(name: &str) {
    let _ = ORG_NAME.set(name.to_owned());
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    org_name: &'a str,
    status: u16,
    title: &'a str,
    message: &'a str,
}

pub fn error_page(status: StatusCode, title: &str, message: &str) -> Response {
    let page = ErrorPage {
        org_name: ORG_NAME
            .get()
            .map_or(pdms_core::config::DEFAULT_ORG_NAME, String::as_str),
        status: status.as_u16(),
        title,
        message,
    };
    match page.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(_) => (status, title.to_owned()).into_response(),
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Login { return_to } => {
                let target = format!(
                    "/login?next={}",
                    url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect::<String>()
                );
                let mut res = Redirect::to(&target).into_response();
                res.headers_mut().insert(
                    "hx-redirect",
                    header::HeaderValue::from_str(&target)
                        .unwrap_or(header::HeaderValue::from_static("/login")),
                );
                res
            }
            AppError::Csrf => error_page(
                StatusCode::FORBIDDEN,
                "Request blocked",
                "The form was missing a valid security token, or came from another site. Reload the page and try again.",
            ),
            AppError::RateLimited => error_page(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many attempts",
                "Too many sign-in attempts from your network. Wait a minute and try again.",
            ),
            AppError::BadRequest(m) => error_page(StatusCode::BAD_REQUEST, "Bad request", &m),
            AppError::Render(e) => {
                tracing::error!(error = %e, "template rendering failed");
                error_page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "The error has been logged.",
                )
            }
            AppError::Core(e) => match e {
                CoreError::Unauthenticated => AppError::Login {
                    return_to: "/".into(),
                }
                .into_response(),
                CoreError::Forbidden(_) => error_page(
                    StatusCode::FORBIDDEN,
                    "Access denied",
                    "You do not have permission to do that. Ask an administrator if you need access.",
                ),
                CoreError::NotFound => error_page(
                    StatusCode::NOT_FOUND,
                    "Not found",
                    "That page or record does not exist.",
                ),
                CoreError::Validation(m) => {
                    error_page(StatusCode::UNPROCESSABLE_ENTITY, "Check your input", &m)
                }
                CoreError::Conflict(m) => {
                    error_page(StatusCode::CONFLICT, "Cannot complete that", &m)
                }
                other => {
                    tracing::error!(error = %other, "request failed");
                    error_page(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Something went wrong",
                        "The error has been logged.",
                    )
                }
            },
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
