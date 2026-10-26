// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Setting SharePoint storage up from Administration (S1, D-114): the test
//! changes nothing, and the rules for the site and for turning storage on
//! and off.

mod common;
mod mock_graph;

use common::*;
use mock_graph::{MockGraph, certificate};
use pdms_authz::Visibility;
use pdms_core::config::StorageBackend;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::sharepoint::setup;
use pdms_core::{Config, Core, CoreError, directory};

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.config()).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

/// Connected to the mock Microsoft 365, files still stored locally.
async fn connected(sp: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let graph = sp.config(certificate());
    core_with(|c| c.graph = graph).await
}

async fn add_local_file(db: &pdms_testkit::TestDb) {
    sqlx::query(
        "INSERT INTO blobs (sha256, size_bytes, media_type, storage_backend)
         VALUES (sha256('x'), 1, 'application/pdf', 'local')",
    )
    .execute(&db.owner)
    .await
    .unwrap();
}

#[tokio::test]
async fn the_test_changes_nothing_in_sharepoint() {
    let sp = MockGraph::start().await;
    let (core, _db) = connected(&sp).await;
    let p = setup::test(&core, &system(), mock_graph::SITE_URL, "Documents")
        .await
        .unwrap();
    assert!(p.ok, "{:?}", p.lines);
    assert!(p.lines.iter().all(|(ok, _)| *ok), "{:?}", p.lines);
    assert!(sp.names("").is_empty(), "no folder created");
    assert!(
        !sp.columns().iter().any(|c| c.starts_with("Pdms")),
        "no column created"
    );
    assert!(
        audit_actions(&core)
            .await
            .contains(&"settings.tested".to_owned())
    );

    let p = setup::test(&core, &system(), mock_graph::SITE_URL, "Policies")
        .await
        .unwrap();
    assert!(!p.ok);
    assert!(
        p.lines.last().unwrap().1.contains("Libraries found"),
        "{:?}",
        p.lines
    );
}

#[tokio::test]
async fn the_test_needs_the_microsoft_365_connection() {
    let (core, _db) = core().await;
    let p = setup::test(&core, &system(), mock_graph::SITE_URL, "Documents")
        .await
        .unwrap();
    assert!(!p.ok);
    assert!(p.lines[0].1.contains("Microsoft 365 connection"));
}

#[tokio::test]
async fn storage_turns_on_and_off() {
    let sp = MockGraph::start().await;
    let (core, _db) = connected(&sp).await;
    let err = setup::turn_on(&core, &system()).await.unwrap_err();
    assert!(err.to_string().contains("site address"), "{err}");

    setup::save_site(&core, &system(), mock_graph::SITE_URL, "Documents", 10)
        .await
        .unwrap();
    assert_eq!(core.config().sharepoint.delta_interval_minutes, 10);
    setup::turn_on(&core, &system()).await.unwrap();
    assert_eq!(core.config().storage.backend, StorageBackend::SharePoint);
    assert!(core.sharepoint().is_some(), "applied without a restart");
    assert!(setup::state(&core).await.unwrap().on);

    let err = setup::turn_off(
        &core,
        &system(),
        "https://contoso.sharepoint.com/sites/Other",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "{err:?}");
    assert!(setup::state(&core).await.unwrap().on);
    setup::turn_off(&core, &system(), &format!("{}/", mock_graph::SITE_URL))
        .await
        .unwrap();
    assert_eq!(core.config().storage.backend, StorageBackend::Local);
}

#[tokio::test]
async fn storage_stays_off_while_files_are_stored_on_this_server() {
    let sp = MockGraph::start().await;
    let (core, db) = connected(&sp).await;
    setup::save_site(&core, &system(), mock_graph::SITE_URL, "Documents", 15)
        .await
        .unwrap();
    add_local_file(&db).await;
    let err = setup::turn_on(&core, &system()).await.unwrap_err();
    assert!(matches!(err, CoreError::Conflict(_)), "{err:?}");
    assert!(err.to_string().contains("1 document file"), "{err}");
    assert_eq!(core.config().storage.backend, StorageBackend::Local);
}

