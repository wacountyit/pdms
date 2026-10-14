// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Canonical encoding, hashing, and chain verification.
//!
//! Hash version 1: `entry_hash = SHA-256(canonical_json(row))`, where the row
//! is every `audit_log` column except `entry_hash` itself (including `seq`,
//! `hash_version`, and `prev_hash`, so each entry commits to its predecessor).
//! Canonical JSON follows RFC 8785 (JCS) for the types we allow: object keys
//! sorted by UTF-16 code units, no insignificant whitespace, minimal string
//! escaping, integers in plain decimal. Timestamps are RFC 3339 UTC with six
//! fractional digits; hashes are lowercase hex; absent values are `null`.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::event::{AuditEvent, AuditValue, format_timestamp};

pub const HASH_VERSION: i16 = 1;
pub const GENESIS_HASH: [u8; 32] = [0; 32];

pub type Hash = [u8; 32];

/// An event linked into the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedEvent {
    pub seq: i64,
    pub hash_version: i16,
    pub prev_hash: Hash,
    pub entry_hash: Hash,
    pub event: AuditEvent,
}

/// The current end of the chain.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChainHead {
    pub seq: i64,
    pub entry_hash: Hash,
}

impl ChainHead {
    pub const GENESIS: ChainHead = ChainHead {
        seq: 0,
        entry_hash: GENESIS_HASH,
    };
}

/// Normalize `event` and link it after `head`.
pub fn seal(mut event: AuditEvent, head: ChainHead) -> SealedEvent {
    event.normalize();
    let seq = head.seq + 1;
    let entry_hash = compute_hash(&event, seq, HASH_VERSION, &head.entry_hash);
    SealedEvent {
        seq,
        hash_version: HASH_VERSION,
        prev_hash: head.entry_hash,
        entry_hash,
        event,
    }
}

pub fn compute_hash(event: &AuditEvent, seq: i64, hash_version: i16, prev_hash: &Hash) -> Hash {
    Sha256::digest(canonical_bytes(event, seq, hash_version, prev_hash)).into()
}

