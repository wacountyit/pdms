// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Append-only, hash-chained audit log.
//!
//! The pure part ([`AuditEvent`], [`seal`], [`Verifier`]) has no I/O. The
//! `postgres` feature adds [`append`], [`record`], and [`verify`].

mod chain;
mod event;
#[cfg(feature = "postgres")]
mod pg;

pub use chain::{
    Break, BreakKind, ChainHead, GENESIS_HASH, HASH_VERSION, Hash, SealedEvent, Verifier,
    VerifyReport, canonical_bytes, compute_hash, seal,
};
pub use event::{
    Actor, AuditEvent, AuditValue, AuthMethod, Category, FieldChange, NotAuditable, Outcome,
    UnknownVariant, format_timestamp,
};
#[cfg(feature = "postgres")]
pub use pg::{AUDIT_COLUMNS, AuditError, AuditRow, DecodeError, append, record, verify};

#[cfg(test)]
mod tests;
