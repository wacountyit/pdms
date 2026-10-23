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
use futures_util::StreamExt;
use mock_graph::{MockGraph, certificate};
use pdms_authz::{Permission, Scope, Subject, Visibility};
use pdms_core::config::StorageBackend;
use pdms_core::directory;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::exports;
use pdms_core::sharepoint::Area;
use pdms_core::{Config, Core, CoreError, Ctx, acl, sharepoint};
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
        "PDMS had to create County-wide itself"
    );
    for c in [
        "PdmsDocumentId",
        "PdmsVisibility",
        "PdmsCurrent",
        "PdmsLink",
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

/// A department whose members may create and edit; returns (department, member).
async fn editing_department(core: &Core, code: &str, name: &str, email: &str) -> (Uuid, Uuid) {
    let dept = new_department(core, code, name).await;
    let user = directory::create_user(&core.db, &system(), email, email, Some(dept))
        .await
        .unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(
            &core.db,
            &system(),
            Subject::Department(dept),
            Scope::Department(dept),
            p,
            None,
            "test",
        )
        .await
        .unwrap();
    }
    (dept, user)
}

async fn try_new_doc(
    core: &Core,
    ctx: &Ctx,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    visibility: Visibility,
    file: Option<(&str, &[u8])>,
) -> Result<Uuid, CoreError> {
    let file = match file {
        Some((name, bytes)) => Some(upload(core, name, bytes).await),
        None => None,
    };
    let other = types::type_by_key(&core.db, "other").await.unwrap().id;
    documents::create(
        core,
        ctx,
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: other,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            visibility,
            tags: vec![],
            file,
            note: String::new(),
        },
    )
    .await
}

async fn new_doc(
    core: &Core,
    ctx: &Ctx,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    visibility: Visibility,
    file: Option<(&str, &[u8])>,
) -> Uuid {
    try_new_doc(core, ctx, dept, owner, title, visibility, file)
        .await
        .unwrap()
}

async fn external_id(core: &Core, id: Uuid) -> String {
    sqlx::query_scalar("SELECT external_id FROM documents WHERE id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

async fn document_count(core: &Core) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&core.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_upload_lands_in_its_department_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("Fuel supply agreement");
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel Supply Agreement",
        Visibility::Department,
        Some(("agreement.pdf", &bytes)),
    )
    .await;
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Fuel Supply Agreement");
    let names = sp.names(&folder);
    assert_eq!(names.len(), 1, "{names:?}");
    // Until the mirror job runs, the file carries its temporary name.
    assert!(
        names[0].starts_with(&format!("{ext} (uploading ")) && names[0].ends_with(".pdf"),
        "{names:?}"
    );
    assert_eq!(
        sp.item(&format!("{folder}/{}", names[0])).unwrap().content,
        bytes
    );
    let backend: String = sqlx::query_scalar(
        "SELECT b.storage_backend FROM blobs b JOIN document_versions v ON v.blob_sha256 = b.sha256 WHERE v.document_id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(backend, "sharepoint");
    let recorded: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sharepoint_files WHERE document_id = $1")
            .bind(id)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(recorded, 1);
}

#[tokio::test]
async fn large_files_are_sent_in_chunks() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = "PDMS chunk test\n".repeat(50_000).into_bytes(); // 800,000 bytes: three 320 KiB chunks
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Big notes",
        Visibility::Department,
        Some(("notes.txt", &bytes)),
    )
    .await;
    let puts = sp
        .requests()
        .iter()
        .filter(|r| r.starts_with("PUT /upload/"))
        .count();
    assert_eq!(puts, 3);
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Big notes");
    let name = sp.names(&folder).pop().unwrap();
    assert!(name.ends_with(".txt"));
    assert_eq!(sp.item(&format!("{folder}/{name}")).unwrap().content, bytes);
}

