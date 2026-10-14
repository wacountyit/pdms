// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use chrono::{TimeZone, Utc};
use proptest::prelude::*;
use uuid::Uuid;

use crate::*;

fn fixed_event() -> AuditEvent {
    let mut actor = Actor::system("test");
    actor.user_id = Some(Uuid::from_u128(1));
    actor.display = Some("Ada \"Admin\" Lovelace".into());
    actor.auth_method = AuthMethod::Local;
    actor.ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)));
    actor.user_agent = Some("ua/1.0".into());
    let mut e = AuditEvent::new(
        Category::Metadata,
        "document.metadata_updated",
        actor,
        "document",
    )
    .object(
        Uuid::from_u128(2),
        Some("Agreement – Facilities Maintenance"),
    )
    .document(Uuid::from_u128(2))
    .department(Some(Uuid::from_u128(3)))
    .change("renewal_date", "2026-01-01", "2027-01-01")
    .change("unchanged", "x", "x")
    .context("request_ref", "REQ-1\n");
    e.id = Uuid::from_u128(4);
    e.occurred_at = Utc
        .with_ymd_and_hms(2026, 10, 13, 15, 4, 5)
        .single()
        .unwrap()
        + chrono::Duration::nanoseconds(123_456_789);
    e
}

#[test]
fn canonical_encoding_golden_vector() {
    let s = seal(fixed_event(), ChainHead::GENESIS);
    let canonical = String::from_utf8(canonical_bytes(
        &s.event,
        s.seq,
        s.hash_version,
        &s.prev_hash,
    ))
    .unwrap();
    let expected = concat!(
        r#"{"action":"document.metadata_updated","actor_auth_method":"local","#,
        r#""actor_display":"Ada \"Admin\" Lovelace","actor_ip":"10.0.0.7","actor_user_agent":"ua/1.0","#,
        r#""actor_user_id":"00000000-0000-0000-0000-000000000001","api_token_id":null,"category":"metadata","#,
        r#""changes":[{"after":"2027-01-01","before":"2026-01-01","field":"renewal_date"}],"#,
        r#""context":{"request_ref":"REQ-1\n"},"department_id":"00000000-0000-0000-0000-000000000003","#,
        r#""document_id":"00000000-0000-0000-0000-000000000002","hash_version":1,"#,
        r#""id":"00000000-0000-0000-0000-000000000004","object_id":"00000000-0000-0000-0000-000000000002","#,
        r#""object_label":"Agreement – Facilities Maintenance","object_type":"document","#,
        r#""occurred_at":"2026-10-13T15:04:05.123456Z","outcome":"success","#,
        r#""prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","#,
        r#""request_id":null,"seq":1}"#
    );
    assert_eq!(canonical, expected);
    // Pin the hash too, so any change to hashing is a deliberate version bump.
    assert_eq!(
        hex::encode(s.entry_hash),
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(expected.as_bytes()))
    );
}

#[test]
fn unchanged_fields_are_not_recorded() {
    let e = fixed_event();
    let changes = e.changes.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].field, "renewal_date");
}

#[test]
fn string_escaping_follows_jcs() {
    let mut e = fixed_event();
    e.context.clear();
    e = e.context("k", "\u{1}\u{8}\u{c}\t\\/é😀");
    let s = seal(e, ChainHead::GENESIS);
    let c = String::from_utf8(canonical_bytes(&s.event, 1, 1, &s.prev_hash)).unwrap();
    assert!(c.contains(r#""k":"\u0001\b\f\t\\/é😀""#), "{c}");
}

#[test]
fn map_keys_sort_by_utf16_code_units() {
    // U+FB01 sorts after U+1F600 by UTF-8 bytes but before it by UTF-16 units.
    let mut e = fixed_event();
    e.context.clear();
    e = e.context("\u{1F600}", 1).context("\u{FB01}", 2);
    let s = seal(e, ChainHead::GENESIS);
    let c = String::from_utf8(canonical_bytes(&s.event, 1, 1, &s.prev_hash)).unwrap();
    assert!(c.contains("{\"\u{1F600}\":1,\"\u{FB01}\":2}"), "{c}");
}

#[test]
fn seal_normalizes_for_database_round_trip() {
    let mut e = fixed_event();
    e.actor.ip = Some(IpAddr::V6(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped()));
    e.object_label = Some("nul\0byte".into());
    let s = seal(e, ChainHead::GENESIS);
    assert_eq!(
        s.event.actor.ip,
        Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)))
    );
    assert_eq!(s.event.object_label.as_deref(), Some("nul\u{FFFD}byte"));
    assert_eq!(s.event.occurred_at.timestamp_subsec_nanos() % 1000, 0);
}

