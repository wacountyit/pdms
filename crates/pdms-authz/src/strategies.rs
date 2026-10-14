// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Proptest generators over a small fixed universe of IDs, so generated
//! principals, grants, and documents collide often enough to be interesting.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, TimeZone, Utc};
use proptest::prelude::*;
use uuid::Uuid;

use crate::model::{DocumentAttrs, Grant, Permission, Principal, RoleHolding, Scope, Subject};

pub const USERS: [Uuid; 3] = [
    Uuid::from_u128(0x1001),
    Uuid::from_u128(0x1002),
    Uuid::from_u128(0x1003),
];
pub const DEPARTMENTS: [Uuid; 3] = [
    Uuid::from_u128(0x2001),
    Uuid::from_u128(0x2002),
    Uuid::from_u128(0x2003),
];
pub const GROUPS: [Uuid; 2] = [Uuid::from_u128(0x3001), Uuid::from_u128(0x3002)];
pub const ROLES: [Uuid; 2] = [Uuid::from_u128(0x4001), Uuid::from_u128(0x4002)];
pub const DOCUMENTS: [Uuid; 3] = [
    Uuid::from_u128(0x5001),
    Uuid::from_u128(0x5002),
    Uuid::from_u128(0x5003),
];

/// Fixed "now" so expiry comparisons are deterministic.
pub fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

fn pick<const N: usize>(ids: [Uuid; N]) -> impl Strategy<Value = Uuid> {
    proptest::sample::select(ids.to_vec())
}

fn subset<const N: usize>(ids: [Uuid; N]) -> impl Strategy<Value = BTreeSet<Uuid>> {
    proptest::sample::subsequence(ids.to_vec(), 0..=N).prop_map(|v| v.into_iter().collect())
}

pub fn permission() -> impl Strategy<Value = Permission> {
    proptest::sample::select(Permission::ALL.to_vec())
}

pub fn subject() -> impl Strategy<Value = Subject> {
    prop_oneof![
        pick(USERS).prop_map(Subject::User),
        pick(GROUPS).prop_map(Subject::Group),
        pick(DEPARTMENTS).prop_map(Subject::Department),
        pick(ROLES).prop_map(Subject::Role),
    ]
}

pub fn scope() -> impl Strategy<Value = Scope> {
    prop_oneof![
        Just(Scope::Global),
        pick(DEPARTMENTS).prop_map(Scope::Department),
        pick(DOCUMENTS).prop_map(Scope::Document),
    ]
}

pub fn expiry() -> impl Strategy<Value = Option<DateTime<Utc>>> {
    prop_oneof![
        3 => Just(None),
        1 => Just(Some(now() - Duration::days(1))),
        1 => Just(Some(now() + Duration::days(1))),
    ]
}

/// A grant satisfying the DB constraints (no document-scoped `create`).
pub fn grant() -> impl Strategy<Value = Grant> {
    (subject(), scope(), permission(), expiry())
        .prop_filter("document-scoped create is not storable", |(_, sc, p, _)| {
            !(matches!(sc, Scope::Document(_)) && *p == Permission::Create)
        })
        .prop_map(|(subject, scope, permission, expires_at)| Grant {
            id: Uuid::new_v4(),
            subject,
            scope,
            permission,
            expires_at,
        })
}

/// A set of grants with no duplicate (subject, scope, permission) key.
pub fn grants() -> impl Strategy<Value = Vec<Grant>> {
    proptest::collection::vec(grant(), 0..10).prop_map(|gs| {
        let mut seen = std::collections::HashSet::new();
        gs.into_iter()
            .filter(|g| seen.insert((g.subject, g.scope, g.permission)))
            .collect()
    })
}

pub fn role_holding() -> impl Strategy<Value = RoleHolding> {
    (pick(ROLES), proptest::option::of(pick(DEPARTMENTS))).prop_map(|(role_id, department_id)| {
        RoleHolding {
            role_id,
            role_key: format!("role_{}", role_id.as_u128()),
            department_id,
        }
    })
}

pub fn principal() -> impl Strategy<Value = Principal> {
    (
        pick(USERS),
        prop::bool::weighted(0.9),
        subset(DEPARTMENTS),
        subset(GROUPS),
        proptest::collection::vec(role_holding(), 0..3),
    )
        .prop_map(|(user_id, active, departments, groups, mut roles)| {
            roles.sort_by_key(|r| (r.role_id, r.department_id));
            roles.dedup_by_key(|r| (r.role_id, r.department_id));
            Principal {
                user_id,
                active,
                departments,
                groups,
                roles,
            }
        })
}

pub fn document() -> impl Strategy<Value = DocumentAttrs> {
    (
        pick(DOCUMENTS),
        pick(DEPARTMENTS),
        pick(USERS),
        prop::bool::weighted(0.8),
        prop::bool::weighted(0.2),
    )
        .prop_map(
            |(id, department_id, owner_id, inherit_acl, is_confidential)| DocumentAttrs {
                id,
                department_id,
                owner_id,
                inherit_acl,
                is_confidential,
            },
        )
}
