// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → SharePoint: background run status, files changed
//! directly in SharePoint and how to resolve them, files PDMS does not track,
//! and access on department folders that PDMS did not grant.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use pdms_authz::Capability;
use pdms_core::sharepoint::{access, changes, resolve, status};
use uuid::Uuid;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, fmt_time_opt, redirect_with};

const BACK: &str = "/admin/sharepoint";

pub struct RunRow {
    pub label: &'static str,
    pub last_run: String,
    pub error: String,
}

pub struct FlagRow {
    pub version_id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub path: String,
    pub can_view: bool,
    pub version_no: i32,
    pub missing: bool,
    pub changed_by: String,
    pub changed_at: String,
}

pub struct StrayRow {
    pub item_id: String,
    pub name: String,
    pub path: String,
    pub created_by: String,
    pub by_pdms: bool,
    pub first_seen: String,
}

#[derive(Template)]
#[template(path = "admin/sharepoint.html")]
struct SharePointPage {
    l: Layout,
    enabled: bool,
    site_url: String,
    client_id: String,
    runs: Vec<RunRow>,
    dead_jobs: Vec<status::DeadJob>,
    flagged: Vec<FlagRow>,
    unexpected: Vec<StrayRow>,
    foreign: Vec<status::ForeignGrant>,
}

fn run_label(name: &str) -> &'static str {
    match name {
        status::DELTA_CURSOR => "Change check",
        status::RECONCILE_CURSOR => "Department folder access",
        status::ENTRA_CURSOR => "Entra user sync",
        _ => "Other",
    }
}

pub async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    let p = status::panel(&state.core, &user.ctx).await?;
    let page = SharePointPage {
        l: Layout::new(&state, &user, flash, "SharePoint", "admin").admin_tab("sharepoint"),
        enabled: p.enabled,
        site_url: p.site_url,
        client_id: p.client_id,
        runs: p
            .runs
            .into_iter()
            .map(|r| RunRow {
                label: run_label(&r.name),
                last_run: fmt_time_opt(state.tz, r.last_run),
                error: r.last_error.unwrap_or_default(),
            })
            .collect(),
        dead_jobs: p.dead_jobs,
        flagged: p
            .flagged
            .into_iter()
            .map(|f| FlagRow {
                version_id: f.version_id,
                document_id: f.document_id,
                external_id: f.external_id.unwrap_or_default(),
                can_view: f.title.is_some(),
                title: f.title.unwrap_or_default(),
                path: f.path.unwrap_or_default(),
                version_no: f.version_no,
                missing: f.state == "missing",
                changed_by: f.changed_by.unwrap_or_else(|| "unknown".into()),
                changed_at: fmt_time_opt(state.tz, f.changed_at),
            })
            .collect(),
        unexpected: p
            .unexpected
            .into_iter()
            .map(|u| StrayRow {
                item_id: u.item_id,
                name: u.name,
                path: u.path,
                created_by: u.created_by,
                by_pdms: u.by_pdms,
                first_seen: fmt_time(state.tz, u.first_seen),
            })
            .collect(),
        foreign: p.foreign,
    };
    Ok(Html(page.render()?))
}

pub async fn check_now(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    let r = changes::enqueue_now(&state.core.db).await;
    done(
        &state,
        BACK,
        r,
        "Checking SharePoint for changes. Refresh this page in a minute.",
    )
}

pub async fn update_access(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    let r = access::enqueue_reconcile(&state.core.db).await;
    done(
        &state,
        BACK,
        r,
        "Updating department folder access. Refresh this page in a minute.",
    )
}

pub async fn restore(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    let r = resolve::restore_original(&state.core, &user.ctx, version_id).await;
    done(&state, BACK, r, "The original file is back in SharePoint.")
}

pub async fn accept(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    let r = resolve::accept_as_new_version(&state.core, &user.ctx, version_id).await;
    done(
        &state,
        BACK,
        r,
        "The change was added to the document as a new version.",
    )
}

pub async fn recheck(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    match resolve::recheck(&state.core, &user.ctx, version_id).await {
        Ok(true) => Ok(redirect_with(
            &state,
            BACK,
            true,
            "The file matches PDMS's record again.",
        )),
        Ok(false) => Ok(redirect_with(
            &state,
            BACK,
            false,
            "The file still differs from PDMS's record.",
        )),
        Err(e) => done(&state, BACK, Err::<(), _>(e), ""),
    }
}

pub async fn dismiss(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK)
        .await?;
    let r = resolve::dismiss_unexpected(&state.core, &user.ctx, &item_id).await;
    done(&state, BACK, r, "Dismissed.")
}
