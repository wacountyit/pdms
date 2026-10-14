// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::capability::{Capability, has_capability};
use crate::model::{
    Assignee, DocumentAttrs, Grant, Permission, Policy, PositionAttrs, Principal, Scope,
};

/// Why access was allowed. Useful for explanations in the UI and access reviews.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AllowReason {
    Owner,
    Grant(Uuid),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DenyReason {
    InactiveUser,
    NoMatchingGrant,
    /// `create` applies to departments; use [`decide_create`].
    CreateIsDepartmentScoped,
    SelfApproval,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow(AllowReason),
    Deny(DenyReason),
}

impl Decision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Decision::Allow(_))
    }
}

/// Whether a grant's scope covers the document.
///
/// Department and global grants reach a document only when it inherits ACLs and
/// is not confidential; document grants always apply.
pub fn scope_covers(scope: Scope, doc: &DocumentAttrs) -> bool {
    let inherits = doc.inherit_acl && !doc.is_confidential;
    match scope {
        Scope::Document(id) => id == doc.id,
        Scope::Department(id) => inherits && id == doc.department_id,
        Scope::Global => inherits,
    }
}

/// Decide whether `principal` may perform `perm` on an existing document.
///
/// `grants` may contain unrelated grants; every rule is re-checked here, so
/// callers can pass a superset (for example every grant for the document, its
/// department, and global scope).
pub fn decide(
    policy: &Policy,
    principal: &Principal,
    perm: Permission,
    doc: &DocumentAttrs,
    grants: &[Grant],
    now: DateTime<Utc>,
) -> Decision {
    if !principal.active {
        return Decision::Deny(DenyReason::InactiveUser);
    }
    if perm == Permission::Create {
        return Decision::Deny(DenyReason::CreateIsDepartmentScoped);
    }
    if doc.owner_id == principal.user_id && policy.owner_permissions.satisfies(perm) {
        return Decision::Allow(AllowReason::Owner);
    }
    grants
        .iter()
        .find(|g| {
            perm.satisfied_by(g.permission)
                && g.is_live(now)
                && principal.matches_subject(g.subject, doc.department_id)
                && scope_covers(g.scope, doc)
        })
        .map_or(Decision::Deny(DenyReason::NoMatchingGrant), |g| {
            Decision::Allow(AllowReason::Grant(g.id))
        })
}

/// Decide whether `principal` may create documents in `department_id`.
pub fn decide_create(
    principal: &Principal,
    department_id: Uuid,
    grants: &[Grant],
    now: DateTime<Utc>,
) -> Decision {
    if !principal.active {
        return Decision::Deny(DenyReason::InactiveUser);
    }
    grants
        .iter()
        .find(|g| {
            g.permission == Permission::Create
                && g.is_live(now)
                && principal.matches_subject(g.subject, department_id)
                && match g.scope {
                    Scope::Global => true,
                    Scope::Department(id) => id == department_id,
                    Scope::Document(_) => false,
                }
        })
        .map_or(Decision::Deny(DenyReason::NoMatchingGrant), |g| {
            Decision::Allow(AllowReason::Grant(g.id))
        })
}

/// Separation of duties: may `approver` approve something `submitter` submitted?
/// Callers must also check the `approve` permission with [`decide`].
pub fn check_self_approval(
    policy: &Policy,
    approver: Uuid,
    submitter: Uuid,
) -> Result<(), DenyReason> {
    if approver == submitter && !policy.allow_self_approval {
        Err(DenyReason::SelfApproval)
    } else {
        Ok(())
    }
}

/// May `principal` see a position and its appointments? Appointment managers
/// and auditors see all; the position's manager and members of its
/// department see it.
pub fn can_view_position(principal: &Principal, position: &PositionAttrs) -> bool {
    principal.active
        && (has_capability(principal, Capability::ViewAllAppointments)
            || position.manager_user_id == Some(principal.user_id)
            || position
                .department_id
                .is_some_and(|d| principal.departments.contains(&d)))
}

/// May `principal` record requirement checklist items for appointments to a
/// position? Appointment managers and the position's manager may.
pub fn can_track_position(principal: &Principal, position: &PositionAttrs) -> bool {
    principal.active
        && (has_capability(principal, Capability::ManageAppointments)
            || position.manager_user_id == Some(principal.user_id))
}

/// Is `principal` the assignee of a workflow step on a document in
/// `department_id`? Callers must also check the document permission the
/// step needs with [`decide`].
pub fn is_assignee(principal: &Principal, assignee: Assignee, department_id: Uuid) -> bool {
    principal.active
        && match assignee {
            Assignee::User(id) => id == principal.user_id,
            Assignee::Role(id) => principal.holds_role_for(id, department_id),
        }
}