#[tokio::test]
async fn storage_stays_off_when_the_site_fails_the_test() {
    let sp = MockGraph::start().await;
    let (core, _db) = connected(&sp).await;
    setup::save_site(&core, &system(), mock_graph::SITE_URL, "Policies", 15)
        .await
        .unwrap();
    let err = setup::turn_on(&core, &system()).await.unwrap_err();
    assert!(err.to_string().contains("stays off"), "{err}");
    assert_eq!(core.config().storage.backend, StorageBackend::Local);
}

#[tokio::test]
async fn storage_set_in_the_configuration_file_cannot_be_switched() {
    let sp = MockGraph::start().await;
    let graph = sp.config(certificate());
    let (core, _db) = core_with(|c| {
        c.graph = graph;
        c.sharepoint.site_url = mock_graph::SITE_URL.into();
        c.locked.insert("storage.backend".into());
    })
    .await;
    let err = setup::turn_on(&core, &system()).await.unwrap_err();
    assert!(err.to_string().contains("configuration file"), "{err}");
}

#[tokio::test]
async fn a_site_set_in_the_configuration_file_is_used_when_the_form_leaves_it_out() {
    // The page shows locked fields disabled, and a browser doesn't send a
    // disabled field: the test and save arrive with them empty.
    let sp = MockGraph::start().await;
    let graph = sp.config(certificate());
    let (core, _db) = core_with(|c| {
        c.graph = graph;
        c.sharepoint.site_url = mock_graph::SITE_URL.into();
        c.locked.insert("sharepoint.site_url".into());
        c.locked.insert("sharepoint.library".into());
    })
    .await;
    let p = setup::test(&core, &system(), "", "").await.unwrap();
    assert!(p.ok, "{:?}", p.lines);
    setup::save_site(&core, &system(), "", "", 30)
        .await
        .unwrap();
    assert_eq!(core.config().sharepoint.site_url, mock_graph::SITE_URL);
    assert_eq!(core.config().sharepoint.delta_interval_minutes, 30);
}

#[tokio::test]
async fn the_site_cannot_move_once_sharepoint_holds_files() {
    let sp = MockGraph::start().await;
    let graph = sp.config(certificate());
    let (core, _db) = core_with(|c| {
        c.graph = graph;
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = mock_graph::SITE_URL.into();
    })
    .await;
    let rb = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let (owner, _) = user_in_new_department(&core, "CL", "owner@x.org").await;
    let other = types::type_by_key(&core.db, "other").await.unwrap().id;
    documents::create(
        &core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: "Policy".into(),
                document_type_id: other,
                department_id: rb,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            visibility: Visibility::Department,
            tags: vec![],
            file: Some(upload(&core, "p.pdf", &pdf("p")).await),
            note: String::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(setup::state(&core).await.unwrap().sharepoint_files, 1);

    let err = setup::save_site(
        &core,
        &system(),
        "https://contoso.sharepoint.com/sites/Other",
        "Documents",
        15,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::Conflict(_)), "{err:?}");
    // The check interval can still change.
    setup::save_site(&core, &system(), mock_graph::SITE_URL, "documents", 30)
        .await
        .unwrap();
    assert_eq!(core.config().sharepoint.delta_interval_minutes, 30);
}

#[tokio::test]
async fn only_settings_managers_can_set_storage_up() {
    let sp = MockGraph::start().await;
    let (core, _db) = connected(&sp).await;
    let (clerk, _) = user_in_new_department(&core, "RB", "clerk@x.org").await;
    let ctx = as_user(&core, clerk).await;
    for err in [
        setup::test(&core, &ctx, mock_graph::SITE_URL, "Documents")
            .await
            .unwrap_err(),
        setup::save_site(&core, &ctx, mock_graph::SITE_URL, "Documents", 15)
            .await
            .unwrap_err(),
        setup::turn_on(&core, &ctx).await.unwrap_err(),
        setup::turn_off(&core, &ctx, mock_graph::SITE_URL)
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::Forbidden(_)), "{err:?}");
    }
}
