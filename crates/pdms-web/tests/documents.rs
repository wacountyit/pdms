#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Document pages end to end: uploads without JavaScript, access denials,
//! downloads, and admin capabilities.

mod support;

use axum::http::StatusCode;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::{acl, directory};
use support::*;
use uuid::Uuid;

fn pdf(text: &str) -> Vec<u8> {
    format!("%PDF-1.4\n% {text}\n%%EOF\n").into_bytes()
}

async fn type_id(app: &App, key: &str) -> String {
    pdms_core::documents::types::type_by_key(&app.core.db, key)
        .await
        .unwrap()
        .id
        .to_string()
}

/// A department whose members can create and edit; returns (dept, user, totp secret).
async fn editor(app: &App, code: &str, email: &str) -> (Uuid, Uuid, Vec<u8>) {
    let dept = directory::create_department(&app.core.db, &system(), code, code)
        .await
        .unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(
            &app.core.db,
            &system(),
            Subject::Department(dept),
            Scope::Department(dept),
            p,
            None,
            "t",
        )
        .await
        .unwrap();
    }
    let (uid, secret) = user(&app.core, email, Some(dept), None).await;
    (dept, uid, secret)
}

async fn create_via_form(
    b: &mut Browser,
    app: &App,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    file: Option<(&str, &[u8])>,
) -> Resp {
    let csrf = b.get("/documents/new").await.csrf();
    let t = type_id(app, "partnership_agreement").await;
    let dept = dept.to_string();
    let owner = owner.to_string();
    b.multipart(
        "/documents",
        &[
            ("_csrf", csrf.as_str()),
            ("title", title),
            ("document_type_id", t.as_str()),
            ("department_id", dept.as_str()),
            ("owner_id", owner.as_str()),
            ("status", "active"),
            ("effective_date", "2013-05-01"),
            ("tags", "Ops, Partnership"),
        ],
        file,
    )
    .await
}

