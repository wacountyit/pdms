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
    // Tags are curated (D-088): the two the form ticks must exist.
    for name in ["Ops", "Partnership"] {
        let _ = pdms_core::documents::tags::create_curated(
            &app.core.db,
            &system(),
            name,
            "#1f6feb",
            "",
        )
        .await;
    }
    let csrf = b.get("/documents/new").await.csrf();
    let t = type_id(app, "interlocal_agreement").await;
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
async fn editors_pick_curated_tags() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = create_via_form(&mut b, &app, dept, uid, "Tagged agreement", None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    // The new-document form offers the curated tags as boxes, not a text field.
    let form = b.get("/documents/new").await;
    assert!(
        form.body
            .contains(r#"<input type="checkbox" name="tags" value="Ops""#)
    );
    assert!(!form.body.contains(r#"id="tags" name="tags""#));
    // The document page offers the curated tags it does not have yet: none.
    let page = b.get(r.location()).await;
    assert!(
        !page.body.contains(r#"<select id="tag-name""#),
        "both are on it"
    );
    pdms_core::documents::tags::create_curated(&app.core.db, &system(), "Grant", "#1f6feb", "")
        .await
        .unwrap();
    let page = b.get(r.location()).await;
    assert!(page.body.contains(r#"<select id="tag-name" name="name" required><option value="Grant">Grant</option></select>"#), "{}", page.body);
    // A tag nobody curated is refused.
    let doc = r.location().to_owned();
    let refused = b
        .post(
            &format!("{doc}/tags"),
            &[("_csrf", page.csrf().as_str()), ("name", "Invented")],
        )
        .await;
    assert_eq!(refused.status, StatusCode::SEE_OTHER);
    assert!(!b.get(&doc).await.body.contains(">Invented<"));
}

#[tokio::test]
async fn document_forms_are_grouped_in_panels() {
    let app = app().await;
    let (_, _, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let new = b.get("/documents/new").await;
    for section in [
        "Details",
        "Dates",
        "Additional fields",
        "Ownership",
        "Who can see it",
        "Tags",
        "File",
    ] {
        assert!(
            new.body.contains(&format!(
                r#"<legend class="form-section-title">{section}</legend>"#
            )),
            "{section}"
        );
    }
    // Every field the form posted before is still there.
    for name in [
        "title",
        "description",
        "document_type_id",
        "execution_date",
        "effective_date",
        "last_review_date",
        "next_review_date",
        "renewal_date",
        "expiration_date",
        "review_interval_months",
        "department_id",
        "owner_id",
        "status",
        "visibility",
        "file",
        "note",
    ] {
        assert!(new.body.contains(&format!(r#"name="{name}""#)), "{name}");
    }
    assert_eq!(
        new.body.matches(r#"<label class="choice-card">"#).count(),
        3
    );
    assert!(
        new.body
            .contains(r##"hx-get="/documents/fields" hx-target="#custom-fields""##)
    );
    assert_eq!(new.body.matches("<h1").count(), 1);
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
    assert!(page.body.contains("ILA-"));
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
        format!("{doc}/versions/1/view"),
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
    let t = type_id(&app, "interlocal_agreement").await;
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

#[tokio::test]
async fn owner_publishes_county_wide_then_restricts_through_the_pages() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "HR", "alice@x.org").await;
    let mut a = Browser::new(&app);
    a.login("alice@x.org", &secret).await;
    let csrf = a.get("/documents/new").await.csrf();
    let t = type_id(&app, "policy").await;
    let (d, o) = (dept.to_string(), uid.to_string());
    let doc = a
        .multipart(
            "/documents",
            &[
                ("_csrf", csrf.as_str()),
                ("title", "Leave policy"),
                ("document_type_id", t.as_str()),
                ("department_id", d.as_str()),
                ("owner_id", o.as_str()),
                ("status", "active"),
                ("visibility", "county_wide"),
            ],
            Some(("leave.pdf", &pdf("leave"))),
        )
        .await
        .location()
        .to_owned();

    let ops = directory::create_department(&app.core.db, &system(), "OPS", "OPS")
        .await
        .unwrap();
    let (_, bob_secret) = user(&app.core, "bob@x.org", Some(ops), None).await;
    let mut b = Browser::new(&app);
    b.login("bob@x.org", &bob_secret).await;
    let list = b.get("/documents").await;
    assert!(list.body.contains("Leave policy") && list.body.contains("County-wide"));
    assert_eq!(
        b.get(&format!("{doc}/download")).await.status,
        StatusCode::OK
    );

    // The owner sees the form on the document page and restricts it.
    let page = a.get(&doc).await;
    assert!(page.body.contains(&format!("action=\"{doc}/visibility\"")));
    let csrf = page.csrf();
    a.post(
        &format!("{doc}/visibility"),
        &[("_csrf", csrf.as_str()), ("visibility", "restricted")],
    )
    .await;
    assert_eq!(b.get(&doc).await.status, StatusCode::FORBIDDEN);
    assert!(
        actions(&app.core)
            .await
            .contains(&"document.visibility_changed".to_owned())
    );
}

#[tokio::test]
async fn unknown_visibility_value_is_rejected() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "HR", "alice@x.org").await;
    let mut a = Browser::new(&app);
    a.login("alice@x.org", &secret).await;
    let csrf = a.get("/documents/new").await.csrf();
    let t = type_id(&app, "policy").await;
    let (d, o) = (dept.to_string(), uid.to_string());
    let r = a
        .multipart(
            "/documents",
            &[
                ("_csrf", csrf.as_str()),
                ("title", "Leave policy"),
                ("document_type_id", t.as_str()),
                ("department_id", d.as_str()),
                ("owner_id", o.as_str()),
                ("visibility", "public"),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("choose who can see the document"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "nothing created");
}

async fn seed(
    app: &App,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    status: pdms_core::documents::status::DocumentStatus,
    review: Option<chrono::NaiveDate>,
    renewal: Option<chrono::NaiveDate>,
) {
    use pdms_core::documents::{MetadataInput, NewDocument};
    let t = type_id(app, "policy").await.parse().unwrap();
    pdms_core::documents::create(
        &app.core,
        &system(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
                department_id: dept,
                next_review_date: review,
                renewal_date: renewal,
                ..Default::default()
            },
            owner_id: owner,
            status,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();
}

/// The `<a>` element (href, inner html) whose href carries `status=<value>`.
fn tab<'a>(body: &'a str, value: &str) -> (&'a str, &'a str) {
    let needle = format!("status={value}\"");
    let end = body
        .find(&needle)
        .unwrap_or_else(|| panic!("no {value} tab"));
    let start = body[..end].rfind("<a href=\"").unwrap();
    let close = body[end..].find("</a>").unwrap() + end;
    (&body[start..end], &body[end..close])
}

#[tokio::test]
async fn status_tabs_show_counts_and_keep_filters() {
    use pdms_core::documents::status::DocumentStatus as S;
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "HR", "alice@x.org").await;
    seed(&app, dept, uid, "Safety plan", S::Active, None, None).await;
    seed(&app, dept, uid, "Safety manual", S::Active, None, None).await;
    seed(&app, dept, uid, "Safety draft", S::Draft, None, None).await;
    seed(&app, dept, uid, "Budget memo", S::Active, None, None).await;
    seed(&app, dept, uid, "Safety old", S::Active, None, None).await;
    sqlx::query("UPDATE documents SET status = 'archived' WHERE title = 'Safety old'")
        .execute(&app.core.db)
        .await
        .unwrap();
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let t = type_id(&app, "policy").await;
    let page = b
        .get(&format!("/documents?q=Safety&type={t}&page=1&sort=title"))
        .await;
    for value in [
        "active",
        "in_review",
        "renewal_due",
        "overdue",
        "draft",
        "expired",
    ] {
        let (href, _) = tab(&page.body, value);
        assert!(href.contains("q=Safety"), "{value}: {href}");
        assert!(href.contains(&format!("type={t}")), "{value}: {href}");
        assert!(href.contains("sort=title"), "{value}: {href}");
        assert!(!href.contains("page="), "{value}: {href}");
    }
    let (_, inner) = tab(&page.body, "active");
    assert!(inner.contains("Active") && inner.contains(">2<"), "{inner}");
    let (_, inner) = tab(&page.body, "draft");
    assert!(inner.contains(">1<"), "{inner}");
    // "All" excludes archived, has no status param, and is the current tab.
    let tabs = page.body.find("class=\"tabs\"").unwrap();
    let all = page.body[tabs..].find("aria-current=\"page\"").unwrap() + tabs;
    let all = &page.body[all..all + 120];
    assert!(all.contains("All") && all.contains(">3<"), "{all}");
    assert!(
        page.body.contains("<details class=\"disclosure\" open"),
        "sort is a More filter"
    );
    let plain = b.get("/documents?q=Safety").await;
    assert!(!plain.body.contains("<details class=\"disclosure\" open"));
    // Choosing a tab marks it current and opens nothing else.
    let page = b.get("/documents?status=archived").await;
    assert!(page.body.contains("Safety old") && !page.body.contains("Budget memo"));
    assert!(page.body.contains("<details class=\"disclosure\" open"));
    // Archived has no tab of its own, so one is added and marked current.
    let tabs = &page.body[page.body.find("aria-label=\"Status\"").unwrap()..];
    let tabs = &tabs[..tabs.find("</nav>").unwrap()];
    let (_, inner) = tab(tabs, "archived");
    assert!(inner.contains("aria-current=\"page\""), "{inner}");
    assert!(
        inner.contains("Archived") && inner.contains(">1<"),
        "{inner}"
    );
    assert_eq!(tabs.matches("aria-current").count(), 1, "{tabs}");
}

#[tokio::test]
async fn next_date_column_shows_overdue_and_labels() {
    use pdms_core::documents::status::DocumentStatus as S;
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "HR", "alice@x.org").await;
    let today = chrono::Utc::now().date_naive();
    let day = chrono::Days::new(40);
    seed(
        &app,
        dept,
        uid,
        "Late one",
        S::Active,
        Some(today - day),
        None,
    )
    .await;
    seed(
        &app,
        dept,
        uid,
        "Renews soon",
        S::Active,
        None,
        Some(today + day),
    )
    .await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let body = b.get("/documents").await.body;
    assert!(body.contains("Next date") && !body.contains("<th scope=\"col\">Next review"));
    assert!(body.contains("Overdue · "), "{body}");
    assert!(body.contains("Renewal · "));
    // An expired document's past review is not called overdue.
    seed(
        &app,
        dept,
        uid,
        "Lapsed one",
        S::Active,
        Some(today - day),
        None,
    )
    .await;
    sqlx::query("UPDATE documents SET status = 'expired' WHERE title = 'Lapsed one'")
        .execute(&app.core.db)
        .await
        .unwrap();
    let body = b.get("/documents?status=expired").await.body;
    assert!(body.contains("Lapsed one"), "{body}");
    assert!(!body.contains("Overdue · "), "{body}");
    let empty = b.get("/documents?q=zzzznothing").await.body;
    assert!(empty.contains("Clear filters"));
}

// ---------------------------------------------------------------------------
// The document page (direction D)
// ---------------------------------------------------------------------------

fn local_today(app: &App) -> chrono::NaiveDate {
    pdms_core::schedule::local_today(chrono::Utc::now(), app.core.config().timezone())
}

/// Run the planner as of this morning: date-driven statuses and review tasks.
async fn plan_now(app: &App) {
    let morning = local_today(app).and_hms_opt(16, 0, 0).unwrap().and_utc();
    pdms_core::notifications::plan(&app.core, morning)
        .await
        .unwrap();
}

async fn doc_id(app: &App, title: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM documents WHERE title = $1")
        .bind(title)
        .fetch_one(&app.core.db)
        .await
        .unwrap()
}

/// The value of the key-number tile labelled `label`, if shown.
fn key_number(body: &str, label: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(
        r#"<dt class="label">{}</dt>\s*<dd class="value">([^<]*)</dd>\s*<dd class="sub">([^<]*)</dd>"#,
        regex::escape(label)
    ))
    .unwrap();
    re.captures(body)
        .map(|c| format!("{} | {}", c[1].trim(), c[2].trim()))
}

#[tokio::test]
async fn document_page_keeps_every_action() {
    use pdms_core::documents::status::DocumentStatus as S;
    let app = app().await;
    let (dept, uid, _) = editor(&app, "OPS", "alice@x.org").await;
    let (admin, admin_secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    // Administrators view and manage permissions everywhere; give this one
    // edit and delete too, so every part of the page applies.
    for p in [Permission::Edit, Permission::Delete] {
        acl::grant(
            &app.core.db,
            &system(),
            Subject::User(admin),
            Scope::Department(dept),
            p,
            None,
            "t",
        )
        .await
        .unwrap();
    }
    // No retention rule, so nothing blocks deletion and its form shows.
    sqlx::query("UPDATE document_types SET retention_policy_id = NULL")
        .execute(&app.core.db)
        .await
        .unwrap();
    seed(&app, dept, uid, "Fleet policy", S::Active, None, None).await;
    let id = doc_id(&app, "Fleet policy").await;
    let mut b = Browser::new(&app);
    b.login("admin@x.org", &admin_secret).await;
    let page = b.get(&format!("/documents/{id}")).await;
    assert_eq!(page.status, StatusCode::OK);
    let body = page.body.as_str();

    let manage = body
        .find("<details class=\"disclosure\" id=\"manage\">")
        .expect("a closed Manage disclosure");
    let manage_end = body[manage..].find("</details>").unwrap() + manage;
    assert!(body[manage..manage_end].contains("Manage this document"));
    for path in ["status", "tags", "versions"] {
        let action = format!("action=\"/documents/{id}/{path}\"");
        let at = body.find(&action).unwrap_or_else(|| panic!("no {action}"));
        assert!(at < manage, "{action} stays outside Manage");
    }
    for path in ["visibility", "grants", "owner", "retention", "delete"] {
        let action = format!("action=\"/documents/{id}/{path}\"");
        let at = body.find(&action).unwrap_or_else(|| panic!("no {action}"));
        assert!(at > manage && at < manage_end, "{action} is inside Manage");
    }
    assert!(body.contains(&format!("action=\"/documents/{id}/rules\"")));
    assert!(body.contains(&format!("action=\"/documents/{id}/comments\"")));
    assert!(body.contains(&format!("href=\"/documents/{id}/edit\"")));
    assert!(body.contains(&format!("href=\"/documents/{id}/history\"")));

    // "On this page" links only to sections that exist: no dates here.
    for anchor in ["workflow", "files", "comments", "manage"] {
        assert!(body.contains(&format!("href=\"#{anchor}\"")), "{anchor}");
        assert!(body.contains(&format!("id=\"{anchor}\"")), "{anchor}");
    }
    assert!(!body.contains("href=\"#dates\"") && !body.contains("id=\"dates\""));
    assert!(
        !body.contains(">Start review</a>"),
        "admin is not the owner"
    );
}

#[tokio::test]
async fn start_review_button_only_for_owner_with_due_review() {
    use pdms_core::documents::status::DocumentStatus as S;
    let app = app().await;
    let (dept, alice, alice_secret) = editor(&app, "OPS", "alice@x.org").await;
    let today = local_today(&app);
    let days = chrono::Days::new;
    seed(
        &app,
        dept,
        alice,
        "Late review",
        S::Active,
        Some(today - days(10)),
        None,
    )
    .await;
    seed(
        &app,
        dept,
        alice,
        "Soon review",
        S::Active,
        Some(today + days(20)),
        None,
    )
    .await;
    seed(
        &app,
        dept,
        alice,
        "Later review",
        S::Active,
        Some(today + days(60)),
        None,
    )
    .await;
    plan_now(&app).await;
    let late = doc_id(&app, "Late review").await;
    let soon = doc_id(&app, "Soon review").await;
    let later = doc_id(&app, "Later review").await;

    let mut a = Browser::new(&app);
    a.login("alice@x.org", &alice_secret).await;
    let start = "<a class=\"button primary\" href=\"#reviews\">Start review</a>";
    for d in [late, soon] {
        let page = a.get(&format!("/documents/{d}")).await;
        assert!(page.body.contains(start), "{}", page.body);
    }
    // Once the review is started, the button goes.
    let page = a.get(&format!("/documents/{soon}")).await;
    let start_form = regex::Regex::new(&format!(
        r#"action="(/documents/{soon}/tasks/[0-9a-f-]+/start)""#
    ))
    .unwrap();
    let action = start_form.captures(&page.body).unwrap()[1].to_owned();
    let r = a.post(&action, &[("_csrf", &page.csrf())]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = a.get(&format!("/documents/{soon}")).await;
    assert!(!page.body.contains(">Start review</a>"), "already started");
    let page = a.get(&format!("/documents/{later}")).await;
    // The open task's card, not the "Days until review" tile's "Review due …".
    let card = format!(
        "<h3>Review due {} <span class=\"badge task-open\">Open</span></h3>",
        (today + days(60)).format("%b %-d, %Y")
    );
    assert!(page.body.contains(&card), "the task exists: {card}");
    // Its only key date is ahead, so today starts the timeline's axis.
    assert!(
        page.body
            .contains("<line class=\"timeline-today\" x1=\"0.0\"")
    );
    assert!(page.body.contains(
        "<circle cx=\"1000.0\" cy=\"22\" r=\"7\" class=\"timeline-mark timeline-future\">"
    ));
    assert!(!page.body.contains(">Start review</a>"), "due in 60 days");

    // Carol can view the overdue document but does not own it.
    let board = directory::create_department(&app.core.db, &system(), "BOARD", "Clerk")
        .await
        .unwrap();
    let (_, carol_secret) = user(&app.core, "carol@x.org", Some(board), None).await;
    pdms_core::documents::share(
        &app.core,
        &system(),
        late,
        Subject::Department(board),
        Permission::View,
        None,
        "review",
    )
    .await
    .unwrap();
    let mut c = Browser::new(&app);
    c.login("carol@x.org", &carol_secret).await;
    let page = c.get(&format!("/documents/{late}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains(">Start review</a>"));
    assert!(page.body.contains("Days overdue"));
    assert!(
        !page.body.contains("id=\"manage\"") && !page.body.contains("href=\"#manage\""),
        "nothing to manage, so no disclosure and no link"
    );
}

#[tokio::test]
async fn key_numbers_show_versions_and_overdue() {
    use pdms_core::signatures::{self, NewSignatory, SignedRecord};
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let doc = create_via_form(
        &mut b,
        &app,
        dept,
        uid,
        "Facilities agreement",
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
                ("note", "Amendment No. 1"),
                ("milestone_date", "2014-06-01"),
            ],
            Some(("amendment.pdf", &pdf("amend"))),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    // A details-only version carries the amendment's file forward.
    let r = b
        .post(
            &format!("{doc}/versions/1/restore"),
            &[("_csrf", csrf.as_str()), ("part", "metadata")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let id: Uuid = doc.trim_start_matches("/documents/").parse().unwrap();
    let today = local_today(&app);
    sqlx::query("UPDATE documents SET next_review_date = $1 WHERE id = $2")
        .bind(today - chrono::Days::new(40))
        .bind(id)
        .execute(&app.core.db)
        .await
        .unwrap();
    plan_now(&app).await;
    let mut signers = Vec::new();
    for name in ["Jordan Presiding", "Morgan Partner"] {
        let s = signatures::add(
            &app.core,
            &system(),
            id,
            NewSignatory {
                name: name.into(),
                title: "Signer".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        signers.push(s);
    }
    signatures::record_signed(
        &app.core,
        &system(),
        signers[0],
        SignedRecord {
            signed_on: Some("2014-06-03".parse().unwrap()),
            note: String::new(),
            file: None,
        },
    )
    .await
    .unwrap();

    let page = b.get(&doc).await;
    let body = page.body.as_str();
    assert_eq!(
        key_number(body, "Versions").as_deref(),
        Some("2 | Latest Jun 1, 2014"),
        "{body}"
    );
    let overdue = key_number(body, "Days overdue").unwrap();
    assert!(overdue.starts_with("40 | Review was due "), "{overdue}");
    assert!(key_number(body, "Missed review cycles").is_some());
    assert!(
        key_number(body, "Days until review").is_none(),
        "overdue replaces days until"
    );
    assert_eq!(
        key_number(body, "Signatures").as_deref(),
        Some("1 of 2 | 1 pending")
    );
    assert!(body.contains("Signed Jun 3, 2014") && body.contains("Awaiting signature"));
    // Files: the amendment's upload holds the current file.
    assert_eq!(body.matches(">Current</span>").count(), 1);
    assert!(body.contains(
        "<span class=\"file-name\">amendment.pdf</span> <span class=\"status status-good\">Current</span>"
    ));

    // Key dates: the SVG is decorative, the ordered list is the accessible version.
    assert!(body.contains(
        "<svg class=\"timeline\" viewBox=\"0 0 1000 64\" aria-hidden=\"true\" focusable=\"false\">"
    ));
    assert!(body.contains("href=\"#dates\"") && body.contains("id=\"dates\""));
    let list = body.find("<ol class=\"timeline-list\">").unwrap();
    let list = &body[list..list + body[list..].find("</ol>").unwrap()];
    assert!(
        list.contains("Effective") && list.contains("May 1, 2013"),
        "{list}"
    );
    assert!(list.contains("Amendment No. 1") && list.contains("Jun 1, 2014"));
    assert!(list.contains("Review missed"), "{list}");
}

/// When SharePoint can't be reached, nothing is saved and the new-document
/// form comes back with what was typed and a message (D-102).
#[tokio::test]
async fn a_sharepoint_outage_keeps_the_new_document_form() {
    use pdms_core::config::{GraphConfig, Secret, StorageBackend};
    let mut config = config();
    // Nothing listens on port 9 here, so every Graph request fails at once.
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
    config.storage.backend = StorageBackend::SharePoint;
    config.sharepoint.site_url = "https://contoso.sharepoint.com/sites/PDMSTest".into();
    let app = app_with(config).await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let r = create_via_form(
        &mut b,
        &app,
        dept,
        uid,
        "Fuel supply agreement",
        Some(("a.pdf", &pdf("a"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    assert!(r.body.contains("nothing was saved"), "{}", r.body);
    assert!(r.body.contains("choose it again"), "{}", r.body);
    assert!(
        r.body.contains(r#"value="Fuel supply agreement""#),
        "{}",
        r.body
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

const VIEW_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; sandbox";
const PAGE_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

fn png_bytes() -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 4, 8, 6, 0, 0, 0, 0, 0, 0, 0]);
    v
}

fn jpeg_bytes() -> Vec<u8> {
    let mut v = vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00,
    ];
    v.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
    v
}

fn gif_bytes() -> Vec<u8> {
    let mut v = b"GIF89a".to_vec();
    v.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0, 0x3B]);
    v
}

fn webp_bytes() -> Vec<u8> {
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&[26, 0, 0, 0]);
    v.extend_from_slice(b"WEBPVP8 ");
    v.extend_from_slice(&[0; 16]);
    v
}

/// Legacy Word: the OLE signature, then zeros.
fn doc_bytes() -> Vec<u8> {
    let mut v = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    v.extend_from_slice(&[0; 504]);
    v
}

fn header<'a>(r: &'a Resp, name: &str) -> &'a str {
    r.headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

#[tokio::test]
async fn view_shows_pdfs_and_images_in_the_browser() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("report.pdf", pdf("view"), "application/pdf"),
        ("photo.png", png_bytes(), "image/png"),
        ("scan.jpg", jpeg_bytes(), "image/jpeg"),
        ("map.gif", gif_bytes(), "image/gif"),
        ("chart.webp", webp_bytes(), "image/webp"),
    ];
    for (name, bytes, media) in cases {
        let doc = create_via_form(&mut b, &app, dept, uid, name, Some((name, &bytes)))
            .await
            .location()
            .to_owned();
        let r = b.get(&format!("{doc}/versions/1/view")).await;
        assert_eq!(r.status, StatusCode::OK, "{name}: {}", r.body);
        assert_eq!(header(&r, "content-type"), media, "{name}");
        let cd = header(&r, "content-disposition");
        assert!(
            cd.starts_with("inline;") && cd.contains(name),
            "{name}: {cd}"
        );
        assert_eq!(header(&r, "x-content-type-options"), "nosniff");
        assert_eq!(header(&r, "cache-control"), "private, no-store");
        assert!(!r.headers.contains_key("accept-ranges"), "{name}");
        assert_eq!(header(&r, "content-security-policy"), VIEW_CSP, "{name}");
        assert_eq!(header(&r, "x-frame-options"), "DENY");
        assert_eq!(
            header(&r, "content-length"),
            bytes.len().to_string(),
            "{name}"
        );
        if media == "application/pdf" {
            assert_eq!(r.body.as_bytes(), bytes.as_slice());
        }
    }
}

#[tokio::test]
async fn view_sends_other_types_as_downloads() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let txt = b"Meeting notes\n<script>alert(1)</script>\n".to_vec();
    for (name, bytes) in [("minutes.doc", doc_bytes()), ("notes.txt", txt)] {
        let r = create_via_form(&mut b, &app, dept, uid, name, Some((name, &bytes))).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{name}: {}", r.body);
        let doc = r.location().to_owned();
        let v = b.get(&format!("{doc}/versions/1/view")).await;
        let d = b.get(&format!("{doc}/versions/1/download")).await;
        assert_eq!(v.status, StatusCode::OK, "{name}");
        assert!(
            header(&v, "content-disposition").starts_with("attachment;"),
            "{name}"
        );
        assert_eq!(header(&v, "content-security-policy"), PAGE_CSP, "{name}");
        assert_eq!(v.body, d.body, "{name}");
        assert_eq!(header(&v, "content-length"), header(&d, "content-length"));
    }
}

#[tokio::test]
async fn only_the_view_answer_gets_the_view_policy() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut a = Browser::new(&app);
    a.login("alice@x.org", &secret).await;
    let doc = create_via_form(&mut a, &app, dept, uid, "Ops", Some(("a.pdf", &pdf("a"))))
        .await
        .location()
        .to_owned();
    let hr = directory::create_department(&app.core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    let (_, bob_secret) = user(&app.core, "bob@x.org", Some(hr), None).await;
    let mut b = Browser::new(&app);
    b.login("bob@x.org", &bob_secret).await;

    let page = a.get(&doc).await;
    let download = a.get(&format!("{doc}/download")).await;
    let css = a.get("/static/app.css").await;
    let before = actions(&app.core).await.len();
    let missing = a.get(&format!("{doc}/versions/99/view")).await;
    let missing_dl = a.get(&format!("{doc}/versions/99/download")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.status, missing_dl.status);
    assert_eq!(actions(&app.core).await.len(), before, "no audit row");
    let denied = b.get(&format!("{doc}/versions/1/view")).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    for (name, r) in [
        ("page", &page),
        ("download", &download),
        ("css", &css),
        ("missing", &missing),
        ("denied", &denied),
    ] {
        let csp = header(r, "content-security-policy");
        assert_eq!(csp, PAGE_CSP, "{name}");
        assert!(!csp.contains("sandbox"), "{name}");
    }
}

#[tokio::test]
async fn view_keeps_odd_file_names_safe() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let name = "R\u{e9}sum\u{e9} \"final\".pdf";
    let doc = create_via_form(&mut b, &app, dept, uid, "Odd", Some((name, &pdf("odd"))))
        .await
        .location()
        .to_owned();
    let view = b.get(&format!("{doc}/versions/1/view")).await;
    let dl = b.get(&format!("{doc}/versions/1/download")).await;
    let (v, d) = (
        header(&view, "content-disposition"),
        header(&dl, "content-disposition"),
    );
    assert!(d.contains("filename*=UTF-8''"), "{d}");
    assert_eq!(v, d.replacen("attachment", "inline", 1));
}

#[tokio::test]
async fn each_version_is_viewed_with_its_own_file() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "OPS", "alice@x.org").await;
    let mut b = Browser::new(&app);
    b.login("alice@x.org", &secret).await;
    let first = pdf("first");
    let second = pdf("second one");
    let doc = create_via_form(&mut b, &app, dept, uid, "Two", Some(("one.pdf", &first)))
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
            Some(("two.pdf", &second)),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let v1 = b.get(&format!("{doc}/versions/1/view")).await;
    let v2 = b.get(&format!("{doc}/versions/2/view")).await;
    assert_eq!(v1.body.as_bytes(), first.as_slice());
    assert_eq!(v2.body.as_bytes(), second.as_slice());
}