/// Canonical JSON for hash version 1.
pub fn canonical_bytes(
    event: &AuditEvent,
    seq: i64,
    hash_version: i16,
    prev_hash: &Hash,
) -> Vec<u8> {
    let a = &event.actor;
    let opt_str = |s: &Option<String>| s.as_deref().map_or(AuditValue::Null, AuditValue::from);
    let opt_uuid = |u: Option<uuid::Uuid>| AuditValue::from(u);
    let changes = event.changes.as_ref().map_or(AuditValue::Null, |cs| {
        AuditValue::List(
            cs.iter()
                .map(|c| {
                    AuditValue::Map(
                        [
                            ("after".to_owned(), c.after.clone()),
                            ("before".to_owned(), c.before.clone()),
                            ("field".to_owned(), AuditValue::from(c.field.as_str())),
                        ]
                        .into(),
                    )
                })
                .collect(),
        )
    });

    // Keys are ASCII, so byte order equals UTF-16 order; BTreeMap sorts them.
    let row: std::collections::BTreeMap<String, AuditValue> = [
        ("action", AuditValue::from(event.action.as_str())),
        ("actor_auth_method", a.auth_method.as_str().into()),
        ("actor_display", opt_str(&a.display)),
        ("actor_ip", a.ip.map(|ip| ip.to_string()).into()),
        ("actor_user_agent", opt_str(&a.user_agent)),
        ("actor_user_id", opt_uuid(a.user_id)),
        ("api_token_id", opt_uuid(a.api_token_id)),
        ("category", event.category.as_str().into()),
        ("changes", changes),
        ("context", AuditValue::Map(event.context.clone())),
        ("department_id", opt_uuid(event.department_id)),
        ("document_id", opt_uuid(event.document_id)),
        ("hash_version", i64::from(hash_version).into()),
        ("id", event.id.into()),
        ("object_id", opt_str(&event.object_id)),
        ("object_label", opt_str(&event.object_label)),
        ("object_type", event.object_type.as_str().into()),
        ("occurred_at", format_timestamp(event.occurred_at).into()),
        ("outcome", event.outcome.as_str().into()),
        ("prev_hash", hex::encode(prev_hash).into()),
        ("request_id", opt_str(&a.request_id)),
        ("seq", seq.into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();

    let mut out = String::with_capacity(1024);
    write_value(&mut out, &AuditValue::Map(row));
    out.into_bytes()
}

fn write_value(out: &mut String, v: &AuditValue) {
    match v {
        AuditValue::Null => out.push_str("null"),
        AuditValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        AuditValue::Int(i) => {
            let _ = write!(out, "{i}");
        }
        AuditValue::Str(s) => write_string(out, s),
        AuditValue::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        AuditValue::Map(m) => {
            let mut entries: Vec<(&String, &AuditValue)> = m.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_value(out, val);
            }
            out.push('}');
        }
    }
}

/// RFC 8785 string serialization (same as ECMAScript `JSON.stringify`).
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A problem found while verifying the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// A sequence number is missing or repeated (rows deleted or inserted).
    SeqGap {
        expected: i64,
        found: i64,
    },
    /// The row does not point at the previous row's hash.
    PrevHashMismatch {
        expected: Hash,
        found: Hash,
    },
    /// The row's content no longer matches its hash (row modified).
    EntryHashMismatch {
        computed: Hash,
        stored: Hash,
    },
    UnsupportedHashVersion(i16),
    /// The row could not be decoded (for example a float injected into JSON).
    Undecodable(String),
    /// The chain-head record disagrees with the last row (rows truncated or appended outside the protocol).
    HeadMismatch {
        head: ChainHead,
        last: ChainHead,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Break {
    pub seq: i64,
    pub kind: BreakKind,
}

impl std::fmt::Display for Break {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let h = hex::encode;
        match &self.kind {
            BreakKind::SeqGap { expected, found } => {
                write!(
                    f,
                    "seq {}: expected seq {expected}, found {found}",
                    self.seq
                )
            }
            BreakKind::PrevHashMismatch { expected, found } => write!(
                f,
                "seq {}: prev_hash {} does not match previous entry {}",
                self.seq,
                h(found),
                h(expected)
            ),
            BreakKind::EntryHashMismatch { computed, stored } => write!(
                f,
                "seq {}: content modified (stored hash {}, recomputed {})",
                self.seq,
                h(stored),
                h(computed)
            ),
            BreakKind::UnsupportedHashVersion(v) => {
                write!(f, "seq {}: unsupported hash_version {v}", self.seq)
            }
            BreakKind::Undecodable(e) => write!(f, "seq {}: undecodable row: {e}", self.seq),
            BreakKind::HeadMismatch { head, last } => write!(
                f,
                "chain head says seq {} / {}, last row is seq {} / {}",
                head.seq,
                h(&head.entry_hash),
                last.seq,
                h(&last.entry_hash)
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub entries_checked: u64,
    pub last: ChainHead,
    pub breaks: Vec<Break>,
    /// True when more breaks were found than are reported.
    pub truncated: bool,
}

impl VerifyReport {
    pub fn is_intact(&self) -> bool {
        self.breaks.is_empty()
    }
}

/// Streaming verifier: feed rows in `seq` order, then call [`Verifier::finish`].
///
/// After a break it re-synchronizes on the stored values, so one tampered row
/// is reported once rather than invalidating everything after it.
#[derive(Debug)]
pub struct Verifier {
    last: ChainHead,
    checked: u64,
    breaks: Vec<Break>,
    truncated: bool,
}

const MAX_REPORTED_BREAKS: usize = 1000;

impl Default for Verifier {
    fn default() -> Self {
        Verifier {
            last: ChainHead::GENESIS,
            checked: 0,
            breaks: Vec::new(),
            truncated: false,
        }
    }
}

impl Verifier {
    pub fn new() -> Self {
        Self::default()
    }

    fn report(&mut self, seq: i64, kind: BreakKind) {
        if self.breaks.len() < MAX_REPORTED_BREAKS {
            self.breaks.push(Break { seq, kind });
        } else {
            self.truncated = true;
        }
    }

    pub fn push(&mut self, row: &SealedEvent) {
        self.checked += 1;
        let expected_seq = self.last.seq + 1;
        if row.seq != expected_seq {
            self.report(
                row.seq,
                BreakKind::SeqGap {
                    expected: expected_seq,
                    found: row.seq,
                },
            );
        }
        if row.prev_hash != self.last.entry_hash {
            self.report(
                row.seq,
                BreakKind::PrevHashMismatch {
                    expected: self.last.entry_hash,
                    found: row.prev_hash,
                },
            );
        }
        if row.hash_version == HASH_VERSION {
            let computed = compute_hash(&row.event, row.seq, row.hash_version, &row.prev_hash);
            if computed != row.entry_hash {
                self.report(
                    row.seq,
                    BreakKind::EntryHashMismatch {
                        computed,
                        stored: row.entry_hash,
                    },
                );
            }
        } else {
            self.report(row.seq, BreakKind::UnsupportedHashVersion(row.hash_version));
        }
        self.last = ChainHead {
            seq: row.seq,
            entry_hash: row.entry_hash,
        };
    }

    /// Record a row that could not be decoded into a [`SealedEvent`].
    pub fn push_undecodable(&mut self, seq: i64, stored_hash: Option<Hash>, error: String) {
        self.checked += 1;
        self.report(seq, BreakKind::Undecodable(error));
        self.last = ChainHead {
            seq,
            entry_hash: stored_hash.unwrap_or(GENESIS_HASH),
        };
    }

    /// Finish, comparing against the stored chain head when available.
    pub fn finish(mut self, head: Option<ChainHead>) -> VerifyReport {
        if let Some(head) = head
            && head != self.last
        {
            self.report(
                head.seq,
                BreakKind::HeadMismatch {
                    head,
                    last: self.last,
                },
            );
        }
        VerifyReport {
            entries_checked: self.checked,
            last: self.last,
            breaks: self.breaks,
            truncated: self.truncated,
        }
    }
}
