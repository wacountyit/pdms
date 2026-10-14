#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 4: records request packages (acceptance test 1 end to end),
//! API tokens, webhooks and providers, the audit forwarder, reports, and
//! integrity verification (acceptance test 8 at the core level).

mod common;

use std::io::Read;
use std::sync::{Arc, Mutex};

use common::*;
use pdms_authz::{Assignee, Permission, Scope, Subject};
use pdms_core::api_tokens;
use pdms_core::config::{AuditForwardTarget, Config};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, VersionKind, types};
use pdms_core::exports::{self, PackageRequest, Versions};
use pdms_core::integrations::{self, webhooks};
use pdms_core::reports::{self, ReportParams};
use pdms_core::workflow::{self, StepDecision, StepKind, StepStatus, Submission};
use pdms_core::{Core, CoreError, acl, directory, integrity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.config).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

async fn grant(core: &Core, user: Uuid, dept: Uuid, p: Permission) {
    acl::grant(
        &core.db,
        &system(),
        Subject::User(user),
        Scope::Department(dept),
        p,
        None,
        "test",
    )
    .await
    .unwrap();
}

async fn role(core: &Core, user: Uuid, key: &str, dept: Option<Uuid>) {
    let r = directory::role_id_by_key(&core.db, key).await.unwrap();
    directory::assign_role(&core.db, &system(), user, r, dept)
        .await
        .unwrap();
}

async fn new_user(core: &Core, name: &str, dept: Option<Uuid>) -> Uuid {
    directory::create_user(&core.db, &system(), name, &format!("{name}@x.org"), dept)
        .await
        .unwrap()
}

fn read_zip(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut out = std::collections::BTreeMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut b = Vec::new();
        f.read_to_end(&mut b).unwrap();
        out.insert(f.name().to_owned(), b);
    }
    out
}

async fn download(core: &Core, ctx: &pdms_core::Ctx, id: Uuid) -> Vec<u8> {
    use futures_util::StreamExt;
    let (_, mut s) = exports::open(core, ctx, id).await.unwrap();
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    out
}

