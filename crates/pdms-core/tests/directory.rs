#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod common;

use common::*;
use pdms_audit::{Actor, AuthMethod};
use pdms_authz::{DocumentAttrs, Permission, Policy, Scope, Subject, Visibility, decide};
use pdms_core::auth::principal;
use pdms_core::directory::{self, sync};
use pdms_core::model::UserStatus;
use pdms_core::{ClientInfo, CoreError, Ctx, acl};
use uuid::Uuid;

async fn as_user(core: &pdms_core::Core, user: Uuid) -> Ctx {
    let p = principal::load(&core.db, user).await.unwrap();
    Ctx::user(p, "someone", AuthMethod::Local, &ClientInfo::default())
}

#[tokio::test]
async fn non_admins_cannot_change_the_directory() {
    let (core, _db) = core().await;
    let (user, dept) = user_in_new_department(&core, "HR", "h@example.org").await;
    let ctx = as_user(&core, user).await;
    let r = directory::create_department(&core.db, &ctx, "X", "X").await;
    assert!(matches!(r, Err(CoreError::Forbidden(_))));
    let r = acl::grant(
        &core.db,
        &ctx,
        Subject::User(user),
        Scope::Department(dept),
        Permission::Edit,
        None,
        "",
    )
    .await;
    assert!(matches!(r, Err(CoreError::Forbidden(_))));
    let r = directory::add_department_member(&core.db, &ctx, user, dept, false).await;
    assert!(matches!(r, Err(CoreError::Forbidden(_))));
}

#[tokio::test]
async fn new_department_gets_read_only_default_grant() {
    let (core, _db) = core().await;
    let (user, dept) = user_in_new_department(&core, "OPS", "r@example.org").await;
    let grants = acl::relevant_grants(&core.db, dept, None).await.unwrap();
    let p = principal::load(&core.db, user).await.unwrap();
    let doc = DocumentAttrs {
        id: Uuid::now_v7(),
        department_id: dept,
        owner_id: Uuid::nil(),
        visibility: Visibility::Department,
    };
    let now = chrono::Utc::now();
    assert!(decide(&Policy::default(), &p, Permission::View, &doc, &grants, now).is_allowed());
    for perm in [
        Permission::Edit,
        Permission::Delete,
        Permission::Approve,
        Permission::ManagePermissions,
        Permission::Export,
    ] {
        assert!(
            !decide(&Policy::default(), &p, perm, &doc, &grants, now).is_allowed(),
            "{perm}"
        );
    }
}

#[tokio::test]
async fn grant_and_revoke_take_effect_on_next_check_and_are_audited() {
    let (core, _db) = core().await;
    let (user, dept) = user_in_new_department(&core, "OPS", "r@example.org").await;
    let doc = DocumentAttrs {
        id: Uuid::now_v7(),
        department_id: dept,
        owner_id: Uuid::nil(),
        visibility: Visibility::Department,
    };
    let check = || async {
        let p = principal::load(&core.db, user).await.unwrap();
        let g = acl::relevant_grants(&core.db, dept, Some(doc.id))
            .await
            .unwrap();
        decide(
            &Policy::default(),
            &p,
            Permission::Edit,
            &doc,
            &g,
            chrono::Utc::now(),
        )
        .is_allowed()
    };
    assert!(!check().await);
    let id = acl::grant(
        &core.db,
        &system(),
        Subject::User(user),
        Scope::Department(dept),
        Permission::Edit,
        None,
        "test",
    )
    .await
    .unwrap();
    assert!(check().await);
    acl::revoke(&core.db, &system(), id).await.unwrap();
    assert!(!check().await, "revocation must apply immediately");

    let actions = audit_actions(&core).await;
    assert!(actions.contains(&"acl.granted".into()) && actions.contains(&"acl.revoked".into()));
    let report = pdms_audit::verify(&core.db).await.unwrap();
    assert!(report.is_intact(), "{:?}", report.breaks);
}

