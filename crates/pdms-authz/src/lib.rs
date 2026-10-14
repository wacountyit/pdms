// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Central authorization for PDMS.
//!
//! Pure logic only: callers load the [`Principal`] and relevant [`Grant`]s for
//! each request and ask this crate for a [`Decision`]. Queries that return many
//! documents embed [`visible_documents_predicate`] instead. Deny by default.

mod capability;
mod engine;
mod model;
mod sql;
#[cfg(any(test, feature = "strategies"))]
pub mod strategies;
#[cfg(test)]
mod tests;

pub use capability::{Capability, has_capability, role_keys};
pub use engine::{
    AllowReason, Decision, DenyReason, can_track_position, can_view_position, check_self_approval,
    decide, decide_create, is_assignee, scope_covers,
};
pub use model::{
    Assignee, DocumentAttrs, Grant, Permission, PermissionSet, Policy, PositionAttrs, Principal,
    RoleHolding, Scope, Subject, UnknownPermission,
};
pub use sql::{InvalidAlias, SqlFragment, SqlParam, SqlPart, visible_documents_predicate};