/// Acceptance test 1, end to end.
#[tokio::test]
async fn audit_scenario_from_upload_to_records_requests_export() {
    let (core, db) = core().await;
    let (user_a, dept) = user_in_new_department(&core, "OPS", "a@x.org").await;
    let user_b = new_user(&core, "b", Some(dept)).await;
    let user_c = new_user(&core, "c", Some(dept)).await;
    let counsel = new_user(&core, "counsel", None).await;
    let approver = new_user(&core, "approver", Some(dept)).await;
    let admin = new_user(&core, "admin", None).await;
    let officer = new_user(&core, "officer", None).await;
    for u in [user_a, user_b, user_c] {
        grant(&core, u, dept, Permission::Edit).await;
        grant(&core, u, dept, Permission::Create).await;
    }
    grant(&core, user_b, dept, Permission::ManagePermissions).await;
    role(&core, counsel, "legal_counsel", None).await;
    role(&core, approver, "department_head", Some(dept)).await;
    role(&core, admin, "admin", None).await;
    role(&core, officer, "records_officer", None).await;

    // User A uploads an agreement.
    let a = as_user(&core, user_a).await;
    let agreement = types::type_by_key(&core.db, "partnership_agreement")
        .await
        .unwrap()
        .id;
    let mut meta = MetadataInput {
        title: "Equipment maintenance agreement".into(),
        document_type_id: agreement,
        department_id: dept,
        execution_date: "2013-02-12".parse().ok(),
        renewal_date: "2027-01-01".parse().ok(),
        ..Default::default()
    };
    let doc = documents::create(
        &core,
        &a,
        NewDocument {
            metadata: meta.clone(),
            owner_id: user_a,
            status: DocumentStatus::Active,
            tags: vec![],
            file: Some(upload(&core, "agreement-2013.pdf", &pdf("original")).await),
            note: String::new(),
        },
    )
    .await
    .unwrap();

    // User B changes the renewal date and the owner.
    let b = as_user(&core, user_b).await;
    let rv = documents::view(&core, &b, doc)
        .await
        .unwrap()
        .document
        .row_version;
    meta.renewal_date = "2028-01-01".parse().ok();
    documents::update(&core, &b, doc, meta.clone(), rv)
        .await
        .unwrap();
    documents::transfer_owner(&core, &b, doc, user_c, "C now manages it")
        .await
        .unwrap();

    // User C uploads an amended version and submits it; legal counsel and the
    // department head approve.
    let c = as_user(&core, user_c).await;
    documents::add_version(
        &core,
        &c,
        doc,
        upload(&core, "agreement-amendment-2014.pdf", &pdf("amendment")).await,
        VersionKind::Amendment,
        "Amendment 1",
        "2014-06-01".parse().ok(),
    )
    .await
    .unwrap();
    workflow::submit(
        &core,
        &c,
        doc,
        Submission {
            approvers: vec![Assignee::User(approver)],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for (who, kind) in [
        (counsel, StepKind::LegalReview),
        (approver, StepKind::Approval),
    ] {
        let p = workflow::panel(&core, &system(), doc).await.unwrap();
        let step = p.workflows[0]
            .steps
            .iter()
            .find(|s| s.kind == kind && s.status == StepStatus::Pending)
            .unwrap()
            .id;
        workflow::decide_step(
            &core,
            &as_user(&core, who).await,
            step,
            StepDecision::Approve,
            "ok",
        )
        .await
        .unwrap();
    }

    // An administrator changes the ACL.
    let adm = as_user(&core, admin).await;
    documents::share(
        &core,
        &adm,
        doc,
        Subject::User(officer),
        Permission::View,
        None,
        "records request",
    )
    .await
    .unwrap();

    // An authorized user exports it for a records request.
    let o = as_user(&core, officer).await;
    let pkg = exports::build(
        &core,
        &o,
        PackageRequest {
            request_reference: "REQ-2026-014".into(),
            document_ids: vec![doc],
            fields: exports::FIELDS
                .iter()
                .map(|(k, _)| (*k).to_owned())
                .collect(),
            versions: Versions::All,
            include_audit: true,
            note: "Requested by the Gazette".into(),
        },
    )
    .await
    .unwrap();

    // The timeline shows every event in order, with who, when, and before/after values.
    let timeline = documents::timeline(&core, &adm, doc).await.unwrap();
    let actions: Vec<&str> = timeline.iter().map(|e| e.event.action.as_str()).collect();
    let position = |a: &str| {
        actions
            .iter()
            .position(|x| *x == a)
            .unwrap_or_else(|| panic!("{a} missing from {actions:?}"))
    };
    let order = [
        "document.created",
        "document.metadata_updated",
        "document.owner_transferred",
        "workflow.submitted",
        "workflow.step_approved",
        "workflow.approved",
        "acl.granted",
        "document.exported",
    ];
    let positions: Vec<usize> = order.iter().map(|a| position(a)).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{actions:?}");
    assert!(
        timeline
            .windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].event.occurred_at <= w[1].event.occurred_at)
    );
    let edit = &timeline[position("document.metadata_updated")].event;
    assert_eq!(edit.actor.user_id, Some(user_b));
    let changes = format!("{:?}", edit.changes);
    assert!(
        changes.contains("2027-01-01") && changes.contains("2028-01-01"),
        "{changes}"
    );
    let exported = &timeline[position("document.exported")].event;
    assert_eq!(exported.actor.user_id, Some(officer));
    assert!(format!("{:?}", exported.context).contains("REQ-2026-014"));

    // Version history keeps both files.
    let versions = documents::versions(&core, &adm, doc).await.unwrap();
    let files: Vec<_> = versions.iter().filter(|v| v.file_changed).collect();
    assert_eq!(files.len(), 2);

    // The package: README, manifest, metadata, both files, audit history, checksums.
    let zip_bytes = download(&core, &o, pkg).await;
    let info = &exports::list(&core, &o).await.unwrap()[0];
    assert_eq!(hex::encode(sha2_digest(&zip_bytes)), info.package_sha256);
    let entries = read_zip(&zip_bytes);
    let manifest: serde_json::Value = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert_eq!(manifest["request_reference"], "REQ-2026-014");
    assert_eq!(
        hex::encode(sha2_digest(&entries["manifest.json"])),
        info.manifest_sha256
    );
    let ext = manifest["documents"][0]["external_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        manifest["documents"][0]["files"].as_array().unwrap().len(),
        2
    );
    assert!(
        entries
            .keys()
            .any(|k| k.starts_with(&format!("documents/{ext}/files/v1_")))
    );
    let history =
        String::from_utf8(entries[&format!("documents/{ext}/audit-history.csv")].clone()).unwrap();
    assert!(history.contains("document.owner_transferred"));
    let sums = String::from_utf8(entries["SHA256SUMS"].clone()).unwrap();
    for line in sums.lines() {
        let (sha, path) = line.split_once("  ").unwrap();
        assert_eq!(hex::encode(sha2_digest(&entries[path])), sha, "{path}");
    }
    assert!(sums.contains("manifest.json") && sums.contains("README.txt"));

    // Audit rows cannot be altered through the app's role; verify-audit detects tampering.
    for sql in [
        "UPDATE audit_log SET action = 'x.y' WHERE seq = 1",
        "DELETE FROM audit_log WHERE seq = 1",
    ] {
        assert!(sqlx::query(sql).execute(&core.db).await.is_err(), "{sql}");
    }
    assert!(integrity::verify(&core, true).await.unwrap().is_ok());
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER USER")
        .execute(&db.superuser)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE audit_log SET object_label = 'tampered' WHERE action = 'document.exported'",
    )
    .execute(&db.superuser)
    .await
    .unwrap();
    let r = integrity::verify(&core, true).await.unwrap();
    assert!(
        r.problems.iter().any(|p| p.kind == "audit"),
        "{:?}",
        r.problems
    );
}

