// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → Microsoft 365 → Connection: the "PDMS Service" app
//! registration (tenant and client IDs) and its certificate, made in PDMS
//! (S1, D-113). The Entra ID user sync and SharePoint storage use it.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::CoreError;
use pdms_core::graph::certs::{self, CertificateInfo};
use pdms_core::settings::{self, Change, NewValue};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, redirect_with};

const PATH: &str = "/admin/microsoft-365";

pub struct CertRow {
    pub id: Uuid,
    pub active: bool,
    pub sha1: String,
    pub sha256: String,
    pub expires: String,
    pub created: String,
}

#[derive(Template)]
#[template(path = "admin/microsoft365.html")]
struct ConnectionPage {
    l: Layout,
    tenant_id: String,
    client_id: String,
    tenant_locked: bool,
    client_locked: bool,
    /// A certificate in the configuration file (`secrets/graph_certificate`) wins.
    file_certificate: bool,
    connected: bool,
    thumbprint: String,
    certs: Vec<CertRow>,
}

fn row(state: &AppState, c: CertificateInfo) -> CertRow {
    CertRow {
        id: c.id,
        active: c.state == "active",
        sha1: c.thumbprint_sha1,
        sha256: c.thumbprint_sha256,
        expires: c.not_after.format("%Y-%m-%d").to_string(),
        created: fmt_time(state.tz, c.created_at),
    }
}

pub async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let config = state.core.config();
    let file = state.core.file_config();
    let graph = state.core.graph();
    let certs: Vec<CertRow> = certs::list(&state.core.db)
        .await?
        .into_iter()
        .map(|c| row(&state, c))
        .collect();
    let page = ConnectionPage {
        l: Layout::new(&state, &user, flash, "Microsoft 365 connection", "admin")
            .admin_tab("microsoft-365"),
        tenant_id: config.graph.tenant_id.clone(),
        client_id: config.graph.client_id.clone(),
        tenant_locked: file.locked.contains("graph.tenant_id"),
        client_locked: file.locked.contains("graph.client_id"),
        file_certificate: !file.graph.certificate.is_empty(),
        connected: graph.is_some(),
        thumbprint: graph.map(|g| g.thumbprint().to_owned()).unwrap_or_default(),
        certs,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct IdsForm {
    #[serde(default)]
    tenant_id: String,
    #[serde(default)]
    client_id: String,
}

/// `01234567-89ab-cdef-0123-456789abcdef`.
fn is_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

pub async fn save(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<IdsForm>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let file = state.core.file_config();
    let (tenant, client) = (f.tenant_id.trim(), f.client_id.trim());
    let mut changes = Vec::new();
    for (key, value) in [("graph.tenant_id", tenant), ("graph.client_id", client)] {
        if file.locked.contains(key) {
            continue;
        }
        if !value.is_empty() && !is_guid(value) {
            return Ok(redirect_with(
                &state,
                PATH,
                false,
                "The tenant and client IDs look like 01234567-89ab-cdef-0123-456789abcdef.",
            ));
        }
        changes.push(Change {
            key: key.into(),
            value: if value.is_empty() {
                NewValue::Clear
            } else {
                NewValue::Set(serde_json::Value::String(value.to_owned()))
            },
        });
    }
    let no_certificate = file.graph.certificate.is_empty()
        && certs::list(&state.core.db).await?.is_empty()
        && !tenant.is_empty()
        && !client.is_empty();
    if no_certificate {
        return Ok(redirect_with(
            &state,
            PATH,
            false,
            "Generate a certificate first: PDMS needs it to sign in.",
        ));
    }
    let r = settings::save(&state.core, &user.ctx, changes).await;
    done(
        &state,
        PATH,
        r,
        "Saved. PDMS uses the Microsoft 365 connection from now on.",
    )
}

pub async fn test(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<IdsForm>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let config = state.core.config();
    let pick = |typed: &str, saved: &str| {
        if typed.trim().is_empty() {
            saved.to_owned()
        } else {
            typed.trim().to_owned()
        }
    };
    let tenant = pick(&f.tenant_id, &config.graph.tenant_id);
    let client = pick(&f.client_id, &config.graph.client_id);
    let outcome = certs::test_connection(&state.core, &user.ctx, &tenant, &client).await?;
    Ok(redirect_with(
        &state,
        PATH,
        outcome.ok,
        &outcome.lines.join(" "),
    ))
}

pub async fn generate(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let r = certs::generate(&state.core, &user.ctx).await;
    done(
        &state,
        PATH,
        r,
        "Certificate made. Download it and upload it to the app registration.",
    )
}

pub async fn activate(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let r = certs::activate(&state.core, &user.ctx, id).await;
    done(
        &state,
        PATH,
        r,
        "PDMS signs in with the new certificate from now on.",
    )
}

pub async fn retire(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let r = certs::retire(&state.core, &user.ctx, id).await;
    done(&state, PATH, r, "Certificate discarded.")
}

/// The public certificate, to upload to the app registration.
pub async fn download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(file): Path<String>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, PATH)
        .await?;
    let id: Uuid = file
        .strip_suffix(".crt")
        .and_then(|i| i.parse().ok())
        .ok_or(CoreError::NotFound)?;
    let pem = certs::public_certificate(&state.core, &user.ctx, id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-pem-file".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"pdms-service.crt\"".to_owned(),
            ),
        ],
        pem,
    )
        .into_response())
}
