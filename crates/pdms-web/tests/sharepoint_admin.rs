// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Administration → SharePoint and the document-page notice. This app stores
//! files locally; the SharePoint records are written directly, as the change
//! check would leave them.

mod support;

use axum::http::StatusCode;
use pdms_authz::Visibility;
use pdms_core::directory;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use support::*;
use uuid::Uuid;

/// A document with one file, recorded as edited in SharePoint by Pat Clerk.
/// Returns (document, file version).
async fn flagged_document(
    app: &App,
    owner: Uuid,
    dept: Uuid,
    visibility: Visibility,
    title: &str,
) -> (Uuid, Uuid) {
    let core = &app.core;
    let mut w =
        pdms_storage::spool::SpoolWriter::create(&core.config().storage.spool_path, 1 << 20)
            .await
            .unwrap();
    w.write(b"%PDF-1.4\n% agreement\n%%EOF\n").await.unwrap();
    let file = documents::files::Upload {
        file: w.finish().await.unwrap(),
        file_name: "agreement.pdf".into(),
    };
    let t = types::type_by_key(&core.db, "contract").await.unwrap().id;
    let doc = documents::create(
        core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file: Some(file),
            note: String::new(),
            visibility,
        },
    )
    .await
    .unwrap();
    let version: Uuid = sqlx::query_scalar(
        "SELECT id FROM document_versions WHERE document_id = $1 AND file_changed",
    )
    .bind(doc)
    .fetch_one(&core.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sharepoint_files (document_version_id, document_id, drive_id, item_id, list_item_id, name,
                                       quick_xor_hash, size, last_known_path, state, changed_by, changed_at)
         VALUES ($1, $2, 'drive-1', $3, 'li-1', 'x.pdf', 'AAAAAAAAAAAAAAAAAAAAAAAAAAA=', 27, $4,
                 'changed_outside', 'Pat Clerk <pat.clerk@county.example>', now())",
    )
    .bind(version)
    .bind(doc)
    .bind(format!("item-{version}"))
    .bind(format!("Departments/Road and Bridge/{title}/{title} v1 – Original.pdf"))
    .execute(&core.db)
    .await
    .unwrap();
    (doc, version)
}

#[tokio::test]
async fn sharepoint_panel_is_for_administrators() {
    let app = app().await;
    let core = &app.core;
    let dept = directory::create_department(&core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let (admin, admin_totp) = user(core, "admin@x.org", Some(dept), Some("admin")).await;
    let (clerk, clerk_totp) = user(core, "clerk@x.org", Some(dept), None).await;
    let (doc, version) = flagged_document(
        &app,
        admin,
        dept,
        Visibility::Department,
        "Fuel Supply Agreement",
    )
    .await;
    // A Restricted document the administrator cannot open.
    let (secret_doc, _) = flagged_document(
        &app,
        clerk,
        dept,
        Visibility::Restricted,
        "Personnel Investigation",
    )
    .await;
    let secret_id: String = sqlx::query_scalar("SELECT external_id FROM documents WHERE id = $1")
        .bind(secret_doc)
        .fetch_one(&core.db)
        .await
        .unwrap();

    let mut c = Browser::new(&app);
    c.login("clerk@x.org", &clerk_totp).await;
    assert_eq!(
        c.get("/admin/sharepoint").await.status,
        StatusCode::FORBIDDEN
    );
    // People who can open the document see that its file was changed.
    let page = c.get(&format!("/documents/{doc}")).await;
    assert!(
        page.body
            .contains("A file of this document was changed outside PDMS."),
        "{}",
        page.body
    );
    assert!(
        !page.body.contains(r#"href="/admin/sharepoint""#),
        "no admin link for staff"
    );

    let mut b = Browser::new(&app);
    b.login("admin@x.org", &admin_totp).await;
    let page = b.get("/admin/sharepoint").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("SharePoint storage is not turned on"));
    assert!(page.body.contains("Fuel Supply Agreement"));
    assert!(page.body.contains("Pat Clerk"));
    assert!(
        page.body
            .contains(&format!("/admin/sharepoint/files/{version}/restore"))
    );
    // Nothing identifying for the document the administrator cannot open
    // (the owner's answer for P4): no title, path or ID.
    assert!(
        !page.body.contains("Personnel Investigation"),
        "title or path leaked"
    );
    assert!(!page.body.contains(&secret_id), "ID leaked");
    assert!(page.body.contains("A Restricted document"));
    assert_eq!(page.body.matches("<h1").count(), 1);
    // The document page links administrators to the SharePoint page.
    let doc_page = b.get(&format!("/documents/{doc}")).await;
    assert!(doc_page.body.contains(r#"<a href="/admin/sharepoint">"#));
    assert!(
        page.body.contains(r#"href="/admin/sharepoint""#),
        "navigation link"
    );

    let r = b
        .post("/admin/sharepoint/check", &[("_csrf", &page.csrf())])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'sharepoint.delta'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(queued, 1);
    // Without SharePoint storage, resolving explains itself instead of failing.
    let r = b
        .post(
            &format!("/admin/sharepoint/files/{version}/restore"),
            &[("_csrf", &page.csrf())],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
}
