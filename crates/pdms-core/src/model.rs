// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Database enums shared across modules.

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "user_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Active,
    Disabled,
    /// Left the organization; kept for history and ownership reports.
    Departed,
}

impl UserStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            UserStatus::Active => "active",
            UserStatus::Disabled => "disabled",
            UserStatus::Departed => "departed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            UserStatus::Active,
            UserStatus::Disabled,
            UserStatus::Departed,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "membership_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MembershipSource {
    Manual,
    /// Managed by IdP group sync; reconciled at each login.
    Idp,
}

impl MembershipSource {
    pub fn as_str(self) -> &'static str {
        match self {
            MembershipSource::Manual => "manual",
            MembershipSource::Idp => "idp",
        }
    }
}

/// Mirrors the `permission` enum; converts to and from [`pdms_authz::Permission`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "permission", rename_all = "snake_case")]
pub enum DbPermission {
    View,
    Create,
    Edit,
    Delete,
    Approve,
    ManagePermissions,
    Export,
}

impl From<DbPermission> for pdms_authz::Permission {
    fn from(p: DbPermission) -> Self {
        use pdms_authz::Permission as P;
        match p {
            DbPermission::View => P::View,
            DbPermission::Create => P::Create,
            DbPermission::Edit => P::Edit,
            DbPermission::Delete => P::Delete,
            DbPermission::Approve => P::Approve,
            DbPermission::ManagePermissions => P::ManagePermissions,
            DbPermission::Export => P::Export,
        }
    }
}

impl From<pdms_authz::Permission> for DbPermission {
    fn from(p: pdms_authz::Permission) -> Self {
        use pdms_authz::Permission as P;
        match p {
            P::View => DbPermission::View,
            P::Create => DbPermission::Create,
            P::Edit => DbPermission::Edit,
            P::Delete => DbPermission::Delete,
            P::Approve => DbPermission::Approve,
            P::ManagePermissions => DbPermission::ManagePermissions,
            P::Export => DbPermission::Export,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "acl_scope", rename_all = "snake_case")]
pub enum DbScope {
    Global,
    Department,
    Document,
}
