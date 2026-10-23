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
use pdms_core::CoreError;
use pdms_core::branding;

/// Minimal PNG header bytes: signature + IHDR chunk with the given size.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    v
}

#[tokio::test]
async fn only_settings_managers_can_change_branding() {
    let (core, _db) = core().await;
    let (user, _) = user_in_new_department(&core, "ZZ", "plain@example.org").await;
    let ctx = as_user(&core, user).await;
    assert!(matches!(
        branding::update(&core, &ctx, "X", "#1F3A5F").await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        branding::set_seal(&core, &ctx, &png(64, 64)).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        branding::remove_seal(&core, &ctx).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        branding::reset(&core, &ctx).await,
        Err(CoreError::Forbidden(_))
    ));
}

#[tokio::test]
async fn update_validates_normalizes_and_audits() {
    let (core, _db) = core().await;
    let sys = system();
    branding::update(&core, &sys, "[County Name]", "#1f3a5f")
        .await
        .unwrap();
    let b = branding::load(&core.db).await.unwrap();
    assert_eq!(b.org_name.as_deref(), Some("[County Name]"));
    assert_eq!(b.accent.as_deref(), Some("#1F3A5F"));
    let n = audit_actions(&core).await;
    assert_eq!(n.last().map(String::as_str), Some("branding.updated"));

    branding::update(&core, &sys, " [County Name] ", "#1F3A5F")
        .await
        .unwrap();
    assert_eq!(audit_actions(&core).await.len(), n.len());
    assert_eq!(branding::org_name(&core).await, "[County Name]");

    branding::update(&core, &sys, "", "").await.unwrap();
    assert_eq!(branding::org_name(&core).await, core.config().org.name);
    assert_eq!(branding::load(&core.db).await.unwrap().accent, None);

    let long = "x".repeat(121);
    assert!(matches!(
        branding::update(&core, &sys, &long, "").await,
        Err(CoreError::Validation(_))
    ));
}

#[tokio::test]
async fn county_name_refuses_control_characters() {
    let (core, _db) = core().await;
    let sys = system();
    let before = audit_actions(&core).await.len();
    for bad in [
        "Line\nBreak County",
        "Carriage\rReturn County",
        "Tab\tCounty",
        "Bell\u{7}County",
        "Escape\u{1b}[31mCounty",
        "Delete\u{7f}County",
        "Next\u{85}Line County",
    ] {
        match branding::update(&core, &sys, bad, "").await {
            Err(CoreError::Validation(m)) => {
                assert_eq!(
                    m, "The county name can't contain control characters.",
                    "{bad:?}"
                );
            }
            other => panic!("{bad:?} was accepted: {other:?}"),
        }
    }
    // Nothing was saved or audited.
    assert_eq!(branding::load(&core.db).await.unwrap().org_name, None);
    assert_eq!(audit_actions(&core).await.len(), before);
    // Ordinary punctuation and accented letters are fine, and surrounding
    // whitespace is trimmed rather than refused.
    branding::update(&core, &sys, " Sample-Ñandú County (O'Day) \n", "")
        .await
        .unwrap();
    assert_eq!(
        branding::org_name(&core).await,
        "Sample-Ñandú County (O'Day)"
    );
}

#[tokio::test]
async fn accent_too_light_is_refused_in_service() {
    let (core, _db) = core().await;
    match branding::update(&core, &system(), "", "#F5D76E").await {
        Err(CoreError::Validation(m)) => assert!(m.contains("4.5:1"), "{m}"),
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(branding::load(&core.db).await.unwrap().accent, None);
}

#[tokio::test]
async fn seal_round_trip() {
    let (core, _db) = core().await;
    let sys = system();
    let bytes = png(128, 128);
    let info = branding::set_seal(&core, &sys, &bytes).await.unwrap();
    assert_eq!(info.width, 128);
    let (ct, got) = branding::seal_bytes(&core.db).await.unwrap().unwrap();
    assert_eq!(ct, "image/png");
    assert_eq!(got, bytes);
    let loaded = branding::load(&core.db).await.unwrap().seal.unwrap();
    assert_eq!((loaded.width, loaded.sha256), (128, info.sha256));
    assert!(matches!(
        branding::set_seal(&core, &sys, b"<svg></svg>").await,
        Err(CoreError::Validation(_))
    ));

    branding::remove_seal(&core, &sys).await.unwrap();
    assert!(branding::seal_bytes(&core.db).await.unwrap().is_none());
    assert!(branding::load(&core.db).await.unwrap().seal.is_none());
    let actions = audit_actions(&core).await;
    assert_eq!(
        actions
            .iter()
            .filter(|a| *a == "branding.seal_removed")
            .count(),
        1
    );
    branding::remove_seal(&core, &sys).await.unwrap();
    assert_eq!(audit_actions(&core).await, actions);
}

#[tokio::test]
async fn reset_clears_everything() {
    let (core, _db) = core().await;
    let sys = system();
    branding::update(&core, &sys, "[County Name]", "#1F3A5F")
        .await
        .unwrap();
    branding::set_seal(&core, &sys, &png(64, 64)).await.unwrap();
    branding::reset(&core, &sys).await.unwrap();
    let b = branding::load(&core.db).await.unwrap();
    assert_eq!(b, branding::Branding::default());
    assert_eq!(b.accent_or_default(), branding::DEFAULT_ACCENT);
    assert_eq!(branding::org_name(&core).await, core.config().org.name);
    assert_eq!(
        audit_actions(&core).await.last().map(String::as_str),
        Some("branding.reset")
    );
}
