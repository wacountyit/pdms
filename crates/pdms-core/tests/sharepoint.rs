// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! SharePoint document storage (P3) against the in-process mock Graph.

mod common;
mod mock_graph;

use common::*;
use mock_graph::{MockGraph, certificate};
use pdms_core::config::StorageBackend;
use pdms_core::directory;
use pdms_core::sharepoint::Area;
use pdms_core::{Config, Core, sharepoint};
use uuid::Uuid;

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.config).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

/// A `Core` that stores document files in the mock SharePoint, in 320 KiB chunks.
async fn sp_core(sp: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let graph = sp.config(certificate());
    core_with(|c| {
        c.graph = graph;
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = mock_graph::SITE_URL.into();
        c.sharepoint.upload_chunk_bytes = 327_680;
    })
    .await
}

async fn new_department(core: &Core, code: &str, name: &str) -> Uuid {
    directory::create_department(&core.db, &system(), code, name)
        .await
        .unwrap()
}

#[tokio::test]
async fn first_use_creates_the_area_folders_and_columns() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let handle = core.sharepoint.clone().expect("sharepoint backend");
    let r = handle.resolved().await.unwrap();
    assert_eq!(
        sp.names(""),
        vec!["County-wide", "Departments", "Restricted"]
    );
    assert!(
        r.county_wide_created,
        "PADM had to create County-wide itself"
    );
    for c in [
        "PadmDocumentId",
        "PadmVisibility",
        "PadmCurrent",
        "PadmLink",
    ] {
        assert!(sp.columns().contains(&c.to_owned()), "{c}");
    }
    // Another process finds everything instead of creating duplicates.
    let other =
        sharepoint::SharePoint::new(core.graph.clone().unwrap(), &core.config.sharepoint).unwrap();
    let r2 = other.resolved().await.unwrap();
    assert_eq!(r2.departments_id, r.departments_id);
    assert!(!r2.county_wide_created);
    assert_eq!(sp.columns().len(), 14, "no column created twice");
}

#[tokio::test]
async fn the_sharepoint_backend_needs_the_graph_section() {
    let (core, _db) = core().await;
    let mut config = (*core.config).clone();
    config.storage.backend = StorageBackend::SharePoint;
    config.sharepoint.site_url = mock_graph::SITE_URL.into();
    let Err(err) = Core::new(core.db.clone(), config) else {
        panic!("the SharePoint backend started without [graph]");
    };
    assert!(err.to_string().contains("[graph]"), "{err}");
}

#[tokio::test]
async fn department_folders_are_created_once_per_area() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let rb = new_department(&core, "RB", "Road and Bridge").await;
    let a = sharepoint::department_folder_id(&core, Area::Departments, rb)
        .await
        .unwrap();
    let b = sharepoint::department_folder_id(&core, Area::Departments, rb)
        .await
        .unwrap();
    assert_eq!(a, b);
    sharepoint::department_folder_id(&core, Area::CountyWide, rb)
        .await
        .unwrap();
    assert_eq!(sp.names("Departments"), vec!["Road and Bridge"]);
    assert_eq!(sp.names("County-wide"), vec!["Road and Bridge"]);
}

#[tokio::test]
async fn departments_with_the_same_folder_name_get_their_own_folders() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let one = new_department(&core, "RB1", "Road:Bridge").await;
    let two = new_department(&core, "RB2", "Road|Bridge").await;
    let a = sharepoint::department_folder_id(&core, Area::Departments, one)
        .await
        .unwrap();
    let b = sharepoint::department_folder_id(&core, Area::Departments, two)
        .await
        .unwrap();
    assert_ne!(a, b);
    assert_eq!(
        sp.names("Departments"),
        vec!["Road_Bridge", "Road_Bridge (RB2)"]
    );
}

#[tokio::test]
async fn renaming_a_department_renames_its_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let rb = new_department(&core, "RB", "Road and Bridge").await;
    let before = sharepoint::department_folder_id(&core, Area::Departments, rb)
        .await
        .unwrap();
    sqlx::query("UPDATE departments SET name = 'Roads' WHERE id = $1")
        .bind(rb)
        .execute(&core.db)
        .await
        .unwrap();
    let after = sharepoint::department_folder_id(&core, Area::Departments, rb)
        .await
        .unwrap();
    assert_eq!(before, after, "renamed in place");
    assert_eq!(sp.names("Departments"), vec!["Roads"]);
}

#[tokio::test]
async fn an_untracked_folder_left_by_an_interrupted_run_is_adopted() {
    let sp = MockGraph::start().await;
    let (core, db) = sp_core(&sp).await;
    let rb = new_department(&core, "RB", "Road and Bridge").await;
    let first = sharepoint::department_folder_id(&core, Area::Departments, rb)
        .await
        .unwrap();
    // Forget the row, as if the process died after creating the folder.
    sqlx::query("DELETE FROM sharepoint_department_folders")
        .execute(&db.owner)
        .await
        .unwrap();
    let again = sharepoint::department_folder_id(&core, Area::Departments, rb)
        .await
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(sp.names("Departments"), vec!["Road and Bridge"]);
}
