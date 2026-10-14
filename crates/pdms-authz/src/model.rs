// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Document-level permission. Mirrors the `permission` Postgres enum.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    View,
    Create,
    Edit,
    Delete,
    Approve,
    ManagePermissions,
    Export,
}

impl Permission {
    pub const ALL: [Permission; 7] = [
        Permission::View,
        Permission::Create,
        Permission::Edit,
        Permission::Delete,
        Permission::Approve,
        Permission::ManagePermissions,
        Permission::Export,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::View => "view",
            Permission::Create => "create",
            Permission::Edit => "edit",
            Permission::Delete => "delete",
            Permission::Approve => "approve",
            Permission::ManagePermissions => "manage_permissions",
            Permission::Export => "export",
        }
    }

    /// Whether holding `granted` satisfies a request for `self`.
    /// Every permission except `create` implies `view`.
    pub fn satisfied_by(self, granted: Permission) -> bool {
        granted == self || (self == Permission::View && granted != Permission::Create)
    }

    /// All granted permissions that satisfy a request for `self`.
    pub fn satisfying(self) -> Vec<Permission> {
        Permission::ALL
            .into_iter()
            .filter(|g| self.satisfied_by(*g))
            .collect()
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown permission: {0}")]
pub struct UnknownPermission(pub String);

impl FromStr for Permission {
    type Err = UnknownPermission;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Permission::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| UnknownPermission(s.to_owned()))
    }
}

/// A small set of permissions.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct PermissionSet(u8);

impl PermissionSet {
    pub const EMPTY: PermissionSet = PermissionSet(0);

    pub fn of(perms: &[Permission]) -> Self {
        perms.iter().fold(Self::EMPTY, |s, p| s.with(*p))
    }

    pub fn with(self, p: Permission) -> Self {
        PermissionSet(self.0 | p.bit())
    }

    pub fn contains(self, p: Permission) -> bool {
        self.0 & p.bit() != 0
    }

    /// Whether any member of the set satisfies a request for `p`.
    pub fn satisfies(self, p: Permission) -> bool {
        self.iter().any(|g| p.satisfied_by(g))
    }

    pub fn iter(self) -> impl Iterator<Item = Permission> {
        Permission::ALL
            .into_iter()
            .filter(move |p| self.contains(*p))
    }
}

impl fmt::Debug for PermissionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// Who a grant is given to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Subject {
    User(Uuid),
    Group(Uuid),
    /// All members of the department.
    Department(Uuid),
    /// All holders of the role, org-wide or for the resource's department.
    Role(Uuid),
}

/// What a grant applies to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Scope {
    Global,
    Department(Uuid),
    Document(Uuid),
}

/// One row of `acl_entries`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: Uuid,
    pub subject: Subject,
    pub scope: Scope,
    pub permission: Permission,
    pub expires_at: Option<DateTime<Utc>>,
}

impl Grant {
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|exp| exp > now)
    }
}

/// A role held by a user, optionally limited to one department.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleHolding {
    pub role_id: Uuid,
    pub role_key: String,
    pub department_id: Option<Uuid>,
}

/// The authenticated actor, loaded fresh for every request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub user_id: Uuid,
    pub active: bool,
    pub departments: BTreeSet<Uuid>,
    pub groups: BTreeSet<Uuid>,
    pub roles: Vec<RoleHolding>,
}

impl Principal {
    /// Holds `role_id` org-wide, or scoped to `department_id`.
    pub fn holds_role_for(&self, role_id: Uuid, department_id: Uuid) -> bool {
        self.roles
            .iter()
            .any(|r| r.role_id == role_id && r.department_id.is_none_or(|d| d == department_id))
    }

    /// Holds the role identified by `key` org-wide.
    pub fn has_org_role(&self, key: &str) -> bool {
        self.roles
            .iter()
            .any(|r| r.role_key == key && r.department_id.is_none())
    }

    /// Whether a grant's subject covers this principal for a resource in `department_id`.
    pub fn matches_subject(&self, subject: Subject, department_id: Uuid) -> bool {
        match subject {
            Subject::User(id) => id == self.user_id,
            Subject::Group(id) => self.groups.contains(&id),
            Subject::Department(id) => self.departments.contains(&id),
            Subject::Role(id) => self.holds_role_for(id, department_id),
        }
    }
}

/// The attributes of a document that authorization depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentAttrs {
    pub id: Uuid,
    pub department_id: Uuid,
    pub owner_id: Uuid,
    pub inherit_acl: bool,
    pub is_confidential: bool,
}

/// Organization-configurable policy knobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// What a document's primary owner may do without an explicit grant.
    pub owner_permissions: PermissionSet,
    /// Whether a user may approve a submission they made.
    pub allow_self_approval: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            owner_permissions: PermissionSet::of(&[Permission::View, Permission::Edit]),
            allow_self_approval: false,
        }
    }
}

/// The attributes of a position (a seat on a board or an appointed role)
/// that authorization depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionAttrs {
    pub department_id: Option<Uuid>,
    /// The staff member responsible for the position.
    pub manager_user_id: Option<Uuid>,
}

/// Who a workflow step is assigned to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Assignee {
    User(Uuid),
    /// Any holder of the role, org-wide or for the document's department.
    Role(Uuid),
}