fn sha2_digest(b: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(b).into()
}

#[tokio::test]
async fn exports_follow_acls_and_capabilities() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "HR", "owner@x.org").await;
    let officer = new_user(&core, "officer", None).await;
    role(&core, officer, "records_officer", None).await;
    let clerk = new_user(&core, "clerk", Some(dept)).await;
    let policy = types::type_by_key(&core.db, "policy").await.unwrap().id;
    let mk = |title: &'static str| {
        let core = core.clone();
        async move {
            documents::create(
                &core,
                &system(),
                NewDocument {
                    metadata: MetadataInput {
                        title: title.into(),
                        document_type_id: policy,
                        department_id: dept,
                        ..Default::default()
                    },
                    owner_id: owner,
                    status: DocumentStatus::Active,
                    tags: vec![],
                    file: None,
                    note: String::new(),
                },
            )
            .await
            .unwrap()
        }
    };
    let public = mk("Leave policy").await;
    let secret = mk("Investigation procedures").await;
    documents::set_access_settings(&core, &system(), secret, true, true)
        .await
        .unwrap();
    let o = as_user(&core, officer).await;
    let found = exports::candidates(&core, &o, &Default::default())
        .await
        .unwrap();
    assert!(found.iter().any(|d| d.id == public));
    assert!(
        !found.iter().any(|d| d.id == secret),
        "confidential excluded from broad exports"
    );
    let req = |ids: Vec<Uuid>| PackageRequest {
        request_reference: "REQ-1".into(),
        document_ids: ids,
        fields: vec!["status".into()],
        versions: Versions::Current,
        include_audit: false,
        note: String::new(),
    };
    assert!(matches!(
        exports::build(&core, &o, req(vec![public, secret])).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        exports::build(
            &core,
            &o,
            PackageRequest {
                request_reference: " ".into(),
                ..req(vec![public])
            }
        )
        .await,
        Err(CoreError::Validation(_))
    ));
    let pkg = exports::build(&core, &o, req(vec![public])).await.unwrap();
    // Only records officers build packages; only the requester (or an auditor) downloads.
    let c = as_user(&core, clerk).await;
    assert!(matches!(
        exports::build(&core, &c, req(vec![public])).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        exports::open(&core, &c, pkg).await,
        Err(CoreError::Forbidden(_))
    ));
    let auditor = new_user(&core, "auditor", None).await;
    role(&core, auditor, "auditor", None).await;
    assert!(
        exports::open(&core, &as_user(&core, auditor).await, pkg)
            .await
            .is_ok()
    );
    let entries = read_zip(&download(&core, &o, pkg).await);
    let manifest: serde_json::Value = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert_eq!(
        manifest["options"]["fields"],
        serde_json::json!(["title", "external_id", "status"])
    );
    let actions = audit_actions(&core).await;
    for a in [
        "export_package.created",
        "document.exported",
        "export_package.downloaded",
        "access.denied",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a}");
    }
}