#[tokio::test]
async fn last_administrator_cannot_be_removed() {
    let (core, _db) = core().await;
    let (admin, _) = user_in_new_department(&core, "IT", "a@example.org").await;
    let role = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    let assignment = directory::assign_role(&core.db, &system(), admin, role, None)
        .await
        .unwrap();
    assert!(matches!(
        directory::revoke_role(&core.db, &system(), assignment).await,
        Err(CoreError::Conflict(_))
    ));
    assert!(matches!(
        directory::set_user_status(&core.db, &system(), admin, UserStatus::Departed).await,
        Err(CoreError::Conflict(_))
    ));
    let ctx = as_user(&core, admin).await;
    assert!(matches!(
        directory::set_user_status(&core.db, &ctx, admin, UserStatus::Disabled).await,
        Err(CoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn metadata_edits_record_field_diffs() {
    let (core, _db) = core().await;
    let (user, _) = user_in_new_department(&core, "IT", "old@example.org").await;
    directory::update_user(
        &core.db,
        &system(),
        user,
        "old@example.org",
        "new@example.org",
    )
    .await
    .unwrap();
    let changes: serde_json::Value =
        sqlx::query_scalar("SELECT changes FROM audit_log WHERE action = 'user.updated'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert_eq!(
        changes,
        serde_json::json!([{"field": "email", "before": "old@example.org", "after": "new@example.org"}])
    );
}

#[tokio::test]
async fn idp_sync_reconciles_only_idp_memberships() {
    let (core, _db) = core().await;
    let (user, manual_dept) = user_in_new_department(&core, "HR", "s@example.org").await;
    let synced_dept = directory::create_department(&core.db, &system(), "OPS", "Operations")
        .await
        .unwrap();
    let head = directory::role_id_by_key(&core.db, "department_head")
        .await
        .unwrap();
    directory::create_idp_mapping(
        &core.db,
        &system(),
        "grp-ops",
        "",
        directory::MappingTarget::Department(synced_dept),
    )
    .await
    .unwrap();
    directory::create_idp_mapping(
        &core.db,
        &system(),
        "grp-heads",
        "",
        directory::MappingTarget::Role {
            role_id: head,
            department_id: Some(synced_dept),
        },
    )
    .await
    .unwrap();

    let mut tx = core.db.begin().await.unwrap();
    let change = sync::reconcile_idp_memberships(
        &mut tx,
        user,
        &["grp-ops".into(), "grp-heads".into(), "unmapped".into()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(change.is_some());
    let p = principal::load(&core.db, user).await.unwrap();
    assert!(p.departments.contains(&synced_dept) && p.departments.contains(&manual_dept));
    assert!(p.holds_role_for(head, synced_dept));

    // Same claims again: no change.
    let mut tx = core.db.begin().await.unwrap();
    assert!(
        sync::reconcile_idp_memberships(&mut tx, user, &["grp-ops".into(), "grp-heads".into()])
            .await
            .unwrap()
            .is_none()
    );
    // Removed from IdP groups: IdP memberships go, the manual one stays.
    assert!(
        sync::reconcile_idp_memberships(&mut tx, user, &[])
            .await
            .unwrap()
            .is_some()
    );
    tx.commit().await.unwrap();
    let p = principal::load(&core.db, user).await.unwrap();
    assert_eq!(
        p.departments.into_iter().collect::<Vec<_>>(),
        vec![manual_dept]
    );
    assert!(p.roles.is_empty());
}

#[tokio::test]
async fn audit_search_requires_capability_and_is_itself_audited() {
    let (core, _db) = core().await;
    let (user, _) = user_in_new_department(&core, "IT", "e@example.org").await;
    let ctx = as_user(&core, user).await;
    let filter = pdms_core::audit_log::AuditFilter::default();
    assert!(matches!(
        pdms_core::audit_log::search(&core.db, &ctx, &filter, 50).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        pdms_core::audit_log::export(
            &core.db,
            &ctx,
            &filter,
            pdms_core::audit_log::ExportFormat::Csv
        )
        .await,
        Err(CoreError::Forbidden(_))
    ));

    let auditor = directory::role_id_by_key(&core.db, "auditor")
        .await
        .unwrap();
    directory::assign_role(&core.db, &system(), user, auditor, None)
        .await
        .unwrap();
    let ctx = as_user(&core, user).await;
    let rows = pdms_core::audit_log::search(&core.db, &ctx, &filter, 50)
        .await
        .unwrap();
    assert!(!rows.is_empty());
    let csv = pdms_core::audit_log::export(
        &core.db,
        &ctx,
        &filter,
        pdms_core::audit_log::ExportFormat::Csv,
    )
    .await
    .unwrap();
    assert!(
        String::from_utf8(csv)
            .unwrap()
            .starts_with("seq,occurred_at")
    );
    let actions = audit_actions(&core).await;
    assert!(
        actions.contains(&"audit.searched".into()) && actions.contains(&"audit.exported".into())
    );
    let _ = Actor::system("x");
}
