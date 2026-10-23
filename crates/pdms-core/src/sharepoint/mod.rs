// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document files in a SharePoint library, through Microsoft Graph (D-081 to D-084).
//!
//! PDMS is the only writer. Files live in `Departments/`, `County-wide/`, or
//! `Restricted/`, then a department folder, then one folder per document with
//! one file per version (spec §5). Uploads are synchronous. Renames, moves, and
//! column values are applied by the `sharepoint.mirror_document` job, except
//! moves that narrow who can browse a document: those happen before the
//! change is saved (spec §7.3).

pub mod access;
pub mod changes;
mod files;
mod folders;
pub mod layout;
mod mirror;
mod ops;
pub mod quickxor;
pub mod resolve;
pub mod setup;
pub mod status;

use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::OnceCell;

use crate::config::SharePointConfig;
use crate::error::{CoreError, CoreResult};
use crate::graph::GraphClient;

pub use access::RECONCILE_JOB;
pub use changes::DELTA_JOB;
pub use files::{UploadTarget, UploadedFile};
pub(crate) use files::{open, record_upload, spool_copy, upload_staged};
pub use folders::{DocumentFolder, department_folder_id};
pub use layout::{Area, Spot};
pub(crate) use mirror::mirror_later;
pub use mirror::{
    MIRROR_JOB, MIRROR_SWEEP_JOB, apply_placement_now, enqueue_mirror, mirror_document,
    mirror_sweep, register_jobs,
};
pub use ops::register_ops_jobs;

/// Shown when an upload cannot reach SharePoint.
pub const UPLOAD_UNAVAILABLE: &str = "File storage (SharePoint) is unavailable right now, so nothing was saved. Try again in a few minutes.";

/// Shown when a download cannot reach SharePoint.
pub const READ_UNAVAILABLE: &str =
    "File storage (SharePoint) is unavailable right now. Try again in a few minutes.";

/// Shown when a change that must move files first cannot reach SharePoint.
pub const MOVE_UNAVAILABLE: &str = "File storage (SharePoint) is unavailable right now, so the change was not made. Try again in a few minutes.";

/// Log the underlying failure and return the user-facing "unavailable" error.
pub(crate) fn unavailable(message: &'static str, e: impl std::fmt::Display) -> CoreError {
    tracing::error!(error = %e, "SharePoint request failed");
    CoreError::Unavailable(message.to_owned())
}

/// SharePoint storage, shared by the web server and the worker.
#[derive(Clone)]
pub struct SharePoint {
    inner: Arc<Inner>,
}

struct Inner {
    graph: GraphClient,
    host: String,
    site_path: String,
    library: String,
    chunk_bytes: u64,
    resolved: OnceCell<Resolved>,
}

/// Graph IDs, looked up once per process on first use.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub site_id: String,
    pub drive_id: String,
    pub list_id: String,
    pub departments_id: String,
    pub county_wide_id: String,
    pub restricted_id: String,
    /// PDMS created `County-wide/` itself, so it lacks the "Everyone except
    /// external users" read grant an admin adds at setup (spec Appendix A, step 5).
    pub county_wide_created: bool,
}

impl Resolved {
    pub fn area_id(&self, area: Area) -> &str {
        match area {
            Area::Departments => &self.departments_id,
            Area::CountyWide => &self.county_wide_id,
            Area::Restricted => &self.restricted_id,
        }
    }
}

impl SharePoint {
    pub fn new(graph: GraphClient, cfg: &SharePointConfig) -> CoreResult<SharePoint> {
        let (host, site_path) = layout::parse_site_url(&cfg.site_url)
            .map_err(|e| CoreError::validation(format!("sharepoint.site_url: {e}")))?;
        Ok(SharePoint {
            inner: Arc::new(Inner {
                graph,
                host,
                site_path,
                library: cfg.library.trim().to_owned(),
                chunk_bytes: cfg.upload_chunk_bytes,
                resolved: OnceCell::new(),
            }),
        })
    }

    pub fn graph(&self) -> &GraphClient {
        &self.inner.graph
    }

    pub fn chunk_bytes(&self) -> u64 {
        self.inner.chunk_bytes
    }