#[tokio::test]
async fn create_document_with_file_without_javascript() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = create_via_form(
        &mut b,
        &app,
        dept,
        uid,
        "Equipment maintenance agreement",
        Some(("agreement 2013.pdf", &pdf("2013"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let doc_url = r.location().to_owned();
    assert!(doc_url.starts_with("/documents/"));

    let page = b.get(&doc_url).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Equipment maintenance agreement"));
    assert!(page.body.contains("Document created."));
    assert!(page.body.contains("PAG-"));
    assert!(page.body.contains("Download agreement 2013.pdf"));
    assert!(
        page.body.contains("Partnership") && page.body.contains("Ops"),
        "tags shown"
    );

    // Download: right bytes, attachment, sniffed type, not cacheable.
    let dl = b.get(&format!("{doc_url}/download")).await;
    assert_eq!(dl.status, StatusCode::OK);
    assert_eq!(dl.headers["content-type"], "application/pdf");
    let cd = dl.headers["content-disposition"].to_str().unwrap();
    assert!(
        cd.starts_with("attachment;") && cd.contains("agreement%202013.pdf"),
        "{cd}"
    );
    assert_eq!(dl.headers["x-content-type-options"], "nosniff");
    assert_eq!(dl.body, String::from_utf8(pdf("2013")).unwrap());

    // It appears in search and the CSS for its tags exists.
    let list = b.get("/documents?q=maintenance").await;
    assert!(list.body.contains("Equipment maintenance agreement"));
    let css = b.get("/tag-colors.css").await;
    assert!(css.body.contains(".tag-"));
}

#[tokio::test]
async fn upload_csrf_must_be_first_and_valid() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let csrf = b.get("/documents/new").await.csrf();
    let t = type_id(&app, "other").await;
    let (dept, uid) = (dept.to_string(), uid.to_string());
    let base = [
        ("title", "x"),
        ("document_type_id", t.as_str()),
        ("department_id", dept.as_str()),
        ("owner_id", uid.as_str()),
    ];

    // Missing token.
    let r = b
        .multipart("/documents", &base, Some(("a.pdf", &pdf("a"))))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Token not first.
    let mut late: Vec<(&str, &str)> = base.to_vec();
    late.push(("_csrf", csrf.as_str()));
    assert_eq!(
        b.multipart("/documents", &late, None).await.status,
        StatusCode::FORBIDDEN
    );
    // Wrong token.
    let mut wrong = vec![("_csrf", "AAAA")];
    wrong.extend_from_slice(&base);
    assert_eq!(
        b.multipart("/documents", &wrong, None).await.status,
        StatusCode::FORBIDDEN
    );
    // Multipart to a non-upload route is never deferred.
    let r = b
        .multipart(
            "/admin/departments",
            &[("_csrf", csrf.as_str()), ("code", "X"), ("name", "X")],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn rejected_files_and_validation_errors_rerender_the_form() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let exe = [
        b'M', b'Z', 0x90, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0xFF, 0xFF, 0, 0,
    ];
    let r = create_via_form(
        &mut b,
        &app,
        dept,
        uid,
        "Bad upload",
        Some(("invoice.pdf", &exe)),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("not accepted"), "{}", r.body);
    assert!(
        r.body.contains("value=\"Bad upload\""),
        "entered values kept"
    );
    let r = create_via_form(&mut b, &app, dept, uid, "", None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("title must be"));
}

#[tokio::test]
async fn read_only_and_outside_users_are_denied_on_every_path() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut a = Browser::new(&app);
    a.login("alice@x.org", &secret).await;
    let doc = create_via_form(
        &mut a,
        &app,
        dept,
        uid,
        "Ops agreement",
        Some(("a.pdf", &pdf("a"))),
    )
    .await
    .location()
    .to_owned();

    // Bob is in another department: 403 everywhere, and the list is empty.
    let hr = directory::create_department(&app.core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    let (_, bob_secret) = user(&app.core, "bob@x.org", Some(hr), None).await;
    let mut b = Browser::new(&app);
    b.login("bob@x.org", &bob_secret).await;
    let csrf = b.get("/").await.csrf();
    for path in [
        doc.clone(),
        format!("{doc}/edit"),
        format!("{doc}/download"),
        format!("{doc}/versions/1/download"),
        format!("{doc}/history"),
    ] {
        assert_eq!(
            b.get(&path).await.status,
            StatusCode::FORBIDDEN,
            "GET {path}"
        );
    }
    for (path, form) in [
        (format!("{doc}/edit"), vec![("title", "x")]),
        (format!("{doc}/tags"), vec![("name", "x")]),
        (format!("{doc}/status"), vec![("status", "archived")]),
        (format!("{doc}/delete"), vec![("reason", "x")]),
    ] {
        let mut f = form;
        f.push(("_csrf", csrf.as_str()));
        assert_eq!(
            b.post(&path, &f).await.status,
            StatusCode::FORBIDDEN,
            "POST {path}"
        );
    }
    let list = b.get("/documents").await;
    assert!(!list.body.contains("Ops agreement"));
    assert!(
        !list.body.contains("New document"),
        "cannot create anywhere"
    );
    assert_eq!(b.get("/documents/new").await.status, StatusCode::FORBIDDEN);

    // Rita is in Ops with the default view grant only... (and Ops members here can edit),
    // so use a view-only department member by revoking nothing: a new department member of
    // a view-only department sees but cannot edit.
    let view_only = directory::create_department(&app.core.db, &system(), "BOARD", "Clerk")
        .await
        .unwrap();
    let (_, carol_secret) = user(&app.core, "carol@x.org", Some(view_only), None).await;
    let doc_id: Uuid = doc.trim_start_matches("/documents/").parse().unwrap();
    pdms_core::documents::share(
        &app.core,
        &system(),
        doc_id,
        Subject::Department(view_only),
        Permission::View,
        None,
        "review",
    )
    .await
    .unwrap();
    let mut c = Browser::new(&app);
    c.login("carol@x.org", &carol_secret).await;
    let page = c.get(&doc).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("Edit details") && !page.body.contains("Upload a new version"));
    assert_eq!(
        c.get(&format!("{doc}/edit")).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        c.get(&format!("{doc}/download")).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn upload_new_version_and_edit_details() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let doc = create_via_form(
        &mut b,
        &app,
        dept,
        uid,
        "Agreement",
        Some(("orig.pdf", &pdf("orig"))),
    )
    .await
    .location()
    .to_owned();
    let csrf = b.get(&doc).await.csrf();
    let r = b
        .multipart(
            &format!("{doc}/versions"),
            &[
                ("_csrf", csrf.as_str()),
                ("kind", "amendment"),
                ("note", "Amendment 1"),
                ("milestone_date", "2014-06-01"),
            ],
            Some(("amendment.pdf", &pdf("amend"))),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = b.get(&doc).await;
    assert!(page.body.contains("New version uploaded."));
    assert!(page.body.contains("Amendment 1") && page.body.contains("Jun 1, 2014"));
    assert!(page.body.contains("amendment.pdf") && page.body.contains("orig.pdf"));

    // Edit form round trip with optimistic locking.
    let edit = b.get(&format!("{doc}/edit")).await;
    assert_eq!(edit.status, StatusCode::OK);
    let row_version = regex::Regex::new(r#"name="row_version" value="(\d+)""#)
        .unwrap()
        .captures(&edit.body)
        .unwrap()[1]
        .to_owned();
    let t = type_id(&app, "partnership_agreement").await;
    let dept_s = dept.to_string();
    let form = [
        ("_csrf", csrf.as_str()),
        ("row_version", row_version.as_str()),
        ("title", "Agreement (amended)"),
        ("document_type_id", t.as_str()),
        ("department_id", dept_s.as_str()),
        ("renewal_date", "2027-12-31"),
    ];
    assert_eq!(
        b.post(&format!("{doc}/edit"), &form).await.status,
        StatusCode::SEE_OTHER
    );
    let mut stale_form = form.to_vec();
    stale_form[2] = ("title", "A different, stale edit");
    let stale = b.post(&format!("{doc}/edit"), &stale_form).await;
    assert_eq!(stale.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(stale.body.contains("Someone else changed this document"));
    let page = b.get(&doc).await;
    assert!(page.body.contains("Agreement (amended)") && page.body.contains("Dec 31, 2027"));
}

#[tokio::test]
async fn document_admin_pages_need_their_capabilities() {
    let app = app().await;
    let (_, admin_secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let (_, ro_secret) = user(&app.core, "records@x.org", None, Some("records_officer")).await;
    let (_, nobody_secret) = user(&app.core, "nobody@x.org", None, None).await;

    let mut admin = Browser::new(&app);
    admin.login("admin@x.org", &admin_secret).await;
    for path in [
        "/admin/document-types",
        "/admin/fields",
        "/admin/tags",
        "/admin/retention",
        "/admin/deleted",
    ] {
        assert_eq!(admin.get(path).await.status, StatusCode::OK, "admin {path}");
    }
    let csrf = admin.get("/admin/fields").await.csrf();
    let r = admin
        .post(
            "/admin/fields",
            &[
                ("_csrf", csrf.as_str()),
                ("label", "Vendor"),
                ("key", "vendor"),
                ("field_type", "single_select"),
                ("options", "acme | Acme Co.\nbeta"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let field = admin.get(r.location()).await;
    assert!(field.body.contains("acme | Acme Co."), "{}", field.body);

    let mut records = Browser::new(&app);
    records.login("records@x.org", &ro_secret).await;
    assert_eq!(records.get("/admin/retention").await.status, StatusCode::OK);
    assert!(
        records
            .get("/")
            .await
            .body
            .contains("href=\"/admin/retention\""),
        "nav points to retention"
    );
    for path in [
        "/admin/document-types",
        "/admin/fields",
        "/admin/tags",
        "/admin/users",
        "/admin/deleted",
    ] {
        assert_eq!(
            records.get(path).await.status,
            StatusCode::FORBIDDEN,
            "records officer {path}"
        );
    }

    let mut nobody = Browser::new(&app);
    nobody.login("nobody@x.org", &nobody_secret).await;
    for path in [
        "/admin/document-types",
        "/admin/fields",
        "/admin/tags",
        "/admin/retention",
        "/admin/deleted",
    ] {
        assert_eq!(
            nobody.get(path).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
}
