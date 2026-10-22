#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 1 document behavior against a real database and blob store.

mod common;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use common::*;
use futures_util::StreamExt;
use pdms_authz::{Permission, Scope, Subject, Visibility};
use pdms_core::documents::fields::{self, FieldSpec, FieldType, SelectOption};
use pdms_core::documents::search::{self, CustomFilter, DocumentFilter};
use pdms_core::documents::{
    self, DocumentStatus, MetadataInput, NewDocument, RestorePart, VersionKind, types,
};
use pdms_core::{Core, CoreError, Ctx, acl, directory};
use uuid::Uuid;

fn date(s: &str) -> Option<NaiveDate> {
    Some(s.parse().unwrap())
}

async fn type_id(core: &Core, key: &str) -> Uuid {
    types::type_by_key(&core.db, key).await.unwrap().id
}

fn meta(type_id: Uuid, dept: Uuid, title: &str) -> MetadataInput {
    MetadataInput {
        title: title.into(),
        document_type_id: type_id,
        department_id: dept,
        ..Default::default()
    }
}

/// A department whose members may create and edit documents; returns (dept, member).
async fn editing_department(core: &Core, code: &str, email: &str) -> (Uuid, Uuid) {
    let (user, dept) = user_in_new_department(core, code, email).await;
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

async fn new_doc(
    core: &Core,
    ctx: &Ctx,
    input: MetadataInput,
    owner: Uuid,
    file: Option<&[u8]>,
) -> Uuid {
    let file = match file {
        Some(b) => Some(upload(core, "agreement.pdf", b).await),
        None => None,
    };
    documents::create(
        core,
        ctx,
        NewDocument {
            visibility: Default::default(),
            metadata: input,
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

async fn read_all(mut s: pdms_storage::ByteStream) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    out
}

async fn visible_ids(core: &Core, ctx: &Ctx, f: DocumentFilter) -> Vec<Uuid> {
    search::search(core, ctx, &f)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| i.id)
        .collect()
}

#[tokio::test]
async fn create_with_file_stores_versions_and_audits() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let agreement = type_id(&core, "interlocal_agreement").await;
    let mut input = meta(agreement, dept, "Facilities Maintenance agreement");
    input.effective_date = date("2013-05-01");
    let content = pdf("original 2013");
    let id = new_doc(&core, &ctx, input, alice, Some(&content)).await;

    let view = documents::view(&core, &ctx, id).await.unwrap();
    let d = &view.document;
    assert!(
        d.external_id.starts_with("ILA-") && d.external_id.ends_with("-000001"),
        "{}",
        d.external_id
    );
    assert_eq!(d.status, DocumentStatus::Active);
    assert_eq!(d.current_version, 1);
    let file = view.current_file.unwrap();
    assert_eq!(file.kind, VersionKind::Initial);
    assert_eq!(file.media_type.as_deref(), Some("application/pdf"));

    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(read_all(dl.stream).await, content);
    let acts = audit_actions(&core).await;
    for a in [
        "document.created",
        "document.file_uploaded",
        "document.viewed",
        "document.downloaded",
    ] {
        assert!(acts.contains(&a.to_owned()), "missing {a}");
    }
    assert!(pdms_audit::verify(&core.db).await.unwrap().is_intact());
}

#[tokio::test]
async fn read_only_default_and_direct_access_denials() {
    let (core, _db) = core().await;
    let (ops, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let (bob, _hr) = user_in_new_department(&core, "HR", "bob@x.org").await;
    let rita = directory::create_user(&core.db, &system(), "rita", "rita@x.org", Some(ops))
        .await
        .unwrap();
    // Rita is in Ops but the department grants are view (default) + create/edit; make a
    // read-only colleague by removing her from the edit grant's reach: use a second department.
    let other = type_id(&core, "other").await;
    let ctx_alice = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx_alice,
        meta(other, ops, "Ops policy"),
        alice,
        Some(&pdf("x")),
    )
    .await;

    // Bob (other department): no view, no search hit, no download, no edit — all 403.
    let ctx_bob = as_user(&core, bob).await;
    assert!(matches!(
        documents::view(&core, &ctx_bob, id).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        documents::open_file(&core, &ctx_bob, id, None).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        documents::versions(&core, &ctx_bob, id).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        documents::update(&core, &ctx_bob, id, meta(other, ops, "hijack"), 1).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        visible_ids(&core, &ctx_bob, DocumentFilter::default())
            .await
            .is_empty()
    );
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'access.denied' AND document_id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(denied, 4);

    // Rita (same department): can view and find it.
    let ctx_rita = as_user(&core, rita).await;
    assert!(documents::view(&core, &ctx_rita, id).await.is_ok());
    assert_eq!(
        visible_ids(&core, &ctx_rita, DocumentFilter::default()).await,
        vec![id]
    );
}

#[tokio::test]
async fn new_user_cannot_edit_own_department_documents_by_default() {
    let (core, _db) = core().await;
    let (user, dept) = user_in_new_department(&core, "HR", "new@x.org").await;
    let other = type_id(&core, "other").await;
    let id = new_doc(
        &core,
        &system(),
        meta(other, dept, "HR handbook"),
        user,
        None,
    )
    .await;
    // Owner rule aside: a *different* new user in the same department.
    let colleague = directory::create_user(&core.db, &system(), "c", "c@x.org", Some(dept))
        .await
        .unwrap();
    let ctx = as_user(&core, colleague).await;
    assert!(documents::view(&core, &ctx, id).await.is_ok());
    for perm_check in [
        documents::update(&core, &ctx, id, meta(other, dept, "x"), 1)
            .await
            .err(),
        documents::add_tag(&core, &ctx, id, "x").await.err(),
        documents::delete(&core, &ctx, id, "x").await.err(),
        documents::change_status(&core, &ctx, id, DocumentStatus::Archived, "")
            .await
            .err(),
        documents::transfer_owner(&core, &ctx, id, colleague, "")
            .await
            .err(),
    ] {
        assert!(
            matches!(perm_check, Some(CoreError::Forbidden(_))),
            "{perm_check:?}"
        );
    }
    let create = documents::create(
        &core,
        &ctx,
        NewDocument {
            visibility: Default::default(),
            metadata: meta(other, dept, "x"),
            owner_id: colleague,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await;
    assert!(matches!(create, Err(CoreError::Forbidden(_))));
}

#[tokio::test]
async fn edits_record_field_diffs_and_detect_conflicts() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let contract = type_id(&core, "contract").await;
    let mut input = meta(contract, dept, "Paving contract");
    input.renewal_date = date("2026-01-01");
    let id = new_doc(&core, &ctx, input.clone(), alice, None).await;

    input.renewal_date = date("2027-01-01");
    input.title = "Paving contract (renewed)".into();
    documents::update(&core, &ctx, id, input.clone(), 1)
        .await
        .unwrap();
    let changes: serde_json::Value = sqlx::query_scalar(
        "SELECT changes FROM audit_log WHERE action = 'document.metadata_updated'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    let changes = changes.as_array().unwrap();
    assert!(changes.contains(
        &serde_json::json!({"field": "renewal_date", "before": "2026-01-01", "after": "2027-01-01"})
    ));
    assert!(changes.contains(&serde_json::json!({"field": "title", "before": "Paving contract", "after": "Paving contract (renewed)"})));
    assert_eq!(changes.len(), 2, "unchanged fields are not recorded");

    // A stale editor (row_version 1) is rejected.
    input.title = "stale".into();
    assert!(matches!(
        documents::update(&core, &ctx, id, input, 1).await,
        Err(CoreError::Conflict(_))
    ));
    // Metadata edits are versioned.
    assert_eq!(documents::versions(&core, &ctx, id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn department_transfer_needs_create_in_target() {
    let (core, _db) = core().await;
    let (ops, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let (hr, _) = editing_department(&core, "HR", "hr@x.org").await;
    let ctx = as_user(&core, alice).await;
    let other = type_id(&core, "other").await;
    let id = new_doc(&core, &ctx, meta(other, ops, "Doc"), alice, None).await;
    let r = documents::update(&core, &ctx, id, meta(other, hr, "Doc"), 1).await;
    assert!(matches!(r, Err(CoreError::Forbidden(_))));
    documents::update(&core, &system(), id, meta(other, hr, "Doc"), 1)
        .await
        .unwrap();
    assert!(
        audit_actions(&core)
            .await
            .contains(&"document.department_transferred".to_owned())
    );
}

#[tokio::test]
async fn custom_field_changes_never_break_records_or_exports() {
    // Acceptance test 7: rename, add, deprecate.
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let contract = type_id(&core, "contract").await;
    let amount = fields::create(
        &core.db,
        &system(),
        "contract_amount",
        FieldType::Number,
        Some(contract),
        FieldSpec {
            label: "Amount".into(),
            required: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let vendor = fields::create(
        &core.db,
        &system(),
        "vendor_class",
        FieldType::SingleSelect,
        None,
        FieldSpec {
            label: "Vendor class".into(),
            options: vec![
                SelectOption {
                    value: "local".into(),
                    label: "Local".into(),
                },
                SelectOption {
                    value: "state".into(),
                    label: "State".into(),
                },
            ],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ctx = as_user(&core, alice).await;
    let mut input = meta(contract, dept, "Mowing");
    input.custom = BTreeMap::from([
        ("contract_amount".into(), vec!["$12,500.00".into()]),
        ("vendor_class".into(), vec!["local".into()]),
    ]);
    let id = new_doc(&core, &ctx, input.clone(), alice, None).await;
    // Missing required value is rejected.
    let mut missing = meta(contract, dept, "No amount");
    missing.custom.clear();
    let r = documents::create(
        &core,
        &ctx,
        NewDocument {
            visibility: Default::default(),
            metadata: missing,
            owner_id: alice,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await;
    assert!(matches!(r, Err(CoreError::Validation(m)) if m.contains("Amount is required")));

    // Rename the label (new version), drop an option, add a new required field, deprecate one.
    fields::update(
        &core.db,
        &system(),
        amount,
        FieldSpec {
            label: "Contract value".into(),
            required: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    fields::update(
        &core.db,
        &system(),
        vendor,
        FieldSpec {
            label: "Vendor class".into(),
            options: vec![SelectOption {
                value: "state".into(),
                label: "State".into(),
            }],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    fields::create(
        &core.db,
        &system(),
        "fund_code",
        FieldType::Text,
        Some(contract),
        FieldSpec {
            label: "Fund code".into(),
            required: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    fields::set_deprecated(&core.db, &system(), vendor, true)
        .await
        .unwrap();
    assert_eq!(fields::history(&core.db, amount).await.unwrap().len(), 2);

    // The record still loads with its original values.
    let view = documents::view(&core, &ctx, id).await.unwrap();
    assert_eq!(view.document.custom_fields["contract_amount"], "12500");
    assert_eq!(view.document.custom_fields["vendor_class"], "local");
    // Export includes old and new columns; retired option shows clearly.
    let auditor_ctx = system();
    acl::grant(
        &core.db,
        &system(),
        Subject::User(alice),
        Scope::Department(dept),
        Permission::Export,
        None,
        "t",
    )
    .await
    .unwrap();
    let csv = String::from_utf8(
        search::export_csv(
            &core,
            &as_user(&core, alice).await,
            &DocumentFilter::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(csv.contains("Contract value [contract_amount]"));
    assert!(csv.contains("Vendor class [vendor_class] (deprecated)"));
    assert!(csv.contains("Fund code [fund_code]"));
    assert!(
        csv.contains("\"12500\"") && csv.contains("local (retired option)"),
        "{csv}"
    );
    let _ = auditor_ctx;
    // Editing the old record now requires the new field but keeps the deprecated value.
    input
        .custom
        .insert("fund_code".into(), vec!["GF-100".into()]);
    input.custom.remove("vendor_class");
    documents::update(&core, &ctx, id, input, 1).await.unwrap();
    let view = documents::view(&core, &ctx, id).await.unwrap();
    assert_eq!(
        view.document.custom_fields["vendor_class"], "local",
        "deprecated value kept"
    );
    assert_eq!(view.document.custom_fields["fund_code"], "GF-100");
}

#[tokio::test]
async fn tags_search_and_filters() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let other = type_id(&core, "other").await;
    let a = new_doc(
        &core,
        &ctx,
        meta(other, dept, "Equipment inspection agreement"),
        alice,
        None,
    )
    .await;
    let b = new_doc(
        &core,
        &ctx,
        meta(other, dept, "Gravel supply contract"),
        alice,
        None,
    )
    .await;
    let c = new_doc(
        &core,
        &ctx,
        meta(other, dept, "Holiday schedule"),
        alice,
        None,
    )
    .await;
    // Tags are curated (D-088): an editor picks from what an administrator
    // set up, and cannot invent a new one.
    for name in ["Capital Projects", "Funding: State"] {
        pdms_core::documents::tags::create_curated(&core.db, &system(), name, "#1f6feb", "")
            .await
            .unwrap();
    }
    assert!(
        documents::add_tag(&core, &ctx, a, "Made up here")
            .await
            .is_err(),
        "free-form tags are off"
    );
    documents::add_tag(&core, &ctx, a, "Capital Projects")
        .await
        .unwrap();
    documents::add_tag(&core, &ctx, a, "Funding: State")
        .await
        .unwrap();
    documents::add_tag(&core, &ctx, b, "Funding: State")
        .await
        .unwrap();
    let tags = pdms_core::documents::tags::list(&core.db).await.unwrap();
    let id_of = |name: &str| tags.iter().find(|t| t.name == name).unwrap().id;
    let (capital, state) = (id_of("Capital Projects"), id_of("Funding: State"));

    let mut f = DocumentFilter {
        tags: vec![capital, state],
        all_tags: false,
        ..Default::default()
    };
    let mut any = visible_ids(&core, &ctx, f.clone()).await;
    any.sort();
    let mut expect = vec![a, b];
    expect.sort();
    assert_eq!(any, expect, "OR");
    f.all_tags = true;
    assert_eq!(visible_ids(&core, &ctx, f).await, vec![a], "AND");

    // Full-text search over title and tags (with stemming).
    let q = |s: &str| DocumentFilter {
        q: Some(s.into()),
        ..Default::default()
    };
    assert_eq!(visible_ids(&core, &ctx, q("inspections")).await, vec![a]);
    assert_eq!(
        visible_ids(&core, &ctx, q("capital")).await,
        vec![a],
        "by tag"
    );
    let ext = documents::view(&core, &ctx, c)
        .await
        .unwrap()
        .document
        .external_id;
    assert_eq!(
        visible_ids(&core, &ctx, q(&ext)).await,
        vec![c],
        "by external ID"
    );

    // Removing a tag is reflected in search and audited with before/after.
    documents::remove_tag(&core, &ctx, a, capital)
        .await
        .unwrap();
    assert!(visible_ids(&core, &ctx, q("capital")).await.is_empty());
    let acts = audit_actions(&core).await;
    assert!(
        acts.contains(&"document.tag_added".to_owned())
            && acts.contains(&"document.tag_removed".to_owned())
    );
}

#[tokio::test]
async fn custom_field_filter_and_sort() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let other = type_id(&core, "other").await;
    fields::create(
        &core.db,
        &system(),
        "amount",
        FieldType::Number,
        None,
        FieldSpec {
            label: "Amount".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ctx = as_user(&core, alice).await;
    let mut ids = Vec::new();
    for (title, amt) in [("A", "900"), ("B", "50"), ("C", "10000")] {
        let mut m = meta(other, dept, title);
        m.custom.insert("amount".into(), vec![amt.into()]);
        ids.push(new_doc(&core, &ctx, m, alice, None).await);
    }
    let f = DocumentFilter {
        custom: Some(CustomFilter {
            key: "amount".into(),
            min: Some("100".into()),
            ..Default::default()
        }),
        sort: search::SortField::Custom("amount".into()),
        descending: false,
        ..Default::default()
    };
    assert_eq!(
        visible_ids(&core, &ctx, f).await,
        vec![ids[0], ids[2]],
        "numeric, not lexical"
    );
}

#[tokio::test]
async fn restricted_documents_and_single_document_sharing() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let colleague = directory::create_user(&core.db, &system(), "c", "c@x.org", Some(dept))
        .await
        .unwrap();
    let (outsider, _) = user_in_new_department(&core, "OPS", "o@x.org").await;
    let (admin, _) = user_in_new_department(&core, "IT", "admin@x.org").await;
    let admin_role = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), admin, admin_role, None)
        .await
        .unwrap();
    let other = type_id(&core, "other").await;
    let ctx_alice = as_user(&core, alice).await;
    let secret = new_doc(
        &core,
        &ctx_alice,
        meta(other, dept, "Personnel investigation"),
        alice,
        None,
    )
    .await;
    let public = new_doc(
        &core,
        &ctx_alice,
        meta(other, dept, "Leave policy"),
        alice,
        None,
    )
    .await;

    // Owner gets view/edit but not manage_permissions; an admin (manage_permissions globally) makes it Restricted.
    let ctx_admin = as_user(&core, admin).await;
    documents::set_visibility(
        &core,
        &ctx_admin,
        secret,
        pdms_authz::Visibility::Restricted,
    )
    .await
    .unwrap();
    let ctx_col = as_user(&core, colleague).await;
    assert_eq!(
        visible_ids(&core, &ctx_col, DocumentFilter::default()).await,
        vec![public]
    );
    assert!(matches!(
        documents::view(&core, &ctx_col, secret).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        matches!(
            documents::view(&core, &as_user(&core, admin).await, secret).await,
            Err(CoreError::Forbidden(_))
        ),
        "admin too"
    );
    assert!(
        documents::view(&core, &ctx_alice, secret).await.is_ok(),
        "owner still sees it"
    );

    // Share one document with an outsider without exposing the rest of the department.
    let ctx_out = as_user(&core, outsider).await;
    let grant = documents::share(
        &core,
        &ctx_admin,
        public,
        Subject::User(outsider),
        Permission::View,
        None,
        "inter-department review",
    )
    .await
    .unwrap();
    assert_eq!(
        visible_ids(
            &core,
            &as_user(&core, outsider).await,
            DocumentFilter::default()
        )
        .await,
        vec![public]
    );
    assert!(documents::view(&core, &ctx_out, secret).await.is_err());
    // Revocation applies to the very next check.
    documents::unshare(&core, &ctx_admin, public, grant)
        .await
        .unwrap();
    assert!(matches!(
        documents::view(&core, &as_user(&core, outsider).await, public).await,
        Err(CoreError::Forbidden(_))
    ));
    let acts = audit_actions(&core).await;
    for a in ["document.visibility_changed", "acl.granted", "acl.revoked"] {
        assert!(acts.contains(&a.to_owned()), "missing {a}");
    }
    // Grant/revoke events land on the document's timeline.
    let on_doc: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE document_id = $1 AND action LIKE 'acl.%'",
    )
    .bind(public)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(on_doc, 2);
}

#[tokio::test]
async fn status_transitions_are_validated() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let other = type_id(&core, "other").await;
    let id = documents::create(
        &core,
        &ctx,
        NewDocument {
            visibility: Default::default(),
            metadata: meta(other, dept, "Draft"),
            owner_id: alice,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    documents::change_status(&core, &ctx, id, DocumentStatus::InReview, "ready")
        .await
        .unwrap();
    // Approving needs `approve`, which alice lacks.
    assert!(matches!(
        documents::change_status(&core, &ctx, id, DocumentStatus::Active, "").await,
        Err(CoreError::Forbidden(_))
    ));
    // Date-driven statuses cannot be set by people.
    assert!(matches!(
        documents::change_status(&core, &ctx, id, DocumentStatus::Overdue, "").await,
        Err(CoreError::Validation(_))
    ));
    acl::grant(
        &core.db,
        &system(),
        Subject::User(alice),
        Scope::Department(dept),
        Permission::Approve,
        None,
        "t",
    )
    .await
    .unwrap();
    documents::change_status(
        &core,
        &as_user(&core, alice).await,
        id,
        DocumentStatus::Active,
        "approved",
    )
    .await
    .unwrap();
    let changes: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT changes FROM audit_log WHERE action = 'document.status_changed' ORDER BY seq",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(
        changes[1],
        serde_json::json!([{"field": "status", "before": "in_review", "after": "active"}])
    );
}

#[tokio::test]
async fn ownership_transfer_requires_manage_permissions_and_is_audited() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let bob = directory::create_user(&core.db, &system(), "Bob", "bob@x.org", Some(dept))
        .await
        .unwrap();
    let other = type_id(&core, "other").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, meta(other, dept, "Doc"), alice, None).await;
    assert!(matches!(
        documents::transfer_owner(&core, &ctx, id, bob, "").await,
        Err(CoreError::Forbidden(_))
    ));
    acl::grant(
        &core.db,
        &system(),
        Subject::User(alice),
        Scope::Department(dept),
        Permission::ManagePermissions,
        None,
        "t",
    )
    .await
    .unwrap();
    documents::transfer_owner(&core, &as_user(&core, alice).await, id, bob, "reassigned")
        .await
        .unwrap();
    let changes: serde_json::Value = sqlx::query_scalar(
        "SELECT changes FROM audit_log WHERE action = 'document.owner_transferred'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(
        changes,
        serde_json::json!([{"field": "owner", "before": "alice@x.org", "after": "Bob"}])
    );
}

#[tokio::test]
async fn deletion_respects_retention_and_legal_hold() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    acl::grant(
        &core.db,
        &system(),
        Subject::User(alice),
        Scope::Department(dept),
        Permission::Delete,
        None,
        "t",
    )
    .await
    .unwrap();
    let ctx = as_user(&core, alice).await;
    let policy = new_doc(
        &core,
        &ctx,
        meta(type_id(&core, "policy").await, dept, "Permanent policy"),
        alice,
        None,
    )
    .await;
    let mut cmeta = meta(type_id(&core, "contract").await, dept, "Expired contract");
    cmeta.expiration_date = date("2015-01-01");
    let contract = new_doc(&core, &ctx, cmeta, alice, None).await;
    let other = new_doc(
        &core,
        &ctx,
        meta(type_id(&core, "other").await, dept, "Scratch"),
        alice,
        None,
    )
    .await;

    let r = documents::delete(&core, &ctx, policy, "cleanup").await;
    assert!(matches!(r, Err(CoreError::Conflict(m)) if m.contains("permanently")));
    // Contract expired 2015 + 5 years → deletable, unless on legal hold.
    documents::set_retention(&core, &system(), contract, true, None, "litigation")
        .await
        .unwrap();
    assert!(
        matches!(documents::delete(&core, &ctx, contract, "x").await, Err(CoreError::Conflict(m)) if m.contains("legal hold"))
    );
    documents::set_retention(&core, &system(), contract, false, None, "released")
        .await
        .unwrap();
    documents::delete(&core, &ctx, contract, "past retention")
        .await
        .unwrap();
    documents::delete(&core, &ctx, other, "created in error")
        .await
        .unwrap();

    // Deleted documents disappear everywhere, and leave a tombstone.
    assert!(matches!(
        documents::view(&core, &ctx, other).await,
        Err(CoreError::NotFound)
    ));
    assert_eq!(
        visible_ids(&core, &ctx, DocumentFilter::default()).await,
        vec![policy]
    );
    let tombstone: serde_json::Value = sqlx::query_scalar(
        "SELECT context FROM audit_log WHERE action = 'document.deleted' AND document_id = $1",
    )
    .bind(other)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(tombstone["tombstone"]["title"], "Scratch");
    assert_eq!(tombstone["reason"], "created in error");
    // An administrator can restore it.
    documents::restore_deleted(&core, &system(), other)
        .await
        .unwrap();
    assert!(documents::view(&core, &ctx, other).await.is_ok());
}

#[tokio::test]
async fn versions_upload_restore_and_stay_immutable() {
    let (core, db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let other = type_id(&core, "other").await;
    let v1 = pdf("version one");
    let id = new_doc(
        &core,
        &ctx,
        meta(other, dept, "Original title"),
        alice,
        Some(&v1),
    )
    .await;
    let v2 = pdf("amendment one");
    let n = documents::add_version(
        &core,
        &ctx,
        id,
        upload(&core, "amendment-1.pdf", &v2).await,
        VersionKind::Amendment,
        "Amendment 1",
        date("2014-06-01"),
    )
    .await
    .unwrap();
    assert_eq!(n, 2);
    documents::update(&core, &ctx, id, meta(other, dept, "Changed title"), 1)
        .await
        .unwrap();

    let versions = documents::versions(&core, &ctx, id).await.unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[1].kind, VersionKind::Amendment);
    assert_eq!(versions[1].milestone_date, date("2014-06-01"));
    assert_eq!(
        versions[0].file_name.as_deref(),
        Some("amendment-1.pdf"),
        "metadata version carries the file"
    );
    assert_eq!(
        read_all(
            documents::open_file(&core, &ctx, id, Some(1))
                .await
                .unwrap()
                .stream
        )
        .await,
        v1
    );

    documents::restore_version(&core, &ctx, id, 1, RestorePart::File)
        .await
        .unwrap();
    assert_eq!(
        read_all(
            documents::open_file(&core, &ctx, id, None)
                .await
                .unwrap()
                .stream
        )
        .await,
        v1
    );
    documents::restore_version(&core, &ctx, id, 1, RestorePart::Metadata)
        .await
        .unwrap();
    assert_eq!(
        documents::view(&core, &ctx, id)
            .await
            .unwrap()
            .document
            .title,
        "Original title"
    );
    assert_eq!(documents::versions(&core, &ctx, id).await.unwrap().len(), 5);

    // Versions and blobs cannot be changed, even by the schema owner.
    for (pool, label) in [(&db.app, "app"), (&db.owner, "owner")] {
        for sql in [
            "UPDATE document_versions SET note = 'x'",
            "DELETE FROM document_versions",
            "DELETE FROM blobs",
        ] {
            let err = sqlx::query(sql)
                .execute(pool)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("permission denied") || err.contains("immutable"),
                "{label}: {sql}: {err}"
            );
        }
    }
}

#[tokio::test]
async fn rejected_uploads_are_audited_and_not_stored() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(
        &core,
        &ctx,
        meta(type_id(&core, "other").await, dept, "Doc"),
        alice,
        None,
    )
    .await;
    let exe = [
        b'M', b'Z', 0x90, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0xFF, 0xFF, 0, 0,
    ];
    let r = documents::add_version(
        &core,
        &ctx,
        id,
        upload(&core, "invoice.pdf", &exe).await,
        VersionKind::Revision,
        "",
        None,
    )
    .await;
    assert!(matches!(r, Err(CoreError::Validation(m)) if m.contains("not accepted")));
    let blobs: i64 = sqlx::query_scalar("SELECT count(*) FROM blobs")
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(blobs, 0);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"document.upload_rejected".to_owned())
    );
}

#[tokio::test]
async fn export_requires_export_permission_and_is_audited() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    new_doc(
        &core,
        &ctx,
        meta(type_id(&core, "other").await, dept, "Exportable"),
        alice,
        None,
    )
    .await;
    let csv = String::from_utf8(
        search::export_csv(&core, &ctx, &DocumentFilter::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(csv.lines().count(), 1, "header only: no export permission");
    acl::grant(
        &core.db,
        &system(),
        Subject::Department(dept),
        Scope::Department(dept),
        Permission::Export,
        None,
        "t",
    )
    .await
    .unwrap();
    let csv = String::from_utf8(
        search::export_csv(
            &core,
            &as_user(&core, alice).await,
            &DocumentFilter::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(csv.lines().count(), 2);
    assert!(csv.contains("Exportable"));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'document.list_exported'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(n, 2);
}

/// Acceptance test 1 (the parts available in Phase 1): A uploads an agreement,
/// B changes the renewal date and the owner, C uploads an amended version, an
/// administrator changes the ACL. The timeline shows each event in order with
/// before/after values, version history shows both files, and the audit chain
/// is intact. (Legal approval arrives in Phase 3, records request export in Phase 4.)
#[tokio::test]
async fn acceptance_audit_scenario() {
    let (core, _db) = core().await;
    let (dept, user_a) = editing_department(&core, "OPS", "a@x.org").await;
    let user_b = directory::create_user(&core.db, &system(), "B", "b@x.org", Some(dept))
        .await
        .unwrap();
    let user_c = directory::create_user(&core.db, &system(), "C", "c@x.org", Some(dept))
        .await
        .unwrap();
    let (admin, _) = user_in_new_department(&core, "IT", "admin@x.org").await;
    let admin_role = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), admin, admin_role, None)
        .await
        .unwrap();
    acl::grant(
        &core.db,
        &system(),
        Subject::User(user_b),
        Scope::Department(dept),
        Permission::ManagePermissions,
        None,
        "t",
    )
    .await
    .unwrap();
    let agreement = type_id(&core, "interlocal_agreement").await;

    let mut m = meta(agreement, dept, "Partnership agreement with Partner");
    m.renewal_date = date("2026-12-31");
    let id = new_doc(
        &core,
        &as_user(&core, user_a).await,
        m.clone(),
        user_a,
        Some(&pdf("signed 2013")),
    )
    .await;

    let ctx_b = as_user(&core, user_b).await;
    m.renewal_date = date("2027-12-31");
    documents::update(&core, &ctx_b, id, m, 1).await.unwrap();
    documents::transfer_owner(&core, &ctx_b, id, user_c, "C now manages this agreement")
        .await
        .unwrap();

    let ctx_c = as_user(&core, user_c).await;
    documents::add_version(
        &core,
        &ctx_c,
        id,
        upload(&core, "amended.pdf", &pdf("amendment 2014")).await,
        VersionKind::Amendment,
        "Amendment 1",
        date("2014-06-01"),
    )
    .await
    .unwrap();

    let ctx_admin = as_user(&core, admin).await;
    documents::share(
        &core,
        &ctx_admin,
        id,
        Subject::User(admin),
        Permission::Export,
        None,
        "records request",
    )
    .await
    .unwrap();

    let timeline = documents::timeline(&core, &ctx_admin, id).await.unwrap();
    let actions: Vec<&str> = timeline
        .iter()
        .map(|e| e.event.action.as_str())
        .filter(|a| !matches!(*a, "document.viewed" | "audit.searched"))
        .collect();
    assert_eq!(
        actions,
        vec![
            "document.created",
            "document.file_uploaded",
            "document.metadata_updated",
            "document.owner_transferred",
            "document.file_uploaded",
            "acl.granted",
        ]
    );
    // Strictly increasing sequence and timestamps; every entry names its actor.
    assert!(
        timeline
            .windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].event.occurred_at <= w[1].event.occurred_at)
    );
    let by = |action: &str, n: usize| {
        timeline
            .iter()
            .filter(|e| e.event.action == action)
            .nth(n)
            .unwrap()
    };
    assert_eq!(by("document.created", 0).event.actor.user_id, Some(user_a));
    let edit = by("document.metadata_updated", 0);
    assert_eq!(edit.event.actor.user_id, Some(user_b));
    assert_eq!(
        edit.event.changes.as_ref().unwrap()[0].field,
        "renewal_date"
    );
    assert_eq!(
        by("document.owner_transferred", 0).event.actor.user_id,
        Some(user_b)
    );
    assert_eq!(
        by("document.file_uploaded", 1).event.actor.user_id,
        Some(user_c)
    );
    assert_eq!(by("acl.granted", 0).event.actor.user_id, Some(admin));

    let files: Vec<_> = documents::versions(&core, &ctx_admin, id)
        .await
        .unwrap()
        .into_iter()
        .filter(|v| v.file_changed)
        .collect();
    assert_eq!(files.len(), 2, "both file versions in history");
    assert!(pdms_audit::verify(&core.db).await.unwrap().is_intact());

    // A plain department member cannot read the timeline.
    let viewer = directory::create_user(&core.db, &system(), "V", "v@x.org", Some(dept))
        .await
        .unwrap();
    assert!(matches!(
        documents::timeline(&core, &as_user(&core, viewer).await, id).await,
        Err(CoreError::Forbidden(_))
    ));
}

async fn new_doc_with(
    core: &Core,
    ctx: &Ctx,
    input: MetadataInput,
    owner: Uuid,
    visibility: Visibility,
) -> Uuid {
    documents::create(
        core,
        ctx,
        NewDocument {
            metadata: input,
            owner_id: owner,
            status: DocumentStatus::Active,
            visibility,
            tags: vec![],
            file: Some(upload(core, "policy.pdf", &pdf("leave policy")).await),
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn county_wide_documents_are_viewable_by_every_department() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let (outsider, _) = user_in_new_department(&core, "OPS", "o@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx_alice = as_user(&core, alice).await;
    let shared = new_doc_with(
        &core,
        &ctx_alice,
        meta(other, dept, "Leave policy"),
        alice,
        Visibility::CountyWide,
    )
    .await;
    let internal = new_doc_with(
        &core,
        &ctx_alice,
        meta(other, dept, "HR roster"),
        alice,
        Visibility::Department,
    )
    .await;

    let ctx_out = as_user(&core, outsider).await;
    assert_eq!(
        visible_ids(&core, &ctx_out, DocumentFilter::default()).await,
        vec![shared]
    );
    assert!(documents::view(&core, &ctx_out, shared).await.is_ok());
    assert!(
        documents::open_file(&core, &ctx_out, shared, None)
            .await
            .is_ok(),
        "download allowed"
    );
    assert!(matches!(
        documents::view(&core, &ctx_out, internal).await,
        Err(CoreError::Forbidden(_))
    ));
    // View only: editing details is still refused.
    let doc = documents::view(&core, &ctx_out, shared)
        .await
        .unwrap()
        .document;
    let mut m = meta(other, dept, "Renamed by outsider");
    m.description = doc.description.clone();
    assert!(matches!(
        documents::update(&core, &ctx_out, shared, m, doc.row_version).await,
        Err(CoreError::Forbidden(_))
    ));
    let created = audit_events(&core, "document.created").await;
    assert!(
        created.iter().any(|c| c.contains("county_wide")),
        "visibility recorded at creation"
    );
}

#[tokio::test]
async fn owner_can_change_visibility_and_it_is_audited() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let (outsider, _) = user_in_new_department(&core, "OPS", "o@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx_alice = as_user(&core, alice).await;
    let id = new_doc_with(
        &core,
        &ctx_alice,
        meta(other, dept, "Leave policy"),
        alice,
        Visibility::Department,
    )
    .await;

    documents::set_visibility(&core, &ctx_alice, id, Visibility::CountyWide)
        .await
        .unwrap();
    assert!(
        documents::view(&core, &as_user(&core, outsider).await, id)
            .await
            .is_ok()
    );
    documents::set_visibility(&core, &ctx_alice, id, Visibility::Restricted)
        .await
        .unwrap();
    assert!(matches!(
        documents::view(&core, &as_user(&core, outsider).await, id).await,
        Err(CoreError::Forbidden(_))
    ));
    let changes = audit_events(&core, "document.visibility_changed").await;
    assert_eq!(changes.len(), 2);
    assert!(changes[0].contains("department") && changes[0].contains("county_wide"));
    assert!(changes[1].contains("county_wide") && changes[1].contains("restricted"));
}

#[tokio::test]
async fn editor_who_is_not_owner_cannot_change_visibility() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let bob = directory::create_user(&core.db, &system(), "bob", "bob@x.org", Some(dept))
        .await
        .unwrap();
    let other = type_id(&core, "other").await;
    let id = new_doc_with(
        &core,
        &as_user(&core, alice).await,
        meta(other, dept, "Roster"),
        alice,
        Visibility::Department,
    )
    .await;
    let ctx_bob = as_user(&core, bob).await; // department editor, not the owner
    assert!(matches!(
        documents::set_visibility(&core, &ctx_bob, id, Visibility::CountyWide).await,
        Err(CoreError::Forbidden(_))
    ));
    let doc = documents::view(&core, &ctx_bob, id).await.unwrap().document;
    assert_eq!(doc.visibility, Visibility::Department);
    assert!(
        audit_actions(&core)
            .await
            .contains(&"access.denied".to_owned())
    );
}

#[tokio::test]
async fn setting_same_visibility_is_a_no_op() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc_with(
        &core,
        &ctx,
        meta(other, dept, "Roster"),
        alice,
        Visibility::Department,
    )
    .await;
    let before = documents::view(&core, &ctx, id)
        .await
        .unwrap()
        .document
        .updated_at;
    documents::set_visibility(&core, &ctx, id, Visibility::Department)
        .await
        .unwrap();
    assert_eq!(
        documents::view(&core, &ctx, id)
            .await
            .unwrap()
            .document
            .updated_at,
        before
    );
    assert!(
        audit_events(&core, "document.visibility_changed")
            .await
            .is_empty()
    );
}

/// `changes` of every audit entry with this action, as JSON text, oldest first.
async fn audit_events(core: &Core, action: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT coalesce(changes::text, '') FROM audit_log WHERE action = $1 ORDER BY seq",
    )
    .bind(action)
    .fetch_all(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn demo_seed_shows_all_three_visibilities() {
    let (core, _db) = core().await;
    pdms_core::demo::seed(&core).await.unwrap();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT title, visibility::text FROM documents
          WHERE title IN ('Personnel Policy Manual', 'Workplace Investigation Procedures',
                          'Asphalt Supply Contract FY2026')
          ORDER BY title",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "Asphalt Supply Contract FY2026".to_owned(),
                "department".to_owned()
            ),
            (
                "Personnel Policy Manual".to_owned(),
                "county_wide".to_owned()
            ),
            (
                "Workplace Investigation Procedures".to_owned(),
                "restricted".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn status_counts_respect_visibility() {
    let (core, _db) = core().await;
    let (dept_a, alice) = editing_department(&core, "AAA", "alice@x.org").await;
    let (dept_b, bob) = editing_department(&core, "BBB", "bob@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx_a = as_user(&core, alice).await;
    let ctx_b = as_user(&core, bob).await;
    for (title, v) in [
        ("B internal", Visibility::Department),
        ("B restricted", Visibility::Restricted),
        ("B county", Visibility::CountyWide),
    ] {
        new_doc_with(&core, &ctx_b, meta(other, dept_b, title), bob, v).await;
    }
    documents::create(
        &core,
        &ctx_a,
        NewDocument {
            visibility: Default::default(),
            metadata: meta(other, dept_a, "A draft"),
            owner_id: alice,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();

    let mut counts = search::status_counts(&core, &ctx_a, &DocumentFilter::default())
        .await
        .unwrap();
    counts.sort_by_key(|(s, _)| s.as_str());
    assert_eq!(
        counts,
        vec![(DocumentStatus::Active, 1), (DocumentStatus::Draft, 1)]
    );
    // Status and paging are ignored; other filters still apply.
    let f = DocumentFilter {
        status: Some(DocumentStatus::Draft),
        q: Some("county".into()),
        ..Default::default()
    };
    let counts = search::status_counts(&core, &ctx_a, &f).await.unwrap();
    assert_eq!(counts, vec![(DocumentStatus::Active, 1)]);
}
