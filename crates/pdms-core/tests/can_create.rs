// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! `can_create_any` (bounded queries, used on every page) must give the same
//! answer as `creatable_departments` (one query per department).

mod common;

use common::*;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::documents::access;
use pdms_core::model::UserStatus;
use pdms_core::{Core, Ctx, acl, directory};
use uuid::Uuid;

/// Both answers, asserted equal; returns the shared answer.
async fn agree(core: &Core, ctx: &Ctx, case: &str) -> bool {
    let listed = !access::creatable_departments(core, ctx)
        .await
        .unwrap()
        .is_empty();
    let any = access::can_create_any(core, ctx).await.unwrap();
    assert_eq!(any, listed, "{case}: can_create_any disagrees");
    any
}

async fn grant_create(core: &Core, subject: Subject, scope: Scope) {
    acl::grant(
        &core.db,
        &system(),
        subject,
        scope,
        Permission::Create,
        None,
        "test",
    )
    .await
    .unwrap();
}

async fn new_user(core: &Core, email: &str, dept: Option<Uuid>) -> Uuid {
    directory::create_user(&core.db, &system(), email, email, dept)
        .await
        .unwrap()
}

#[tokio::test]
async fn can_create_any_agrees_with_creatable_departments() {
    let (core, _db) = core().await;

    // No departments at all, even with an org-wide grant: nowhere to create.
    let early = new_user(&core, "early@example.org", None).await;
    grant_create(&core, Subject::User(early), Scope::Global).await;
    assert!(!agree(&core, &as_user(&core, early).await, "no departments").await);

    let (plain, a) = user_in_new_department(&core, "A", "plain@example.org").await;
    let (b_member, b) = user_in_new_department(&core, "B", "b@example.org").await;
    let c = directory::create_department(&core.db, &system(), "C", "C")
        .await
        .unwrap();

    // No grants for them.
    assert!(!agree(&core, &as_user(&core, plain).await, "no grants").await);
    assert!(!agree(&core, &as_user(&core, b_member).await, "no grants (B)").await);

    // A grant in one of several departments: B's members may create in B.
    grant_create(&core, Subject::Department(b), Scope::Department(b)).await;
    assert!(agree(&core, &as_user(&core, b_member).await, "department grant").await);
    assert!(
        !agree(
            &core,
            &as_user(&core, plain).await,
            "another department's grant"
        )
        .await
    );
    // A personal grant scoped to a department they are not in.
    let helper = new_user(&core, "helper@example.org", Some(a)).await;
    grant_create(&core, Subject::User(helper), Scope::Department(c)).await;
    assert!(
        agree(
            &core,
            &as_user(&core, helper).await,
            "user grant in other dept"
        )
        .await
    );

    // An org-wide grant, for a user with no department.
    let clerk = new_user(&core, "clerk@example.org", None).await;
    assert!(!agree(&core, &as_user(&core, clerk).await, "before global grant").await);
    grant_create(&core, Subject::User(clerk), Scope::Global).await;
    assert!(agree(&core, &as_user(&core, clerk).await, "global grant").await);

    // A grant only on a department that is then deactivated.
    let old = directory::create_department(&core.db, &system(), "OLD", "Old")
        .await
        .unwrap();
    let archivist = new_user(&core, "archivist@example.org", Some(old)).await;
    grant_create(&core, Subject::User(archivist), Scope::Department(old)).await;
    assert!(agree(&core, &as_user(&core, archivist).await, "active dept grant").await);
    directory::update_department(&core.db, &system(), old, "Old", false)
        .await
        .unwrap();
    assert!(
        !agree(
            &core,
            &as_user(&core, archivist).await,
            "inactive dept grant"
        )
        .await
    );

    // An inactive user keeps their grants but may not create.
    directory::set_user_status(&core.db, &system(), clerk, UserStatus::Disabled)
        .await
        .unwrap();
    assert!(!agree(&core, &as_user(&core, clerk).await, "inactive user").await);

    // The system context may create wherever a department is active.
    assert!(agree(&core, &system(), "system").await);
}