fn chain(n: usize) -> Vec<SealedEvent> {
    let mut head = ChainHead::GENESIS;
    (0..n)
        .map(|i| {
            let e = fixed_event().context("i", i as i64);
            let s = seal(e, head);
            head = ChainHead {
                seq: s.seq,
                entry_hash: s.entry_hash,
            };
            s
        })
        .collect()
}

fn verify(rows: &[SealedEvent], head: Option<ChainHead>) -> VerifyReport {
    let mut v = Verifier::new();
    rows.iter().for_each(|r| v.push(r));
    v.finish(head)
}

fn head_of(rows: &[SealedEvent]) -> ChainHead {
    rows.last().map_or(ChainHead::GENESIS, |r| ChainHead {
        seq: r.seq,
        entry_hash: r.entry_hash,
    })
}

#[test]
fn intact_chain_verifies() {
    let rows = chain(5);
    let r = verify(&rows, Some(head_of(&rows)));
    assert!(r.is_intact(), "{:?}", r.breaks);
    assert_eq!(r.entries_checked, 5);
}

#[test]
fn modified_row_is_detected_once() {
    let mut rows = chain(5);
    rows[2].event.action = "document.viewed".into();
    let r = verify(&rows, Some(head_of(&rows)));
    assert_eq!(r.breaks.len(), 1, "{:?}", r.breaks);
    assert_eq!(r.breaks[0].seq, 3);
    assert!(matches!(
        r.breaks[0].kind,
        BreakKind::EntryHashMismatch { .. }
    ));
}

#[test]
fn deleted_row_is_detected() {
    let mut rows = chain(5);
    rows.remove(2);
    let r = verify(&rows, Some(head_of(&rows)));
    assert!(r.breaks.iter().any(|b| matches!(
        b.kind,
        BreakKind::SeqGap {
            expected: 3,
            found: 4
        }
    )));
    assert!(
        r.breaks
            .iter()
            .any(|b| matches!(b.kind, BreakKind::PrevHashMismatch { .. }))
    );
}

#[test]
fn truncated_tail_is_detected_by_head() {
    let rows = chain(5);
    let head = head_of(&rows);
    let r = verify(&rows[..3], Some(head));
    assert!(matches!(
        r.breaks.as_slice(),
        [Break {
            kind: BreakKind::HeadMismatch { .. },
            ..
        }]
    ));
}

#[test]
fn rehashed_row_breaks_the_next_link() {
    // An attacker who edits a row and recomputes its hash still breaks the chain.
    let mut rows = chain(4);
    rows[1].event.action = "document.viewed".into();
    rows[1].entry_hash = compute_hash(
        &rows[1].event,
        rows[1].seq,
        rows[1].hash_version,
        &rows[1].prev_hash,
    );
    let r = verify(&rows, Some(head_of(&rows)));
    assert!(matches!(
        r.breaks.as_slice(),
        [Break {
            seq: 3,
            kind: BreakKind::PrevHashMismatch { .. }
        }]
    ));
}

#[test]
fn floats_are_rejected_on_decode() {
    let v: serde_json::Value = serde_json::json!({"x": 1.5});
    assert!(AuditValue::from_json(&v).is_err());
}

#[test]
fn enum_strings_round_trip() {
    for c in Category::ALL {
        assert_eq!(c.as_str().parse::<Category>().unwrap(), *c);
    }
    for m in AuthMethod::ALL {
        assert_eq!(m.as_str().parse::<AuthMethod>().unwrap(), *m);
    }
}

fn audit_value() -> impl Strategy<Value = AuditValue> {
    let leaf = prop_oneof![
        Just(AuditValue::Null),
        any::<bool>().prop_map(AuditValue::Bool),
        any::<i64>().prop_map(AuditValue::Int),
        any::<String>().prop_map(AuditValue::Str),
    ];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(AuditValue::List),
            proptest::collection::btree_map(any::<String>(), inner, 0..4).prop_map(AuditValue::Map),
        ]
    })
}

proptest! {
    #[test]
    fn values_survive_json_round_trip_with_identical_hash(v in audit_value(), label in any::<String>()) {
        let mut e = fixed_event().context("v", v);
        e.object_label = Some(label);
        e.actor.ip = Some(IpAddr::V6(Ipv6Addr::LOCALHOST));
        let s = seal(e, ChainHead::GENESIS);
        // Simulate the jsonb round trip the verifier sees.
        let ctx_text = serde_json::to_string(&s.event.context_json()).unwrap();
        let ctx_back: serde_json::Value = serde_json::from_str(&ctx_text).unwrap();
        let mut back = s.clone();
        back.event.context = match AuditValue::from_json(&ctx_back).unwrap() {
            AuditValue::Map(m) => m,
            _ => unreachable!(),
        };
        prop_assert_eq!(&back.event, &s.event);
        prop_assert_eq!(compute_hash(&back.event, 1, 1, &GENESIS_HASH), s.entry_hash);
    }
}
