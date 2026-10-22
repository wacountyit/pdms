// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document files in a SharePoint library, through Microsoft Graph (D-081 to D-084).
//!
//! PADM is the only writer. Files live in `Departments/`, `County-wide/`, or
//! `Restricted/`, then a department folder, then one folder per document with
//! one file per version (spec §5). Uploads are synchronous. Renames, moves, and
//! column values are applied by the `sharepoint.mirror_document` job, except
//! moves that narrow who can browse a document: those happen before the
//! change is saved (spec §7.3).

mod folders;
pub mod layout;
pub mod quickxor;

use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::OnceCell;

use crate::config::SharePointConfig;
use crate::error::{CoreError, CoreResult};
use crate::graph::GraphClient;

pub use folders::department_folder_id;
pub use layout::{Area, Spot};

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
    /// PADM created `County-wide/` itself, so it lacks the "Everyone except
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
            "PADM created the County-wide folder in SharePoint; give \"Everyone except external users\" read access to it (see the SharePoint setup guide)"
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