#[tokio::test]
async fn a_new_version_goes_into_the_same_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("v1"))),
    )
    .await;
    documents::add_version(
        &core,
        &ctx,
        id,
        upload(&core, "b.pdf", &pdf("v2")).await,
        documents::VersionKind::Amendment,
        "",
        None,
    )
    .await
    .unwrap();
    let ext = external_id(&core, id).await;
    assert_eq!(
        sp.names(&format!("Departments/Road and Bridge/{ext} – Fuel"))
            .len(),
        2
    );
}

#[tokio::test]
async fn awkward_titles_still_get_safe_folders() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let title = format!(
        "~Q3 \"Final\" Agreement: Road #5 / Bridge 50% _vti_ *draft*... {}",
        "x".repeat(400)
    );
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        &title,
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    let ext = external_id(&core, id).await;
    let folders = sp.names("Departments/Road and Bridge");
    assert_eq!(folders.len(), 1);
    let f = &folders[0];
    assert!(
        f.starts_with(&format!(
            "{ext} – Q3 _Final_ Agreement_ Road _5 _ Bridge 50_ _vti-"
        )),
        "{f}"
    );
    assert!(f.chars().count() <= 100);
    assert!(!f.chars().any(|c| "\"*:<>?/\\|#%".contains(c)));
}

#[tokio::test]
async fn upload_failure_saves_nothing() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    core.sharepoint.clone().unwrap().resolved().await.unwrap();
    let before = document_count(&core).await;
    sp.fail("POST /drives/drive-1/items/");
    let err = try_new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, CoreError::Unavailable(m) if m.contains("nothing was saved")),
        "{err:?}"
    );
    assert_eq!(document_count(&core).await, before);
}

#[tokio::test]
async fn empty_files_are_refused() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let err = try_new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Empty",
        Visibility::Department,
        Some(("empty.txt", b"")),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, CoreError::Validation(m) if m.contains("empty")),
        "{err:?}"
    );
}

async fn read_all(mut s: pdms_storage::ByteStream) -> Result<Vec<u8>, std::io::Error> {
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

#[tokio::test]
async fn downloads_stream_from_sharepoint_and_are_audited() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("Fuel supply agreement");
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &bytes)),
    )
    .await;
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(read_all(dl.stream).await.unwrap(), bytes);
    assert!(
        sp.requests()
            .iter()
            .any(|r| r.starts_with("GET /v1.0/drives/drive-1/items/") && r.ends_with("/content"))
    );
    assert!(
        audit_actions(&core)
            .await
            .contains(&"document.downloaded".to_owned())
    );
}

#[tokio::test]
async fn changed_files_stop_the_download_and_are_flagged() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("original"))),
    )
    .await;
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Fuel");
    let name = sp.names(&folder).pop().unwrap();
    sp.tamper(
        &format!("{folder}/{name}"),
        b"%PDF-1.4 edited directly in SharePoint",
    );
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert!(
        read_all(dl.stream).await.is_err(),
        "the download must not complete"
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM sharepoint_files WHERE document_id = $1")
            .bind(id)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(state, "changed_outside");
    assert!(
        audit_actions(&core)
            .await
            .contains(&"sharepoint.changed_outside".to_owned())
    );
}

#[tokio::test]
async fn text_extraction_reads_the_file_from_sharepoint() {
    // Plain text: the test PDFs are not real enough for the PDF text reader.
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.txt", b"Fuel supply agreement")),
    )
    .await;
    pdms_core::ocr::extract(&core, id, 1, false).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status::text FROM document_texts WHERE document_id = $1")
            .bind(id)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(status, "done");
}

