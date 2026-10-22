// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! The dashboard's activity feed (R2 spec §8): only listed document events,
//! only on documents the viewer may see now, names only where the document
//! page shows them.

mod common;

use common::*;
use pdms_authz::{Permission, Scope, Subject, Visibility};
use pdms_core::activity::{self, ActivityKind};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::{Core, Ctx, acl, comments};
use uuid::Uuid;

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

async fn doc(
    core: &Core,
    ctx: &Ctx,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    vis: Visibility,
) -> Uuid {
    let policy = types::type_by_key(&core.db, "policy").await.unwrap().id;
    documents::create(
        core,
        ctx,
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: policy,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            visibility: vis,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

/// (title, kind, actor) of what `ctx` sees.
async fn feed(core: &Core, ctx: &Ctx) -> Vec<(String, ActivityKind, Option<String>)> {
    activity::recent(core, ctx, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.title, a.kind, a.actor))
        .collect()
}

#[tokio::test]
async fn feed_shows_only_listed_events_on_documents_you_can_see_now() {
    let (core, _db) = core().await;
    let (ops, alice) = editing_department(&core, "OPS", "alice@x.org").await;
    let (hr, bob) = editing_department(&core, "HR", "bob@x.org").await;
    let a = as_user(&core, alice).await;
    let b = as_user(&core, bob).await;

    let own = doc(&core, &a, ops, alice, "Ops manual", Visibility::Department).await;
    doc(
        &core,
        &b,
        hr,
        bob,
        "HR department doc",
        Visibility::Department,
    )
    .await;
    doc(
        &core,
        &b,
        hr,
        bob,
        "HR restricted doc",
        Visibility::Restricted,
    )
    .await;
    doc(
        &core,
        &b,
        hr,
        bob,
        "HR county-wide doc",
        Visibility::CountyWide,
    )
    .await;
    let narrowed = doc(&core, &b, hr, bob, "Later narrowed", Visibility::CountyWide).await;
    comments::post(&core, &b, narrowed, None, "Owner's note")
        .await
        .unwrap();
    // Narrowed after the events: the feed follows today's visibility.
    documents::set_visibility(&core, &b, narrowed, Visibility::Department)
        .await
        .unwrap();

    // Events on Alice's own document: a comment (named), a details change
    // (not named), and a view and a download attempt (never listed).
    comments::post(&core, &a, own, None, "Looks right")
        .await
        .unwrap();
    let mut m = MetadataInput::default();
    m.title = "Ops manual".into();
    m.document_type_id = types::type_by_key(&core.db, "policy").await.unwrap().id;
    m.department_id = ops;
    m.description = "Updated".into();
    documents::update(&core, &a, own, m, 1).await.unwrap();
    documents::view(&core, &a, own).await.unwrap();

    let seen = feed(&core, &a).await;
    let titles: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
    assert!(titles.contains(&"Ops manual"));
    assert!(titles.contains(&"HR county-wide doc"));
    for hidden in ["HR department doc", "HR restricted doc", "Later narrowed"] {
        assert!(!titles.contains(&hidden), "{hidden}: {seen:?}");
    }
    // Names only where the document page shows them.
    let comment = seen
        .iter()
        .find(|s| s.0 == "Ops manual" && s.1 == ActivityKind::CommentPosted)
        .unwrap();
    assert!(comment.2.is_some(), "comments name their author");
    let details = seen
        .iter()
        .find(|s| s.1 == ActivityKind::DetailsChanged)
        .unwrap();
    assert_eq!(details.2, None, "details changes are not named");
    // A view is audited but never shown: Alice's own document has exactly
    // the created, comment and details events.
    let own_events = seen.iter().filter(|s| s.0 == "Ops manual").count();
    assert_eq!(own_events, 3, "{seen:?}");

    // Bob sees his own department's documents, including the narrowed one.
    let bobs: Vec<String> = feed(&core, &b).await.into_iter().map(|s| s.0).collect();
    assert!(bobs.contains(&"Later narrowed".to_owned()));
    assert!(!bobs.contains(&"Ops manual".to_owned()));
}