    /// Site, library, list, and area folder IDs. Missing columns and area
    /// folders are created. A failure is not cached; the next call retries.
    pub async fn resolved(&self) -> CoreResult<&Resolved> {
        self.inner
            .resolved
            .get_or_try_init(|| resolve(&self.inner))
            .await
    }
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

#[derive(Deserialize)]
struct Values<T> {
    value: Vec<T>,
}

#[derive(Deserialize)]
struct Drive {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct Column {
    name: String,
}

/// What a read-only check of the site found (Administration → SharePoint).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// The site, library and list were found, and PDMS may manage sharing.
    pub ok: bool,
    /// Each step, in plain words: (worked, what).
    pub lines: Vec<(bool, String)>,
}

impl SharePoint {
    /// Check the site, library, columns and area folders the way first use
    /// does, but change nothing (first use creates what is missing).
    pub async fn probe(&self) -> Probe {
        let mut p = Probe::default();
        match probe(&self.inner, &mut p.lines).await {
            Ok(()) => p.ok = true,
            Err(line) => p.lines.push((false, line)),
        }
        p
    }
}

fn graph_problem(what: &str, e: &crate::graph::GraphError) -> String {
    use crate::graph::GraphError;
    match e {
        GraphError::NotFound => format!(
            "{what} wasn't found, or the PDMS Service app has no access to it (SHAREPOINT_SETUP.md B5)."
        ),
        GraphError::Auth(m) => format!("Microsoft refused: {m}"),
        other => format!("{what}: {other}"),
    }
}

async fn probe(inner: &Inner, out: &mut Vec<(bool, String)>) -> Result<(), String> {
    let g = &inner.graph;
    let site: IdOnly = g
        .get(&format!(
            "/sites/{}:{}?$select=id",
            inner.host, inner.site_path
        ))
        .await
        .map_err(|e| graph_problem("The site", &e))?;
    out.push((true, "Found the site.".into()));
    let drives: Values<Drive> = g
        .get(&format!("/sites/{}/drives?$select=id,name", site.id))
        .await
        .map_err(|e| graph_problem("The site's libraries", &e))?;
    let Some(drive) = drives
        .value
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(&inner.library))
    else {
        let names: Vec<&str> = drives.value.iter().map(|d| d.name.as_str()).collect();
        return Err(format!(
            "The library {:?} isn't on the site. Libraries found: {}.",
            inner.library,
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(", ")
            }
        ));
    };
    out.push((true, format!("Found the library {:?}.", drive.name)));
    let list: IdOnly = g
        .get(&format!("/drives/{}/list?$select=id", drive.id))
        .await
        .map_err(|e| graph_problem("The library's list", &e))?;
    let have: Values<Column> = g
        .get(&format!(
            "/sites/{}/lists/{}/columns?$select=name",
            site.id, list.id
        ))
        .await
        .map_err(|e| graph_problem("The library's columns", &e))?;
    let defs = layout::column_definitions();
    let missing = defs
        .iter()
        .filter(|d| {
            let name = d.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            !have.value.iter().any(|c| c.name == name)
        })
        .count();
    out.push((
        true,
        if missing == 0 {
            format!("All {} PDMS columns are there.", defs.len())
        } else {
            format!(
                "{missing} of the {} PDMS columns are missing; PDMS adds them when it stores the first file.",
                defs.len()
            )
        },
    ));
    for area in Area::ALL {
        let path = format!(
            "/drives/{}/root:/{}?$select=id",
            drive.id,
            layout::encode_segment(area.folder_name())
        );
        match g.get::<IdOnly>(&path).await {
            Ok(_) => out.push((true, format!("The {} folder is there.", area.folder_name()))),
            Err(crate::graph::GraphError::NotFound) => out.push((
                true,
                if area == Area::CountyWide {
                    "The County-wide folder is missing: PDMS creates it, but give \"Everyone except external users\" read access to it afterwards (SHAREPOINT_SETUP.md A5).".to_owned()
                } else {
                    format!("The {} folder is missing; PDMS creates it.", area.folder_name())
                },
            )),
            Err(e) => return Err(graph_problem(&format!("The {} folder", area.folder_name()), &e)),
        }
    }
    let _: serde_json::Value = g
        .get(&format!("/drives/{}/root/permissions", drive.id))
        .await
        .map_err(|e| {
            format!(
                "PDMS can't read the library's sharing ({}). It needs full control of the site to manage department folder access (SHAREPOINT_SETUP.md B5).",
                graph_problem("The library's sharing", &e)
            )
        })?;
    out.push((true, "PDMS can read the library's sharing.".into()));
    Ok(())
}

async fn resolve(inner: &Inner) -> CoreResult<Resolved> {
    let g = &inner.graph;
    // `site_path` comes from a parsed URL, so it is already percent-encoded.
    let site: IdOnly = g
        .get(&format!(
            "/sites/{}:{}?$select=id",
            inner.host, inner.site_path
        ))
        .await?;
    let drives: Values<Drive> = g
        .get(&format!("/sites/{}/drives?$select=id,name", site.id))
        .await?;
    let drive = drives
        .value
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(&inner.library))
        .ok_or_else(|| {
            CoreError::validation(format!(
                "the SharePoint library {:?} was not found on the site",
                inner.library
            ))
        })?;
    let list: IdOnly = g
        .get(&format!("/drives/{}/list?$select=id", drive.id))
        .await?;
    let have: Values<Column> = g
        .get(&format!(
            "/sites/{}/lists/{}/columns?$select=name",
            site.id, list.id
        ))
        .await?;
    for def in layout::column_definitions() {
        let name = def.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if !have.value.iter().any(|c| c.name == name) {
            let _: serde_json::Value = g
                .post(
                    &format!("/sites/{}/lists/{}/columns", site.id, list.id),
                    &def,
                )
                .await?;
        }
    }
    let (departments_id, _) = folders::ensure_area_folder(g, &drive.id, Area::Departments).await?;
    let (county_wide_id, county_wide_created) =
        folders::ensure_area_folder(g, &drive.id, Area::CountyWide).await?;
    let (restricted_id, _) = folders::ensure_area_folder(g, &drive.id, Area::Restricted).await?;
    if county_wide_created {
        tracing::warn!(
            "PDMS created the County-wide folder in SharePoint; give \"Everyone except external users\" read access to it (see the SharePoint setup guide)"
        );
    }
    Ok(Resolved {
        site_id: site.id,
        drive_id: drive.id,
        list_id: list.id,
        departments_id,
        county_wide_id,
        restricted_id,
        county_wide_created,
    })
}