#[tokio::test]
async fn records_packages_read_files_from_sharepoint() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    let officer = directory::create_user(&core.db, &system(), "Dana", "dana@x.org", Some(dept))
        .await
        .unwrap();
    let role = directory::role_id_by_key(&core.db, "records_officer")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), officer, role, None)
        .await
        .unwrap();
    let ctx_o = as_user(&core, officer).await;
    let pkg = exports::build(
        &core,
        &ctx_o,
        exports::PackageRequest {
            request_reference: "REQ-2026-001".into(),
            document_ids: vec![id],
            fields: vec![],
            versions: exports::Versions::Current,
            include_audit: false,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    // The package re-hashes every file against PDMS's record while writing (D-067).
    let (_, package) = exports::open(&core, &ctx_o, pkg).await.unwrap();
    assert!(!read_all(package).await.unwrap().is_empty());
}

#[tokio::test]
async fn integrity_check_covers_sharepoint_files() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    let r = pdms_core::integrity::verify(&core, true).await.unwrap();
    assert!(r.problems.is_empty(), "{} problems", r.problems.len());
    assert_eq!(r.sharepoint_files_checked, 1);
}

#[tokio::test]
async fn restoring_a_file_makes_a_new_file() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let original = pdf("original terms");
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &original)),
    )
    .await;
    documents::add_version(
        &core,
        &ctx,
        id,
        upload(&core, "b.pdf", &pdf("amended")).await,
        documents::VersionKind::Amendment,
        "",
        None,
    )
    .await
    .unwrap();
    let v = documents::restore_version(&core, &ctx, id, 1, documents::RestorePart::File)
        .await
        .unwrap();
    assert_eq!(v, 3);
    let ext = external_id(&core, id).await;
    assert_eq!(
        sp.names(&format!("Departments/Road and Bridge/{ext} – Fuel"))
            .len(),
        3
    );
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(dl.version_no, 3);
    assert_eq!(read_all(dl.stream).await.unwrap(), original);
}

async fn pending_mirrors(core: &Core, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'sharepoint.mirror_document' AND status = 'pending'
           AND payload->>'document_id' = $1",
    )
    .bind(id.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap()
}

async fn clear_mirrors(core: &Core) {
    sqlx::query("DELETE FROM jobs WHERE kind = 'sharepoint.mirror_document'")
        .execute(&core.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_mirror_names_files_and_fills_columns() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel Supply Agreement",
        Visibility::Department,
        Some(("a.pdf", &pdf("v1"))),
    )
    .await;
    documents::add_version(
        &core,
        &ctx,
        id,
        upload(&core, "b.pdf", &pdf("v2")).await,
        documents::VersionKind::Amendment,
        "",
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        pending_mirrors(&core, id).await,
        1,
        "one pending job, however many changes"
    );
    sharepoint::mirror_document(&core, id).await.unwrap();
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Fuel Supply Agreement");
    assert_eq!(
        sp.names(&folder),
        vec![
            format!("{ext} v1 – Original.pdf"),
            format!("{ext} v2 – Amendment.pdf")
        ]
    );
    let v1 = sp.fields(&format!("{folder}/{ext} v1 – Original.pdf"));
    let v2 = sp.fields(&format!("{folder}/{ext} v2 – Amendment.pdf"));
    assert_eq!(v1["PdmsCurrent"], serde_json::json!(false));
    assert_eq!(v2["PdmsCurrent"], serde_json::json!(true));
    assert_eq!(v2["PdmsDocumentId"], serde_json::json!(ext));
    assert_eq!(v2["PdmsDepartment"], serde_json::json!("Road and Bridge"));
    assert_eq!(v2["PdmsVisibility"], serde_json::json!("Department"));
    assert!(
        v2["PdmsLink"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/documents/{id}"))
    );
    // Running it again changes nothing in SharePoint.
    let before = sp.requests().len();
    sharepoint::mirror_document(&core, id).await.unwrap();
    let patches = sp.requests()[before..]
        .iter()
        .filter(|r| r.starts_with("PATCH"))
        .count();
    assert_eq!(patches, 0);
}

#[tokio::test]
async fn retitling_renames_the_document_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("v1"))),
    )
    .await;
    let (doc, _) = documents::for_edit(&core, &ctx, id).await.unwrap();
    let input = MetadataInput {
        title: "Fuel Supply Agreement".into(),
        document_type_id: doc.document_type_id,
        department_id: doc.department_id,
        ..Default::default()
    };
    documents::update(&core, &ctx, id, input, doc.row_version)
        .await
        .unwrap();
    sharepoint::mirror_document(&core, id).await.unwrap();
    let ext = external_id(&core, id).await;
    assert_eq!(
        sp.names("Departments/Road and Bridge"),
        vec![format!("{ext} – Fuel Supply Agreement")]
    );
}