#[tokio::test]
async fn api_tokens_act_as_their_user_within_scopes() {
    let (core, _db) = core().await;
    let (user, _) = user_in_new_department(&core, "IT", "u@x.org").await;
    let ctx = as_user(&core, user).await;
    let client = pdms_core::ClientInfo::default();
    assert!(
        api_tokens::create(&core, &ctx, "x", &["bogus".into()], 30)
            .await
            .is_err()
    );
    assert!(
        api_tokens::create(&core, &ctx, "x", &["documents:read".into()], 9999)
            .await
            .is_err()
    );
    let (id, secret) = api_tokens::create(
        &core,
        &ctx,
        "an ECM system sync",
        &["documents:read".into()],
        30,
    )
    .await
    .unwrap();
    assert!(secret.starts_with("pdms_"));
    let auth = api_tokens::authenticate(&core, &secret, &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(auth.user_id, user);
    assert!(auth.has_scope("documents:read") && !auth.has_scope("documents:write"));
    assert_eq!(auth.ctx.actor.api_token_id, Some(id));
    assert!(
        api_tokens::authenticate(&core, "pdms_wrong", &client)
            .await
            .unwrap()
            .is_none()
    );
    // Use is audited at most once an hour.
    api_tokens::authenticate(&core, &secret, &client)
        .await
        .unwrap();
    let used = audit_actions(&core)
        .await
        .iter()
        .filter(|a| *a == "api_token.used")
        .count();
    assert_eq!(used, 1);
    // Deactivating the user stops the token at once; so does revocation.
    directory::set_user_status(
        &core.db,
        &system(),
        user,
        pdms_core::model::UserStatus::Disabled,
    )
    .await
    .unwrap();
    assert!(
        api_tokens::authenticate(&core, &secret, &client)
            .await
            .unwrap()
            .is_none()
    );
    directory::set_user_status(
        &core.db,
        &system(),
        user,
        pdms_core::model::UserStatus::Active,
    )
    .await
    .unwrap();
    assert!(
        api_tokens::authenticate(&core, &secret, &client)
            .await
            .unwrap()
            .is_some()
    );
    let other = new_user(&core, "other", None).await;
    assert!(
        api_tokens::revoke(&core, &as_user(&core, other).await, id)
            .await
            .is_err(),
        "not theirs"
    );
    api_tokens::revoke(&core, &ctx, id).await.unwrap();
    assert!(
        api_tokens::authenticate(&core, &secret, &client)
            .await
            .unwrap()
            .is_none()
    );
    // Expired tokens are refused.
    let (id2, s2) = api_tokens::create(&core, &ctx, "short", &["calendar:read".into()], 1)
        .await
        .unwrap();
    sqlx::query("UPDATE api_tokens SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(id2)
        .execute(&core.db)
        .await
        .unwrap();
    assert!(
        api_tokens::authenticate(&core, &s2, &client)
            .await
            .unwrap()
            .is_none()
    );
    // The API can be turned off.
    let (off, _db2) = core_with(|c| c.api.enabled = false).await;
    assert!(
        api_tokens::authenticate(&off, &secret, &client)
            .await
            .unwrap()
            .is_none()
    );
}

/// A tiny HTTP endpoint that records requests and answers with `status`.
async fn endpoint(status: u16) -> (String, Arc<Mutex<Vec<(Vec<(String, String)>, Vec<u8>)>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, len) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let headers = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                    .collect();
                store
                    .lock()
                    .unwrap()
                    .push((headers, buf[head_end..].to_vec()));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}/hook"), seen)
}

