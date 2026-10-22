#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 5 through the pages and the API: comments, date suggestions, and the
//! text extraction (OCR) administration page.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::{acl, api_tokens, directory, ocr};
use support::*;
use uuid::Uuid;

fn re(pattern: &str, body: &str) -> String {
    regex::Regex::new(pattern)
        .unwrap()
        .captures(body)
        .unwrap_or_else(|| panic!("{pattern} not found"))[1]
        .to_owned()
}

#[tokio::test]
async fn comments_and_date_suggestions_on_the_document_page() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let (owner, owner_secret) = user(&app.core, "owner@x.org", Some(dept), None).await;
    let (_, gus_secret) = user(&app.core, "gus@x.org", Some(dept), None).await;
    let (_, out_secret) = user(&app.core, "out@x.org", None, None).await;
    acl::grant(
        &app.core.db,
        &system(),
        Subject::User(owner),
        Scope::Department(dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    acl::grant(
        &app.core.db,
        &system(),
        Subject::User(owner),
        Scope::Department(dept),
        Permission::Create,
        None,
        "t",
    )
    .await
    .unwrap();

    // Upload an agreement through the form.
    let mut o = Browser::new(&app);
    o.login("owner@x.org", &owner_secret).await;
    let csrf = o.get("/documents/new").await.csrf();
    let t = pdms_core::documents::types::type_by_key(&app.core.db, "interlocal_agreement")
        .await
        .unwrap()
        .id
        .to_string();
    let dept_s = dept.to_string();
    let owner_s = owner.to_string();
    let pdf = pdms_core::demo::demo_pdf(&[
        "This Agreement shall be effective March 1, 2013 <script>alert(1)</script>",
        "and shall expire on February 28, 2018.",
    ]);
    let r = o
        .multipart(
            "/documents",
            &[
                ("_csrf", csrf.as_str()),
                ("title", "Warehouse agreement"),
                ("document_type_id", t.as_str()),
                ("department_id", dept_s.as_str()),
                ("owner_id", owner_s.as_str()),
                ("status", "active"),
            ],
            Some(("agreement.pdf", &pdf)),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let doc: Uuid = r
        .location()
        .trim_start_matches("/documents/")
        .parse()
        .unwrap();
    ocr::extract(&app.core, doc, 1, false).await.unwrap();

    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("Dates found in the file"));
    assert!(page.body.contains("February 28, 2018") && page.body.contains("Use this date"));
    assert!(
        !page.body.contains("<script>alert(1)</script>"),
        "text from files is escaped"
    );
    let csrf = page.csrf();
    let accept = re(
        r#"action="(/documents/[0-9a-f-]{36}/suggestions/[0-9a-f-]{36}/accept)""#,
        &page.body,
    );
    assert_eq!(
        o.post(&accept, &[("_csrf", csrf.as_str())]).await.status,
        StatusCode::SEE_OTHER
    );
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(
        page.body
            .contains("Date confirmed and saved to the document.")
    );

    // Comments: post with a mention; Gus is notified and replies.
    o.post(
        &format!("/documents/{doc}/comments"),
        &[
            ("_csrf", csrf.as_str()),
            ("body", "@gus please check <b>section 4</b>"),
        ],
    )
    .await;
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(
        page.body
            .contains(r#"<strong class="mention">@gus</strong>"#),
        "mention emphasized"
    );
    assert!(
        page.body.contains("&lt;b&gt;section 4&lt;/b&gt;"),
        "comment text is escaped"
    );
    let first = re(r#"id="comment-([0-9a-f-]{36})""#, &page.body);
    let mut g = Browser::new(&app);
    g.login("gus@x.org", &gus_secret).await;
    assert!(g.get("/notifications").await.body.contains("mentioned you"));
    let gp = g.get(&format!("/documents/{doc}")).await;
    let first_s = first.clone();
    g.post(
        &format!("/documents/{doc}/comments"),
        &[
            ("_csrf", gp.csrf().as_str()),
            ("parent", first_s.as_str()),
            ("body", "Looks fine."),
        ],
    )
    .await;
    // The owner edits, then the history shows both versions; Gus cannot see it.
    o.post(
        &format!("/documents/{doc}/comments/{first}/edit"),
        &[
            ("_csrf", csrf.as_str()),
            ("body", "@gus please check section 5"),
        ],
    )
    .await;
    let hist = o
        .get(&format!("/documents/{doc}/comments/{first}/history"))
        .await;
    assert_eq!(hist.status, StatusCode::OK);
    assert!(hist.body.contains("section 4") && hist.body.contains("section 5"));
    assert_eq!(
        g.get(&format!("/documents/{doc}/comments/{first}/history"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        g.post(
            &format!("/documents/{doc}/comments/{first}/delete"),
            &[("_csrf", gp.csrf().as_str())]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    o.post(
        &format!("/documents/{doc}/comments/{first}/delete"),
        &[("_csrf", csrf.as_str())],
    )
    .await;
    let page = g.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("Deleted by owner@x.org"));
    assert!(page.body.contains("Looks fine."), "the reply stays");

    // Someone who cannot see the document cannot comment.
    let mut x = Browser::new(&app);
    x.login("out@x.org", &out_secret).await;
    let xc = x.get("/").await.csrf();
    assert_eq!(
        x.post(
            &format!("/documents/{doc}/comments"),
            &[("_csrf", xc.as_str()), ("body", "hi")]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );

    // The API offers the comments and the text.
    let ctx = pdms_core::Ctx::user(
        pdms_core::auth::principal::load(&app.core.db, owner)
            .await
            .unwrap(),
        "o",
        pdms_audit::AuthMethod::Local,
        &Default::default(),
    );
    let (_, token) = api_tokens::create(&app.core, &ctx, "t", &["documents:read".into()], 10)
        .await
        .unwrap();
    let mut api = Browser::new(&app);
    for (path, needle) in [
        ("comments", "Looks fine."),
        ("text", "effective March 1, 2013"),
    ] {
        let r = api
            .send(
                Request::get(format!("/api/v1/documents/{doc}/{path}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert!(r.body.contains(needle), "{path}: {}", r.body);
    }
}

#[tokio::test]
async fn ocr_page_is_for_integration_managers() {
    let app = app().await;
    let (_, admin_secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let (_, staff_secret) = user(&app.core, "staff@x.org", None, None).await;
    let mut a = Browser::new(&app);
    a.login("admin@x.org", &admin_secret).await;
    let page = a.get("/admin/ocr").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<h1>Text extraction (OCR)</h1>"));
    assert!(page.body.contains("docker compose --profile ocr up -d"));
    assert!(
        !page.body.contains("Outlook"),
        "the Outlook sync is gone (D-087)"
    );
    // The old address leads to the new page.
    let old = a.get("/admin/outlook").await;
    assert_eq!(old.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(old.location(), "/admin/ocr");
    let mut s = Browser::new(&app);
    s.login("staff@x.org", &staff_secret).await;
    assert_eq!(s.get("/admin/ocr").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn county_wide_document_page_hides_comment_forms_from_non_owners() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let (owner, owner_secret) = user(&app.core, "owner@x.org", Some(dept), None).await;
    let (_, col_secret) = user(&app.core, "col@x.org", Some(dept), None).await;
    let t = pdms_core::documents::types::type_by_key(&app.core.db, "interlocal_agreement")
        .await
        .unwrap()
        .id;
    let doc = pdms_core::documents::create(
        &app.core,
        &system(),
        pdms_core::documents::NewDocument {
            visibility: pdms_authz::Visibility::CountyWide,
            metadata: pdms_core::documents::MetadataInput {
                title: "Public notice".into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: pdms_core::documents::DocumentStatus::Active,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    let form = format!(r#"action="/documents/{doc}/comments""#);
    let sentence = "Only the owner and administrators can comment on County-wide documents.";

    let mut o = Browser::new(&app);
    o.login("owner@x.org", &owner_secret).await;
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains(&form) && !page.body.contains(sentence));

    let mut c = Browser::new(&app);
    c.login("col@x.org", &col_secret).await;
    let page = c.get(&format!("/documents/{doc}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(sentence), "{}", page.body);
    assert!(!page.body.contains(&form));
    let r = c
        .post(
            &format!("/documents/{doc}/comments"),
            &[("_csrf", page.csrf().as_str()), ("body", "hello")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}