#[tokio::test]
async fn every_kind_of_change_queues_the_mirror() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let bob = directory::create_user(&core.db, &system(), "Bob", "bob@x.org", Some(dept))
        .await
        .unwrap();
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("v1"))),
    )
    .await;
    clear_mirrors(&core).await;

    // Tags are curated (D-088).
    documents::tags::create_curated(&core.db, &system(), "Roads", "#1f6feb", "")
        .await
        .unwrap();
    documents::add_tag(&core, &ctx, id, "Roads").await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "tag added");
    clear_mirrors(&core).await;

    documents::change_status(&core, &ctx, id, DocumentStatus::Archived, "")
        .await
        .unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "status");
    clear_mirrors(&core).await;

    documents::transfer_owner(&core, &system(), id, bob, "handover")
        .await
        .unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "owner");
    clear_mirrors(&core).await;

    documents::set_visibility(
        &core,
        &as_user(&core, bob).await,
        id,
        Visibility::CountyWide,
    )
    .await
    .unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "visibility");
    clear_mirrors(&core).await;

    documents::delete(&core, &system(), id, "duplicate")
        .await
        .unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "deleted");
    clear_mirrors(&core).await;

    documents::restore_deleted(&core, &system(), id)
        .await
        .unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "restored");
}

#[tokio::test]
async fn the_nightly_sweep_queues_every_document_once() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let a = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "A",
        Visibility::Department,
        Some(("a.pdf", &pdf("a"))),
    )
    .await;
    let b = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "B",
        Visibility::Department,
        Some(("b.pdf", &pdf("b"))),
    )
    .await;
    new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "No file",
        Visibility::Department,
        None,
    )
    .await;
    clear_mirrors(&core).await;
    assert_eq!(sharepoint::mirror_sweep(&core).await.unwrap(), 2);
    assert_eq!(
        sharepoint::mirror_sweep(&core).await.unwrap(),
        0,
        "already pending"
    );
    assert_eq!(
        pending_mirrors(&core, a).await + pending_mirrors(&core, b).await,
        2
    );
}

#[tokio::test]
async fn tightening_visibility_moves_the_folder_before_saving() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::CountyWide,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    let ext = external_id(&core, id).await;
    assert_eq!(
        sp.names("County-wide/Road and Bridge"),
        vec![format!("{ext} – Fuel")]
    );
    documents::set_visibility(&core, &ctx, id, Visibility::Restricted)
        .await
        .unwrap();
    // No mirror job has run: the move happened as part of the change.
    assert!(sp.names("County-wide/Road and Bridge").is_empty());
    assert_eq!(
        sp.names("Restricted/Road and Bridge"),
        vec![format!("{ext} – Fuel")]
    );
    // The move is audited (D-102).
    assert!(
        audit_actions(&core)
            .await
            .contains(&"sharepoint.folder_moved".to_owned())
    );
}

#[tokio::test]
async fn tightening_is_refused_when_sharepoint_is_down() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::CountyWide,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    sp.fail("PATCH /drives/drive-1/items/");
    let err = documents::set_visibility(&core, &ctx, id, Visibility::Restricted)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Unavailable(m) if m.contains("the change was not made")),
        "{err:?}"
    );
    let doc = documents::view(&core, &ctx, id).await.unwrap().document;
    assert_eq!(doc.visibility, Visibility::CountyWide, "PDMS unchanged");
    assert_eq!(
        sp.names("County-wide/Road and Bridge").len(),
        1,
        "folder unchanged"
    );
}

