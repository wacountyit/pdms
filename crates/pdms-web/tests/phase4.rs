#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 4 through HTTP: the REST API (tokens, scopes, the read-only default,
//! request logging, rate limiting, OpenAPI), and the token, webhook, export,
//! and report pages.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::{acl, api_tokens, directory};
use support::*;
use uuid::Uuid;

async fn call(
    b: &mut Browser,
    method: &str,
    path: &str,
    token: Option<&str>,
    json: Option<&str>,
) -> Resp {
    let mut r = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = match json {
        Some(j) => {
            r = r.header(header::CONTENT_TYPE, "application/json");
            Body::from(j.to_owned())
        }
        None => Body::empty(),
    };
    b.send(r.body(body).unwrap()).await
}

fn json(r: &Resp) -> serde_json::Value {
    serde_json::from_str(&r.body).unwrap_or_else(|_| panic!("not JSON ({}): {}", r.status, r.body))
}

async fn doc(app: &App, dept: Uuid, owner: Uuid, title: &str, file: bool) -> Uuid {
    let t = types::type_by_key(&app.core.db, "policy").await.unwrap().id;
    let file = if file {
        let mut w =
            pdms_storage::spool::SpoolWriter::create(&app.core.config.storage.spool_path, 1 << 20)
                .await
                .unwrap();
        w.write(b"%PDF-1.4\n% hello\n%%EOF\n").await.unwrap();
        Some(pdms_core::documents::files::Upload {
            file: w.finish().await.unwrap(),
            file_name: "policy.pdf".into(),
        })
    } else {
        None
    };
    documents::create(
        &app.core,
        &system(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file,
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

async fn token(app: &App, user: Uuid, scopes: &[&str]) -> String {
    let ctx = pdms_core::Ctx::user(
        pdms_core::auth::principal::load(&app.core.db, user)
            .await
            .unwrap(),
        "t",
        pdms_audit::AuthMethod::Local,
        &Default::default(),
    );
    let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
    api_tokens::create(&app.core, &ctx, "test", &scopes, 30)
        .await
        .unwrap()
        .1
}

/// Acceptance test 2 over the API, plus scopes and the request log.
#[tokio::test]
async fn api_enforces_the_read_only_default_and_scopes() {
    let app = app().await;
    let ops = directory::create_department(&app.core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let hr = directory::create_department(&app.core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    let (newbie, _) = user(&app.core, "new@x.org", Some(ops), None).await;
    let (hr_owner, _) = user(&app.core, "hr@x.org", Some(hr), None).await;
    let mine = doc(&app, ops, hr_owner, "Ops policy", true).await;
    let theirs = doc(&app, hr, hr_owner, "HR policy", false).await;
    let mut b = Browser::new(&app);

    // No token, a bad token: 401 with a JSON body and WWW-Authenticate.
    let r = call(&mut b, "GET", "/api/v1/me", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&r)["error"], "unauthorized");
    assert!(r.headers.contains_key(header::WWW_AUTHENTICATE));
    assert_eq!(
        call(&mut b, "GET", "/api/v1/me", Some("pdms_nope"), None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    let t = token(&app, newbie, &["documents:read", "documents:write"]).await;
    let me = json(&call(&mut b, "GET", "/api/v1/me", Some(&t), None).await);
    assert_eq!(me["user_id"], newbie.to_string());

    // Sees only the department's documents, everywhere.
    let list = json(&call(&mut b, "GET", "/api/v1/documents", Some(&t), None).await);
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["title"], "Ops policy");
    let ext = list["items"][0]["external_id"].as_str().unwrap().to_owned();
    let r = call(
        &mut b,
        "GET",
        &format!("/api/v1/documents/{theirs}"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(json(&r)["error"], "forbidden");
    assert_eq!(
        call(
            &mut b,
            "GET",
            &format!("/api/v1/documents/{theirs}/file"),
            Some(&t),
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // By external ID; file download.
    let d = json(
        &call(
            &mut b,
            "GET",
            &format!("/api/v1/documents/{ext}"),
            Some(&t),
            None,
        )
        .await,
    );
    assert_eq!(d["id"], mine.to_string());
    assert_eq!(d["permissions"], serde_json::json!(["view"]));
    let f = call(
        &mut b,
        "GET",
        &format!("/api/v1/documents/{ext}/file"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(f.status, StatusCode::OK);
    assert!(f.body.starts_with("%PDF"));
    let v = json(
        &call(
            &mut b,
            "GET",
            &format!("/api/v1/documents/{mine}/versions"),
            Some(&t),
            None,
        )
        .await,
    );
    assert_eq!(v[0]["kind"], "initial");

    // Cannot edit anything: no create grant, no edit permission.
    let types = json(&call(&mut b, "GET", "/api/v1/document-types", Some(&t), None).await);
    assert!(
        types
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["key"] == "policy")
    );
    let body = format!(r#"{{"title":"Mine","document_type":"policy","department_id":"{ops}"}}"#);
    assert_eq!(
        call(&mut b, "POST", "/api/v1/documents", Some(&t), Some(&body))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let r = call(
        &mut b,
        "POST",
        &format!("/api/v1/documents/{mine}/status"),
        Some(&t),
        Some(r#"{"status":"archived"}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Scopes limit tokens even when the user could act.
    let read_only = token(&app, newbie, &["calendar:read"]).await;
    let r = call(&mut b, "GET", "/api/v1/documents", Some(&read_only), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(json(&r)["error"], "insufficient_scope");
    assert_eq!(
        call(&mut b, "GET", "/api/v1/calendar", Some(&read_only), None)
            .await
            .status,
        StatusCode::OK
    );
    // Audit search needs both the scope and the auditor capability.
    let audit_tok = token(&app, newbie, &["audit:read"]).await;
    assert_eq!(
        call(&mut b, "GET", "/api/v1/audit", Some(&audit_tok), None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    // With a create grant, writing works.
    acl::grant(
        &app.core.db,
        &system(),
        Subject::User(newbie),
        Scope::Department(ops),
        Permission::Create,
        None,
        "t",
    )
    .await
    .unwrap();
    let r = call(&mut b, "POST", "/api/v1/documents", Some(&t), Some(&body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let created = json(&r);
    assert!(created["external_id"].as_str().unwrap().starts_with("POL-"));
    let r = call(
        &mut b,
        "POST",
        &format!(
            "/api/v1/documents/{}/status",
            created["id"].as_str().unwrap()
        ),
        Some(&t),
        Some(r#"{"status":"active","note":"via API"}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let r = call(
        &mut b,
        "POST",
        &format!(
            "/api/v1/documents/{}/status",
            created["id"].as_str().unwrap()
        ),
        Some(&t),
        Some(r#"{"status":"overdue"}"#),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "people cannot set date-driven statuses"
    );

    // Every request is logged; use of the token is audited, with its ID.
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM api_request_log")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert!(logged >= 15, "{logged}");
    let unauth: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_request_log WHERE token_id IS NULL AND status = 401",
    )
    .fetch_one(&app.core.db)
    .await
    .unwrap();
    assert_eq!(unauth, 2);
    let with_token: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'document.viewed' AND api_token_id IS NOT NULL AND actor_auth_method = 'api_token'",
    )
    .fetch_one(&app.core.db)
    .await
    .unwrap();
    assert!(with_token >= 1);

    // Unknown endpoints are JSON 404s; the OpenAPI document is public.
    let r = call(&mut b, "GET", "/api/v1/nope", Some(&t), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(json(&r)["error"], "not_found");
    let spec = json(&call(&mut b, "GET", "/api/v1/openapi.json", None, None).await);
    assert!(spec["openapi"].as_str().unwrap().starts_with("3."));
    for p in [
        "/api/v1/documents",
        "/api/v1/documents/{id}/file",
        "/api/v1/calendar",
        "/api/v1/audit",
    ] {
        assert!(spec["paths"][p].is_object(), "{p}");
    }
    assert!(spec["components"]["securitySchemes"]["bearer"].is_object());
    let docs = b.get("/api/docs").await;
    assert_eq!(docs.status, StatusCode::OK);
    assert!(
        docs.body
            .contains("/api/v1/documents/{id}/versions/{version}/file")
    );
}

#[tokio::test]
async fn api_tokens_are_rate_limited_and_can_be_turned_off() {
    let mut cfg = config();
    cfg.api.requests_per_minute = 3;
    let app = app_with(cfg).await;
    let (u, _) = user(&app.core, "u@x.org", None, None).await;
    let t = token(&app, u, &["calendar:read"]).await;
    let mut b = Browser::new(&app);
    let mut statuses = Vec::new();
    for _ in 0..5 {
        statuses.push(
            call(&mut b, "GET", "/api/v1/me", Some(&t), None)
                .await
                .status,
        );
    }
    assert_eq!(&statuses[..3], &[StatusCode::OK; 3]);
    assert_eq!(statuses[4], StatusCode::TOO_MANY_REQUESTS);

    let mut off = config();
    off.api.enabled = false;
    let app = app_with(off).await;
    let mut b = Browser::new(&app);
    assert_eq!(
        call(&mut b, "GET", "/api/v1/me", Some(&t), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn token_webhook_export_and_report_pages() {
    let mut cfg = config();
    cfg.integrations.allow_http_webhooks = true;
    let app = app_with(cfg).await;
    let dept = directory::create_department(&app.core.db, &system(), "IT", "IT")
        .await
        .unwrap();
    let (staff, staff_secret) = user(&app.core, "staff@x.org", Some(dept), None).await;
    let (_, admin_secret) = user(&app.core, "admin@x.org", None, Some("admin")).await;
    let (_, officer_secret) = user(&app.core, "officer@x.org", None, Some("records_officer")).await;
    let d = doc(&app, dept, staff, "Network policy", true).await;

    // Any user manages their own tokens; the secret is shown once.
    let mut s = Browser::new(&app);
    s.login("staff@x.org", &staff_secret).await;
    let page = s.get("/account/tokens").await;
    assert!(page.body.contains("documents:read"));
    let r = s
        .post(
            "/account/tokens",
            &[
                ("_csrf", page.csrf().as_str()),
                ("name", "Script"),
                ("scope", "documents:read"),
                ("days", "30"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = s.get("/account/tokens").await;
    let secret = regex::Regex::new(r"(pdms_[A-Za-z0-9_-]{20,})")
        .unwrap()
        .captures(&page.body)
        .unwrap()[1]
        .to_owned();
    assert!(
        !s.get("/account/tokens").await.body.contains(&secret),
        "shown once"
    );
    let mut api = Browser::new(&app);
    assert_eq!(
        call(&mut api, "GET", "/api/v1/documents", Some(&secret), None)
            .await
            .status,
        StatusCode::OK
    );
    let revoke = regex::Regex::new(r"/account/tokens/([0-9a-f-]{36})/revoke")
        .unwrap()
        .captures(&page.body)
        .unwrap()[0]
        .to_owned();
    s.post(&revoke, &[("_csrf", page.csrf().as_str())]).await;
    assert_eq!(
        call(&mut api, "GET", "/api/v1/documents", Some(&secret), None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    // Staff cannot reach integration, export, or access-review pages.
    for path in [
        "/admin/webhooks",
        "/admin/api",
        "/exports",
        "/reports/access_grants",
    ] {
        assert_eq!(s.get(path).await.status, StatusCode::FORBIDDEN, "{path}");
    }
    // Reports: staff see only theirs; CSV downloads.
    let reports = s.get("/reports").await;
    assert!(
        reports.body.contains("Documents by department") && !reports.body.contains("Access review")
    );
    let rep = s.get("/reports/by_department").await;
    assert!(rep.body.contains("<td>IT</td>"), "{}", rep.body);
    let csv = s.get("/reports/ownership.csv").await;
    assert_eq!(csv.headers["content-type"], "text/csv; charset=utf-8");
    assert!(csv.body.contains("Network policy"));

    // Administrators manage webhooks and see every token and the request log.
    let mut a = Browser::new(&app);
    a.login("admin@x.org", &admin_secret).await;
    let page = a.get("/admin/webhooks").await;
    assert_eq!(page.status, StatusCode::OK);
    let staff_id = staff.to_string();
    let r = a
        .post(
            "/admin/webhooks",
            &[
                ("_csrf", page.csrf().as_str()),
                ("name", "Records platform"),
                ("url", "http://127.0.0.1:9/hook"),
                ("events", "document.*\nworkflow.approved"),
                ("acts_as_user_id", staff_id.as_str()),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let hook_page = a.get(r.location()).await;
    assert!(hook_page.body.contains("whsec_"), "secret shown once");
    assert!(hook_page.body.contains("workflow.approved"));
    let api_page = a.get("/admin/api").await;
    assert!(api_page.body.contains("staff@x.org") && api_page.body.contains("Revoked"));
    assert!(api_page.body.contains("GET /api/v1/documents"));

    // A records officer builds and downloads a package.
    let mut o = Browser::new(&app);
    o.login("officer@x.org", &officer_secret).await;
    let page = o.get("/exports?search=1&q=network").await;
    assert!(page.body.contains("Network policy"));
    let did = d.to_string();
    let r = o
        .post(
            "/exports",
            &[
                ("_csrf", page.csrf().as_str()),
                ("request_reference", "REQ-2026-001"),
                ("document", did.as_str()),
                ("versions", "all"),
                ("include_audit", "1"),
                ("field", "status"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = o.get("/exports").await;
    assert!(page.body.contains("Package built.") && page.body.contains("REQ-2026-001"));
    let link = regex::Regex::new(r"/exports/[0-9a-f-]{36}/download")
        .unwrap()
        .captures(&page.body)
        .unwrap()[0]
        .to_owned();
    let z = o.get(&link).await;
    assert_eq!(z.status, StatusCode::OK);
    assert_eq!(z.headers["content-type"], "application/zip");
    assert!(
        z.headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("records-request-REQ-2026-001")
    );
    // Someone else cannot download it.
    assert_eq!(s.get(&link).await.status, StatusCode::FORBIDDEN);
}

/// An HR department whose members may create and edit; returns (dept, user).
async fn hr_editor(app: &App) -> (Uuid, Uuid) {
    let hr = directory::create_department(&app.core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(
            &app.core.db,
            &system(),
            Subject::Department(hr),
            Scope::Department(hr),
            p,
            None,
            "t",
        )
        .await
        .unwrap();
    }
    let (uid, _) = user(&app.core, "alice@x.org", Some(hr), None).await;
    (hr, uid)
}

#[tokio::test]
async fn api_reports_and_accepts_visibility() {
    let app = app().await;
    let (hr, alice) = hr_editor(&app).await;
    let t = token(&app, alice, &["documents:read", "documents:write"]).await;
    let mut b = Browser::new(&app);
    let body = format!(
        r#"{{"title":"Leave policy","document_type":"policy","department_id":"{hr}","status":"active","visibility":"county_wide"}}"#
    );
    let r = call(&mut b, "POST", "/api/v1/documents", Some(&t), Some(&body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = json(&r)["id"].as_str().unwrap().to_owned();
    let d = json(
        &call(
            &mut b,
            "GET",
            &format!("/api/v1/documents/{id}"),
            Some(&t),
            None,
        )
        .await,
    );
    assert_eq!(d["visibility"], "county_wide");
    assert_eq!(d["is_confidential"], false);
    let list = json(&call(&mut b, "GET", "/api/v1/documents", Some(&t), None).await);
    assert_eq!(list["items"][0]["visibility"], "county_wide");

    // Someone in another department sees it through the API too.
    let ops = directory::create_department(&app.core.db, &system(), "OPS", "OPS")
        .await
        .unwrap();
    let (bob, _) = user(&app.core, "bob@x.org", Some(ops), None).await;
    let tb = token(&app, bob, &["documents:read"]).await;
    let theirs = json(&call(&mut b, "GET", "/api/v1/documents", Some(&tb), None).await);
    assert_eq!(theirs["total"], 1);
    assert_eq!(theirs["items"][0]["title"], "Leave policy");
}

#[tokio::test]
async fn api_rejects_unknown_visibility() {
    let app = app().await;
    let (hr, alice) = hr_editor(&app).await;
    let t = token(&app, alice, &["documents:write"]).await;
    let mut b = Browser::new(&app);
    for bad in ["public", "", "County-wide"] {
        let body = format!(
            r#"{{"title":"X","document_type":"policy","department_id":"{hr}","visibility":"{bad}"}}"#
        );
        let r = call(&mut b, "POST", "/api/v1/documents", Some(&t), Some(&body)).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad:?}: {}",
            r.body
        );
        assert!(r.body.contains("visibility"), "{bad:?}: {}", r.body);
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}
