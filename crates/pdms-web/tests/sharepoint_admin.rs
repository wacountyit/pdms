// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Administration → SharePoint and the document-page notice. Files are stored
//! locally (the last test's app stores them in SharePoint but never reaches
//! it); the SharePoint records are written directly, as the change check
//! would leave them.

mod support;

use axum::http::StatusCode;
use pdms_authz::Visibility;
use pdms_core::config::{GraphConfig, Secret, StorageBackend};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::{Core, directory};
use support::*;
use uuid::Uuid;

/// A document with one file, recorded as edited in SharePoint by Pat Clerk.
/// Returns (document, file version).
async fn flagged_document(
    core: &Core,
    owner: Uuid,
    dept: Uuid,
    visibility: Visibility,
    title: &str,
) -> (Uuid, Uuid) {
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
        core,
        admin,
        dept,
        Visibility::Department,
        "Fuel Supply Agreement",
    )
    .await;
    // A Restricted document the administrator cannot open.
    let (secret_doc, _) = flagged_document(
        core,
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
        page.body.contains(
            "A file of this document was changed outside PDMS. Downloading and viewing it are paused"
        ),
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

#[tokio::test]
async fn administrators_set_sharepoint_up_on_the_page() {
    let app = app().await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/sharepoint").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(r#"id="settings-h""#), "{}", page.body);
    assert!(page.body.contains("Storage off"));
    let csrf = page.csrf();
    let site = "https://contoso.sharepoint.com/sites/PDMS";

    // The test needs the Microsoft 365 connection, and shows each step.
    let r = b
        .post(
            "/admin/sharepoint/test",
            &[
                ("_csrf", &csrf),
                ("site_url", site),
                ("library", "Documents"),
                ("interval", "15"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Test result"));
    assert!(r.body.contains("Set up the Microsoft 365 connection first"));
    assert!(r.body.contains(site), "the typed address stays in the form");
    assert!(
        actions(&app.core)
            .await
            .contains(&"settings.tested".to_owned())
    );

    b.post(
        "/admin/sharepoint/settings",
        &[
            ("_csrf", &csrf),
            ("site_url", site),
            ("library", "Documents"),
            ("interval", "20"),
        ],
    )
    .await;
    assert_eq!(app.core.config().sharepoint.site_url, site);
    assert_eq!(app.core.config().sharepoint.delta_interval_minutes, 20);

    // Turning storage on tests the site first, so it stays off here.
    b.post("/admin/sharepoint/turn-on", &[("_csrf", &csrf)])
        .await;
    let page = b.get("/admin/sharepoint").await;
    assert!(
        page.body.contains("SharePoint storage stays off"),
        "{}",
        page.body
    );
    assert_eq!(
        app.core.config().storage.backend,
        pdms_core::config::StorageBackend::Local
    );
}

#[tokio::test]
async fn files_stored_here_keep_sharepoint_storage_off() {
    let app = app().await;
    let (admin, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let rb = directory::create_department(&app.core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    flagged_document(&app.core, admin, rb, Visibility::Department, "Agreement").await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/sharepoint").await;
    assert!(
        page.body
            .contains("1 document file is stored on this server")
    );
    assert!(!page.body.contains("Turn SharePoint storage on"));
}

#[tokio::test]
async fn settings_from_the_configuration_file_are_shown_locked() {
    let mut c = config();
    c.sharepoint.site_url = "https://contoso.sharepoint.com/sites/PDMS".into();
    c.locked.insert("sharepoint.site_url".into());
    c.locked.insert("storage.backend".into());
    let app = app_with(c).await;
    let (_, secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &secret).await;
    let page = b.get("/admin/sharepoint").await;
    assert!(page.body.contains(r#"aria-describedby="sp-site-locked""#));
    assert!(page.body.contains("<code>[storage] backend</code>"));
    assert!(!page.body.contains("Turn SharePoint storage on"));
}

/// A file paused by the SharePoint check is refused at both addresses, as a
/// page with the page policy. SharePoint is never asked for it, so this app
/// stores files in SharePoint but has no Graph to reach (port 9).
#[tokio::test]
async fn a_paused_file_is_refused_at_both_addresses() {
    let mut config = config();
    config.graph = GraphConfig {
        tenant_id: "tenant-1".into(),
        client_id: "client-1".into(),
        certificate: Secret::new(
            pdms_core::graph::cert::generate("PDMS test", 30)
                .unwrap()
                .pem,
        ),
        authority_url: "http://127.0.0.1:9".into(),
        api_url: "http://127.0.0.1:9/v1.0".into(),
        timeout_seconds: 5,
        max_retries: 0,
    };
    config.sharepoint.site_url = "https://contoso.sharepoint.com/sites/PDMSTest".into();
    let local = config.clone();
    config.storage.backend = StorageBackend::SharePoint;
    let app = app_with(config).await;
    // The document is created through a core that stores files here; its
    // file is then recorded as a SharePoint file the check paused.
    let creator = Core::new(app.core.db.clone(), local).unwrap();
    let dept = directory::create_department(&app.core.db, &system(), "RB", "Road and Bridge")
        .await
        .unwrap();
    let (clerk, clerk_totp) = user(&app.core, "clerk@x.org", Some(dept), None).await;
    let (doc, _) = flagged_document(
        &creator,
        clerk,
        dept,
        Visibility::Department,
        "Fuel Supply Agreement",
    )
    .await;
    let mut c = Browser::new(&app);
    c.login("clerk@x.org", &clerk_totp).await;
    let before = actions(&app.core).await.len();
    for path in [
        format!("/documents/{doc}/download"),
        format!("/documents/{doc}/versions/1/view"),
    ] {
        let r = c.get(&path).await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{path}: {}", r.body);
        assert!(r.body.contains("are paused"), "{path}: {}", r.body);
        let csp = r
            .headers
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(csp.starts_with("default-src 'self';"), "{path}: {csp}");
        assert!(!csp.contains("sandbox"), "{path}: {csp}");
    }
    assert_eq!(actions(&app.core).await.len(), before, "no audit row");
}