#[tokio::test]
async fn loosening_waits_for_the_mirror_job() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    documents::set_visibility(&core, &ctx, id, Visibility::CountyWide)
        .await
        .unwrap();
    assert_eq!(
        sp.names("Departments/Road and Bridge").len(),
        1,
        "not moved yet"
    );
    sharepoint::mirror_document(&core, id).await.unwrap();
    assert!(sp.names("Departments/Road and Bridge").is_empty());
    assert_eq!(sp.names("County-wide/Road and Bridge").len(), 1);
}

#[tokio::test]
async fn department_transfer_moves_the_folder_now() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ops = new_department(&core, "OPS", "Operations").await;
    acl::grant(
        &core.db,
        &system(),
        Subject::User(alice),
        Scope::Department(ops),
        Permission::Create,
        None,
        "test",
    )
    .await
    .unwrap();
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    let (doc, _) = documents::for_edit(&core, &ctx, id).await.unwrap();
    let input = MetadataInput {
        title: doc.title.clone(),
        document_type_id: doc.document_type_id,
        department_id: ops,
        ..Default::default()
    };
    documents::update(&core, &ctx, id, input, doc.row_version)
        .await
        .unwrap();
    assert!(sp.names("Departments/Road and Bridge").is_empty());
    assert_eq!(sp.names("Departments/Operations").len(), 1);
}

#[tokio::test]
async fn deleted_documents_leave_the_department_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Fuel",
        Visibility::Department,
        Some(("a.pdf", &pdf("x"))),
    )
    .await;
    documents::delete(&core, &system(), id, "duplicate")
        .await
        .unwrap();
    assert!(sp.names("Departments/Road and Bridge").is_empty());
    assert_eq!(sp.names("Restricted/Road and Bridge").len(), 1);
    documents::restore_deleted(&core, &system(), id)
        .await
        .unwrap();
    sharepoint::mirror_document(&core, id).await.unwrap();
    assert_eq!(sp.names("Departments/Road and Bridge").len(), 1);
}

/// Round trip against a real SharePoint **test** site. Not run by default.
///
/// ```text
/// PDMS_LIVE_TENANT_ID=… PDMS_LIVE_CLIENT_ID=… PDMS_LIVE_CERTIFICATE_FILE=/path/graph.pem \
/// PDMS_LIVE_SITE_URL=https://<tenant>.sharepoint.com/sites/PDMSRecordsTest \
/// cargo test -p pdms-core --test sharepoint live_sharepoint_round_trip -- --ignored --nocapture
/// ```
///
/// It leaves one document folder in the site's `Restricted` area; the
/// printed path says where.
#[tokio::test]
#[ignore = "needs a real SharePoint test site and app registration"]
async fn live_sharepoint_round_trip() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let graph = pdms_core::config::GraphConfig {
        tenant_id: var("PDMS_LIVE_TENANT_ID"),
        client_id: var("PDMS_LIVE_CLIENT_ID"),
        certificate: pdms_core::config::Secret::new(
            std::fs::read_to_string(var("PDMS_LIVE_CERTIFICATE_FILE")).unwrap(),
        ),
        ..Default::default()
    };
    let site = var("PDMS_LIVE_SITE_URL");
    let (core, _db) = core_with(|c| {
        c.graph = graph;
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = site;
    })
    .await;
    let code = format!(
        "LIVE{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(6)
            .collect::<String>()
            .to_uppercase()
    );
    let (dept, alice) = editing_department(
        &core,
        &code,
        &format!("PDMS live test {code}"),
        "live@x.org",
    )
    .await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("PDMS live SharePoint test");
    let id = new_doc(
        &core,
        &ctx,
        dept,
        alice,
        "Live test",
        Visibility::Department,
        Some(("live.pdf", &bytes)),
    )
    .await;
    sharepoint::mirror_document(&core, id).await.unwrap();
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(read_all(dl.stream).await.unwrap(), bytes);
    documents::set_visibility(&core, &ctx, id, Visibility::Restricted)
        .await
        .unwrap();
    println!(
        "left in SharePoint: Restricted/PDMS live test {code}/{} – Live test",
        external_id(&core, id).await
    );
}
