// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → SharePoint: background run status, files changed
//! directly in SharePoint and how to resolve them, files PDMS does not track,
//! and access on department folders that PDMS did not grant. The Settings
//! panel (S1, D-114) sets the site up and turns storage on and off.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::sharepoint::{Probe, access, changes, resolve, setup, status};
use serde::Deserialize;
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
    /// The Settings panel, for people who can manage settings.
    settings: Option<SettingsPanel>,
}

pub struct SettingsPanel {
    pub site_url: String,
    pub library: String,
    pub interval: u64,
    pub site_locked: bool,
    pub library_locked: bool,
    pub interval_locked: bool,
    pub backend_locked: bool,
    pub storage: setup::StorageState,
    /// The result of **Test**, shown under the form.
    pub probe: Option<Probe>,
}

impl SettingsPanel {
    /// What to say under a passing **Test**: the next step, if any.
    pub fn ready_note(&self) -> &'static str {
        if self.storage.on {
            "The site is ready."
        } else if self.backend_locked {
            "The site is ready. Save it."
        } else {
            "The site is ready. Save it, then turn storage on."
        }
    }
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
    render(&state, &user, flash, None).await
}

/// `typed`: the site and library a **Test** tried, with its result.
async fn render(
    state: &AppState,
    user: &CurrentUser,
    flash: FlashMsg,
    typed: Option<(SettingsForm, Probe)>,
) -> AppResult<Html<String>> {
    user.require(state, Capability::ManageSharePoint, BACK)
        .await?;
    let p = status::panel(&state.core, &user.ctx).await?;
    let settings = if user.can(Capability::ManageSettings) {
        let config = state.core.config();
        let file = state.core.file_config();
        let (form, probe) = match typed {
            Some((f, probe)) => (f, Some(probe)),
            None => (
                SettingsForm {
                    site_url: config.sharepoint.site_url.clone(),
                    library: config.sharepoint.library.clone(),
                    interval: config.sharepoint.delta_interval_minutes,
                },
                None,
            ),
        };
        // A locked field always shows the configuration file's value: the
        // browser doesn't send a disabled field back with a Test.
        let site_locked = file.locked.contains("sharepoint.site_url");
        let library_locked = file.locked.contains("sharepoint.library");
        let interval_locked = file.locked.contains("sharepoint.delta_interval_minutes");
        let sp = &config.sharepoint;
        Some(SettingsPanel {
            site_url: if site_locked {
                sp.site_url.clone()
            } else {
                form.site_url
            },
            library: if library_locked {
                sp.library.clone()
            } else {
                form.library
            },
            interval: if interval_locked {
                sp.delta_interval_minutes
            } else {
                form.interval
            },
            site_locked,
            library_locked,
            interval_locked,
            backend_locked: file.locked.contains("storage.backend"),
            storage: setup::state(&state.core).await?,
            probe,
        })
    } else {
        None
    };
    let page = SharePointPage {
        l: Layout::new(state, user, flash, "SharePoint", "admin").admin_tab("sharepoint"),
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
        settings,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct SettingsForm {
    #[serde(default)]
    site_url: String,
    #[serde(default)]
    library: String,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    15
}

pub async fn save_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<SettingsForm>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, BACK)
        .await?;
    let r = setup::save_site(&state.core, &user.ctx, &f.site_url, &f.library, f.interval).await;
    done(&state, BACK, r, "Saved.")
}

/// Check the typed site and library without saving them, and show each step.
pub async fn test(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<SettingsForm>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, BACK)
        .await?;
    let probe = setup::test(&state.core, &user.ctx, &f.site_url, &f.library).await?;
    Ok(render(&state, &user, FlashMsg(None), Some((f, probe)))
        .await?
        .into_response())
}

pub async fn turn_on(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, BACK)
        .await?;
    let r = setup::turn_on(&state.core, &user.ctx).await;
    done(
        &state,
        BACK,
        r,
        "SharePoint storage is on. New document files are stored in SharePoint from now on.",
    )
}

#[derive(Deserialize)]
pub struct TurnOffForm {
    #[serde(default)]
    confirm_site: String,
}

pub async fn turn_off(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<TurnOffForm>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSettings, BACK)
        .await?;
    let r = setup::turn_off(&state.core, &user.ctx, &f.confirm_site).await;
    done(
        &state,
        BACK,
        r,
        "SharePoint storage is off. New document files are stored on this server.",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(on: bool, backend_locked: bool) -> SettingsPanel {
        SettingsPanel {
            site_url: String::new(),
            library: String::new(),
            interval: 15,
            site_locked: false,
            library_locked: false,
            interval_locked: false,
            backend_locked,
            storage: setup::StorageState {
                on,
                local_files: 0,
                sharepoint_files: 0,
            },
            probe: None,
        }
    }

    #[test]
    fn a_passing_test_asks_to_turn_storage_on_only_while_it_is_off() {
        assert!(panel(false, false).ready_note().contains("turn storage on"));
        assert!(!panel(true, false).ready_note().contains("turn storage on"));
        assert!(!panel(true, true).ready_note().contains("turn storage on"));
        assert!(!panel(false, true).ready_note().contains("turn storage on"));
    }
}