async fn deliver_pending(core: &Core) -> Vec<Result<(), String>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM webhook_deliveries WHERE status IN ('pending', 'failed') ORDER BY created_at",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    let mut out = Vec::new();
    for id in ids {
        out.push(webhooks::deliver(core, id, "test").await);
    }
    out
}

#[tokio::test]
async fn webhooks_deliver_signed_events_the_service_user_may_see() {
    let (core, _db) = core_with(|c| {
        c.integrations.allow_http_webhooks = true;
        c.integrations.providers = vec!["log".into()];
    })
    .await;
    let (service, ops) = user_in_new_department(&core, "OPS", "svc@x.org").await;
    let hr = directory::create_department(&core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    let admin = new_user(&core, "admin", None).await;
    role(&core, admin, "admin", None).await;
    let adm = as_user(&core, admin).await;
    let (url, seen) = endpoint(200).await;
    // http is refused unless allowed; patterns are validated.
    let (strict, _db2) = core_with(|_| {}).await;
    let spec = |url: &str, events: &[&str]| webhooks::WebhookSpec {
        name: "Records system".into(),
        url: url.into(),
        events: events.iter().map(|s| (*s).to_owned()).collect(),
        acts_as_user_id: service,
        is_active: true,
    };
    assert!(
        webhooks::create(&strict, &system(), spec(&url, &["document.*"]))
            .await
            .is_err()
    );
    assert!(
        webhooks::create(&core, &adm, spec(&url, &["Document!"]))
            .await
            .is_err()
    );
    assert!(
        webhooks::create(
            &core,
            &as_user(&core, service).await,
            spec(&url, &["document.*"])
        )
        .await
        .is_err()
    );
    let (hook, secret) = webhooks::create(&core, &adm, spec(&url, &["document.*"]))
        .await
        .unwrap();

    integrations::dispatch_all(&core).await.unwrap(); // start the cursor
    let policy = types::type_by_key(&core.db, "policy").await.unwrap().id;
    for (dept, title) in [(ops, "Ops policy"), (hr, "HR secret policy")] {
        documents::create(
            &core,
            &system(),
            NewDocument {
                metadata: MetadataInput {
                    title: title.into(),
                    document_type_id: policy,
                    department_id: dept,
                    ..Default::default()
                },
                owner_id: service,
                status: DocumentStatus::Draft,
                tags: vec![],
                file: None,
                note: String::new(),
            },
        )
        .await
        .unwrap();
    }
    let r = integrations::dispatch_all(&core).await.unwrap();
    assert!(r.deliveries >= 2, "{r:?}");
    assert_eq!(
        integrations::dispatch_all(&core).await.unwrap().deliveries,
        0,
        "idempotent"
    );
    assert!(deliver_pending(&core).await.iter().all(Result::is_ok));
    let got = seen.lock().unwrap().clone();
    // The service user owns both documents, so it may see both (owners can
    // view); the next check covers a document it cannot see.
    let titles: Vec<String> = got
        .iter()
        .map(|(_, b)| {
            serde_json::from_slice::<serde_json::Value>(b).unwrap()["document"]["title"].to_string()
        })
        .collect();
    assert!(titles.iter().any(|t| t.contains("Ops policy")));
    for (headers, body) in &got {
        let h = |k: &str| {
            headers
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(h("x-pdms-event"), "document.created");
        let sig = h("x-pdms-signature");
        let ts: i64 = sig
            .trim_start_matches("t=")
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            sig,
            webhooks::signature(secret.as_bytes(), ts, body),
            "signed"
        );
        let v: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(v["id"].as_str().unwrap(), h("x-pdms-delivery"));
    }
    // The provider received everything too.
    let provider: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries WHERE provider = 'log' AND status = 'delivered'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert!(provider >= 2);

    // A document the service user cannot see is never sent.
    let hidden = documents::create(
        &core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: "Not for the service user".into(),
                document_type_id: policy,
                department_id: hr,
                ..Default::default()
            },
            owner_id: admin,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    integrations::dispatch_all(&core).await.unwrap();
    let to_hook: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries WHERE webhook_id = $1 AND payload->'document'->>'id' = $2",
    )
    .bind(hook)
    .bind(hidden.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(to_hook, 0);

    // A failing endpoint is retried and every attempt logged; redelivery works.
    let (bad_url, _) = endpoint(500).await;
    webhooks::update(&core, &adm, hook, spec(&bad_url, &["document.*"]))
        .await
        .unwrap();
    let doc = documents::create(
        &core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: "Another".into(),
                document_type_id: policy,
                department_id: ops,
                ..Default::default()
            },
            owner_id: service,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    integrations::dispatch_all(&core).await.unwrap();
    let results = deliver_pending(&core).await;
    assert!(
        results
            .iter()
            .any(|r| r.as_ref().is_err_and(|e| e.contains("HTTP 500")))
    );
    let failed = webhooks::deliveries(&core, &adm, Some(hook), 50)
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.status == "failed")
        .unwrap();
    assert_eq!(failed.last_status_code, Some(500));
    webhooks::update(&core, &adm, hook, spec(&url, &["document.*"]))
        .await
        .unwrap();
    webhooks::redeliver(&core, &adm, failed.id).await.unwrap();
    webhooks::deliver(&core, failed.id, "test").await.unwrap();
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webhook_delivery_attempts WHERE delivery_id = $1")
            .bind(failed.id)
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(attempts, 2);
    let _ = doc;
    let actions = audit_actions(&core).await;
    for a in ["webhook.created", "webhook.updated", "webhook.redelivered"] {
        assert!(actions.iter().any(|x| x == a), "{a}");
    }
}

