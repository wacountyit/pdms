// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Health, readiness, metrics, static assets, and the branding theme and seal.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::AppResult;
use crate::state::AppState;

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn readyz(State(state): State<AppState>) -> Response {
    match pdms_core::db::ping(&state.core.db).await {
        Ok(()) => (StatusCode::OK, "ready").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
        }
    }
}

pub async fn metrics(State(state): State<AppState>) -> Response {
    match &state.metrics {
        Some(h) => (
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
            h.render(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

const CSS: &[u8] = include_bytes!("../../static/app.css");
const HTMX: &[u8] = include_bytes!("../../static/htmx.min.js");

pub async fn static_file(Path(name): Path<String>) -> Response {
    let (body, ctype) = match name.as_str() {
        "app.css" => (CSS, "text/css; charset=utf-8"),
        "htmx.min.js" => (HTMX, "text/javascript; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, ctype),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}

const FONT_400: &[u8] = include_bytes!("../../static/fonts/source-sans-3-latin-400.woff2");
const FONT_600: &[u8] = include_bytes!("../../static/fonts/source-sans-3-latin-600.woff2");
const FONT_700: &[u8] = include_bytes!("../../static/fonts/source-sans-3-latin-700.woff2");

/// `GET /static/fonts/{file}`: the embedded Source Sans 3 files (SIL OFL 1.1).
pub async fn static_font(Path(name): Path<String>) -> Response {
    let body = match name.as_str() {
        "source-sans-3-latin-400.woff2" => FONT_400,
        "source-sans-3-latin-600.woff2" => FONT_600,
        "source-sans-3-latin-700.woff2" => FONT_700,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}

/// `GET /theme.css`: the accent family from Administration -> Branding.
pub async fn theme_css(State(state): State<AppState>) -> Response {
    let t = state.branding().theme;
    let css = format!(
        ":root {{ --accent: {}; --accent-hover: {}; --accent-link: {}; --accent-tint: {}; }}\n",
        t.accent, t.hover, t.link, t.tint
    );
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        css,
    )
        .into_response()
}

/// `GET /branding/seal`: the uploaded seal, or 404 when none is set.
pub async fn seal(State(state): State<AppState>) -> AppResult<Response> {
    let Some((ctype, bytes)) = pdms_core::branding::seal_bytes(&state.core.db).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    // The table's CHECK constraint allows only these two types.
    let (ctype, ext) = if ctype == "image/png" {
        ("image/png", "png")
    } else {
        ("image/jpeg", "jpg")
    };
    Ok((
        [
            (header::CONTENT_TYPE, ctype.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"seal.{ext}\""),
            ),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// The license PDMS is distributed under (served as shipped).
const LICENSE: &str = include_str!("../../../../LICENSE");

pub async fn license() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        LICENSE,
    )
        .into_response()
}

/// Where to get this installation's source code (AGPL-3.0 section 13).
pub async fn source(State(state): State<AppState>) -> Response {
    let url = state.core.config.org.source_url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        return axum::response::Redirect::to(url).into_response();
    }
    let org = state
        .org_name()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width, initial-scale=1\"><title>Source code – {org}</title>\
         <link rel=\"stylesheet\" href=\"/static/app.css\"></head><body><main id=\"main\">\
         <h1>Source code</h1>\
         <p>This system runs PDMS, free software licensed under the \
         <a href=\"/license\">GNU Affero General Public License, version 3 or later</a>. \
         You are entitled to the complete source code of the version running here, including \
         any changes made to it.</p>\
         <p>The administrators of {org} have not published a download location yet. Please \
         contact them to receive the source code.</p>\
         <p class=\"muted small\">Administrators: set <code>[org] source_url</code> in \
         <code>config/pdms.toml</code> (or run <code>./install.sh --configure source</code>).</p>\
         </main></body></html>"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}
