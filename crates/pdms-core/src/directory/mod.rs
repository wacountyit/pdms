// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Users, departments, groups, roles, and IdP group mappings.
//!
//! Reads are open to any authenticated caller that reaches them through the web
//! layer's capability check; every mutation requires
//! [`Capability::ManageDirectory`] and is audited in the same transaction.

mod admin;
pub mod entra_values;
pub mod sync;

pub use admin::*;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{CoreError, CoreResult};
use crate::model::{MembershipSource, UserStatus};

#[derive(Clone, Debug, Serialize)]
pub struct Department {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub external_id: Option<String>,
    pub is_active: bool,
    pub member_count: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub display_name: String,
    pub email: String,
    pub username: Option<String>,
    pub status: UserStatus,
    pub last_login_at: Option<DateTime<Utc>>,
    pub has_local_login: bool,
    pub oidc_linked: bool,
    pub departments: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Membership {
    pub department_id: Uuid,
    pub department_name: String,
    pub is_primary: bool,
    pub source: MembershipSource,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupMembership {
    pub group_id: Uuid,
    pub group_name: String,
    pub source: MembershipSource,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoleAssignment {
    pub id: Uuid,
    pub role_id: Uuid,
    pub role_key: String,
    pub role_name: String,
    pub department_id: Option<Uuid>,
    pub department_name: Option<String>,
    pub source: MembershipSource,
}

#[derive(Clone, Debug, Serialize)]
pub struct UserDetail {
    pub user: UserSummary,
    pub memberships: Vec<Membership>,
    pub groups: Vec<GroupMembership>,
    pub roles: Vec<RoleAssignment>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub external_id: Option<String>,
    pub member_count: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupMember {
    pub user_id: Uuid,
    pub display_name: String,
    pub email: String,
    pub source: MembershipSource,
}

#[derive(Clone, Debug, Serialize)]
pub struct Role {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub description: String,
    pub is_system: bool,
    pub holder_count: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct IdpMapping {
    pub id: Uuid,
    pub idp_group_id: String,
    pub description: String,
    pub target: String,
}

pub async fn list_departments(db: &PgPool) -> CoreResult<Vec<Department>> {
    Ok(sqlx::query_as!(
        Department,
        r#"SELECT d.id, d.code, d.name, d.external_id, d.is_active,
                  (SELECT count(*) FROM department_members m WHERE m.department_id = d.id) AS "member_count!"
             FROM departments d ORDER BY d.name"#
    )
    .fetch_all(db)
    .await?)
}

pub async fn get_department(db: &PgPool, id: Uuid) -> CoreResult<Department> {
    sqlx::query_as!(
        Department,
        r#"SELECT d.id, d.code, d.name, d.external_id, d.is_active,
                  (SELECT count(*) FROM department_members m WHERE m.department_id = d.id) AS "member_count!"
             FROM departments d WHERE d.id = $1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(CoreError::NotFound)
}

pub async fn department_members(db: &PgPool, id: Uuid) -> CoreResult<Vec<GroupMember>> {
    Ok(sqlx::query_as!(
        GroupMember,
        r#"SELECT u.id AS user_id, u.display_name, u.email, m.source AS "source: MembershipSource"
             FROM department_members m JOIN users u ON u.id = m.user_id
            WHERE m.department_id = $1 ORDER BY u.display_name"#,
        id
    )
    .fetch_all(db)
    .await?)
}

/// Users matching `query` (name or email substring), at most `limit`.
pub async fn list_users(db: &PgPool, query: &str, limit: i64) -> CoreResult<Vec<UserSummary>> {
    let pattern = format!("%{}%", escape_like(query.trim()));
    Ok(sqlx::query_as!(
        UserSummary,
        r#"SELECT u.id, u.display_name, u.email, u.username, u.status AS "status: UserStatus", u.last_login_at,
                  EXISTS (SELECT 1 FROM local_credentials c WHERE c.user_id = u.id) AS "has_local_login!",
                  (u.oidc_subject IS NOT NULL) AS "oidc_linked!",
                  ARRAY(SELECT d.name FROM department_members m JOIN departments d ON d.id = m.department_id
                         WHERE m.user_id = u.id ORDER BY m.is_primary DESC, d.name) AS "departments!"
             FROM users u
            WHERE u.display_name ILIKE $1 OR u.email ILIKE $1
            ORDER BY u.display_name
            LIMIT $2"#,
        pattern,
        limit
    )
    .fetch_all(db)
    .await?)
}

pub async fn get_user(db: &PgPool, id: Uuid) -> CoreResult<UserDetail> {
    let user = sqlx::query_as!(
        UserSummary,
        r#"SELECT u.id, u.display_name, u.email, u.username, u.status AS "status: UserStatus", u.last_login_at,
                  EXISTS (SELECT 1 FROM local_credentials c WHERE c.user_id = u.id) AS "has_local_login!",
                  (u.oidc_subject IS NOT NULL) AS "oidc_linked!",
                  ARRAY(SELECT d.name FROM department_members m JOIN departments d ON d.id = m.department_id
                         WHERE m.user_id = u.id ORDER BY m.is_primary DESC, d.name) AS "departments!"
             FROM users u WHERE u.id = $1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let memberships = sqlx::query_as!(
        Membership,
        r#"SELECT m.department_id, d.name AS department_name, m.is_primary,
                  m.source AS "source: MembershipSource"
             FROM department_members m JOIN departments d ON d.id = m.department_id
            WHERE m.user_id = $1 ORDER BY m.is_primary DESC, d.name"#,
        id
    )
    .fetch_all(db)
    .await?;
    let groups = sqlx::query_as!(
        GroupMembership,
        r#"SELECT g.id AS group_id, g.name AS group_name, gm.source AS "source: MembershipSource"
             FROM group_members gm JOIN groups g ON g.id = gm.group_id
            WHERE gm.user_id = $1 ORDER BY g.name"#,
        id
    )
    .fetch_all(db)
    .await?;
    let roles = sqlx::query_as!(
        RoleAssignment,
        r#"SELECT ra.id, ra.role_id, r.key AS role_key, r.name AS role_name, ra.department_id,
                  d.name AS "department_name?", ra.source AS "source: MembershipSource"
             FROM role_assignments ra
             JOIN roles r ON r.id = ra.role_id
             LEFT JOIN departments d ON d.id = ra.department_id
            WHERE ra.user_id = $1 ORDER BY r.name, d.name NULLS FIRST"#,
        id
    )
    .fetch_all(db)
    .await?;
    Ok(UserDetail {
        user,
        memberships,
        groups,
        roles,
    })
}

pub async fn list_groups(db: &PgPool) -> CoreResult<Vec<Group>> {
    Ok(sqlx::query_as!(
        Group,
        r#"SELECT g.id, g.name, g.description, g.external_id,
                  (SELECT count(*) FROM group_members m WHERE m.group_id = g.id) AS "member_count!"
             FROM groups g ORDER BY g.name"#
    )
    .fetch_all(db)
    .await?)
}

pub async fn get_group(db: &PgPool, id: Uuid) -> CoreResult<(Group, Vec<GroupMember>)> {
    let group = sqlx::query_as!(
        Group,
        r#"SELECT g.id, g.name, g.description, g.external_id,
                  (SELECT count(*) FROM group_members m WHERE m.group_id = g.id) AS "member_count!"
             FROM groups g WHERE g.id = $1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let members = sqlx::query_as!(
        GroupMember,
        r#"SELECT u.id AS user_id, u.display_name, u.email, m.source AS "source: MembershipSource"
             FROM group_members m JOIN users u ON u.id = m.user_id
            WHERE m.group_id = $1 ORDER BY u.display_name"#,
        id
    )
    .fetch_all(db)
    .await?;
    Ok((group, members))
}

pub async fn list_roles(db: &PgPool) -> CoreResult<Vec<Role>> {
    Ok(sqlx::query_as!(
        Role,
        r#"SELECT r.id, r.key, r.name, r.description, r.is_system,
                  (SELECT count(*) FROM role_assignments a WHERE a.role_id = r.id) AS "holder_count!"
             FROM roles r ORDER BY r.is_system DESC, r.name"#
    )
    .fetch_all(db)
    .await?)
}

pub async fn role_id_by_key(db: &PgPool, key: &str) -> CoreResult<Uuid> {
    sqlx::query_scalar!("SELECT id FROM roles WHERE key = $1", key)
        .fetch_optional(db)
        .await?
        .ok_or(CoreError::NotFound)
}

pub async fn list_idp_mappings(db: &PgPool) -> CoreResult<Vec<IdpMapping>> {
    Ok(sqlx::query_as!(
        IdpMapping,
        r#"SELECT m.id, m.idp_group_id, m.description,
                  CASE
                    WHEN m.department_id IS NOT NULL THEN 'Department: ' || d.name
                    WHEN m.group_id IS NOT NULL THEN 'Group: ' || g.name
                    ELSE 'Role: ' || r.name || coalesce(' (' || rd.name || ')', '')
                  END AS "target!"
             FROM idp_group_mappings m
             LEFT JOIN departments d ON d.id = m.department_id
             LEFT JOIN groups g ON g.id = m.group_id
             LEFT JOIN roles r ON r.id = m.role_id
             LEFT JOIN departments rd ON rd.id = m.role_department_id
            ORDER BY m.idp_group_id"#
    )
    .fetch_all(db)
    .await?)
}

pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