#[tokio::test]
async fn audit_entries_are_forwarded_to_a_file_once() {
    let path = std::env::temp_dir().join(format!("pdms-forward-{}.jsonl", Uuid::new_v4()));
    let p2 = path.clone();
    let (core, _db) = core_with(move |c| {
        c.audit_forward.target = AuditForwardTarget::File;
        c.audit_forward.file_path = p2;
    })
    .await;
    integrations::forward::forward(&core).await.unwrap(); // starts at the current end
    user_in_new_department(&core, "IT", "x@x.org").await;
    let n = integrations::forward::forward(&core).await.unwrap();
    assert!(n >= 2);
    assert_eq!(integrations::forward::forward(&core).await.unwrap(), 0);
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), n);
    assert!(lines.iter().any(|l| l["action"] == "department.created"));
    assert!(
        lines
            .iter()
            .all(|l| l["entry_hash"].as_str().unwrap().len() == 64)
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn reports_respect_access_and_export_to_csv() {
    let (core, db) = core().await;
    let (alice, ops) = user_in_new_department(&core, "OPS", "alice@x.org").await;
    let (bob, hr) = user_in_new_department(&core, "HR", "bob@x.org").await;
    let policy = types::type_by_key(&core.db, "policy").await.unwrap().id;
    for (dept, owner, title) in [
        (ops, alice, "Operations"),
        (hr, bob, "HR"),
        (hr, bob, "HR 2"),
    ] {
        documents::create(
            &core,
            &system(),
            NewDocument {
                metadata: MetadataInput {
                    title: title.into(),
                    document_type_id: policy,
                    department_id: dept,
                    next_review_date: Some(
                        chrono::Utc::now().date_naive() + chrono::Duration::days(10),
                    ),
                    ..Default::default()
                },
                owner_id: owner,
                status: DocumentStatus::Active,
                tags: vec![],
                file: None,
                note: String::new(),
            },
        )
        .await
        .unwrap();
    }
    let a = as_user(&core, alice).await;
    let by_dept = reports::run(&core, &a, "by_department", &ReportParams::default())
        .await
        .unwrap();
    assert_eq!(by_dept.rows.len(), 1, "only what alice can see");
    assert_eq!(by_dept.rows[0][0], "OPS");
    let upcoming = reports::run(&core, &a, "upcoming", &ReportParams::default())
        .await
        .unwrap();
    assert_eq!(upcoming.rows.len(), 1);
    assert!(matches!(
        reports::run(&core, &a, "access_grants", &ReportParams::default()).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        !reports::available(&a)
            .iter()
            .any(|r| r.key == "access_grants")
    );

    // Departed owner shows up on the ownership and orphaned-account reports.
    directory::set_user_status(
        &core.db,
        &system(),
        bob,
        pdms_core::model::UserStatus::Departed,
    )
    .await
    .unwrap();
    let auditor = new_user(&core, "auditor", None).await;
    role(&core, auditor, "auditor", None).await;
    let au = as_user(&core, auditor).await;
    let orphaned = reports::run(&core, &au, "orphaned_accounts", &ReportParams::default())
        .await
        .unwrap();
    assert_eq!(orphaned.rows.len(), 1);
    assert_eq!(orphaned.rows[0][3], "2", "documents owned");
    sqlx::query("UPDATE acl_entries SET granted_at = now() - interval '400 days' WHERE subject_department_id IS NOT NULL")
        .execute(&db.superuser)
        .await
        .unwrap();
    let grants = reports::run(
        &core,
        &au,
        "access_grants",
        &ReportParams {
            days: Some(365),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        grants
            .rows
            .iter()
            .any(|r| r[10].contains("older than 365 days"))
    );
    let csv = String::from_utf8(
        reports::export_csv(&core, &au, "audit_summary", &ReportParams::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(csv.starts_with("Category,Action,Entries,Denied,Failed,People\r\n"));
    assert!(
        audit_actions(&core)
            .await
            .contains(&"report.exported".to_owned())
    );
}

#[tokio::test]
async fn verify_integrity_finds_missing_and_altered_files() {
    let (core, _db) = core().await;
    let (user, dept) = user_in_new_department(&core, "IT", "u@x.org").await;
    let policy = types::type_by_key(&core.db, "policy").await.unwrap().id;
    let mut ids = Vec::new();
    for name in ["a.pdf", "b.pdf"] {
        ids.push(
            documents::create(
                &core,
                &system(),
                NewDocument {
                    metadata: MetadataInput {
                        title: name.into(),
                        document_type_id: policy,
                        department_id: dept,
                        ..Default::default()
                    },
                    owner_id: user,
                    status: DocumentStatus::Active,
                    tags: vec![],
                    file: Some(upload(&core, name, &pdf(name)).await),
                    note: String::new(),
                },
            )
            .await
            .unwrap(),
        );
    }
    let r = integrity::verify(&core, true).await.unwrap();
    assert!(r.is_ok(), "{:?}", r.problems);
    assert_eq!(r.files_checked, 2);
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar("SELECT sha256 FROM blobs ORDER BY created_at")
        .fetch_all(&core.db)
        .await
        .unwrap();
    let path = |h: &[u8]| {
        core.config.storage.local_path.join(
            pdms_storage::BlobHash::from_slice(h)
                .unwrap()
                .relative_path(),
        )
    };
    // Same size, different content: only a re-hash notices.
    let p0 = path(&hashes[0]);
    let mut bytes = std::fs::read(&p0).unwrap();
    bytes[0] ^= 0xff;
    let mut perms = std::fs::metadata(&p0).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&p0, perms).unwrap();
    std::fs::write(&p0, &bytes).unwrap();
    std::fs::remove_file(path(&hashes[1])).unwrap();
    let fast = integrity::verify(&core, false).await.unwrap();
    assert_eq!(fast.problems.len(), 1, "{:?}", fast.problems);
    assert!(fast.problems[0].detail.contains("missing"));
    let full = integrity::verify(&core, true).await.unwrap();
    let details: Vec<&str> = full.problems.iter().map(|p| p.detail.as_str()).collect();
    assert!(
        details.iter().any(|d| d.contains("does not match")),
        "{details:?}"
    );
    assert!(details.iter().any(|d| d.contains("missing")));
    assert!(
        audit_actions(&core)
            .await
            .contains(&"integrity.verified".to_owned())
    );
}
