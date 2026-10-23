// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Reconcile IdP-sourced memberships from a login's group claim.
//!
//! Only rows with `source = 'idp'` are added or removed. Manual memberships are
//! never touched, and an existing manual row wins over an IdP one.

use std::collections::BTreeSet;

use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::CoreResult;

/// Before/after snapshots (sorted labels) when anything changed.
#[derive(Debug, Clone, Default)]
pub struct SyncChange {
    pub departments_before: Vec<String>,
    pub departments_after: Vec<String>,
    pub groups_before: Vec<String>,
    pub groups_after: Vec<String>,
    pub roles_before: Vec<String>,
    pub roles_after: Vec<String>,
}

type RoleKey = (Uuid, Option<Uuid>);

pub async fn reconcile_idp_memberships(
    conn: &mut PgConnection,
    user_id: Uuid,
    idp_groups: &[String],
) -> CoreResult<Option<SyncChange>> {
    let mappings = sqlx::query!(
        "SELECT department_id, group_id, role_id, role_department_id
           FROM idp_group_mappings WHERE idp_group_id = ANY($1)",
        idp_groups
    )
    .fetch_all(&mut *conn)
    .await?;
    let want_depts: BTreeSet<Uuid> = mappings.iter().filter_map(|m| m.department_id).collect();
    let want_groups: BTreeSet<Uuid> = mappings.iter().filter_map(|m| m.group_id).collect();
    let want_roles: BTreeSet<RoleKey> = mappings
        .iter()
        .filter_map(|m| m.role_id.map(|r| (r, m.role_department_id)))
        .collect();

    let have_depts: BTreeSet<Uuid> = sqlx::query_scalar!(
        "SELECT department_id FROM department_members WHERE user_id = $1 AND source = 'idp'",
        user_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let have_groups: BTreeSet<Uuid> = sqlx::query_scalar!(
        "SELECT group_id FROM group_members WHERE user_id = $1 AND source = 'idp'",
        user_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let have_roles: BTreeSet<RoleKey> = sqlx::query!(
        "SELECT role_id, department_id FROM role_assignments WHERE user_id = $1 AND source = 'idp'",
        user_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| (r.role_id, r.department_id))
    .collect();

    if want_depts == have_depts && want_groups == have_groups && want_roles == have_roles {
        return Ok(None);
    }
    if want_depts != have_depts {
        crate::sharepoint::access::enqueue_reconcile(&mut *conn).await?;
    }

    let before = snapshot(conn, user_id).await?;

    for d in have_depts.difference(&want_depts) {
        sqlx::query!(
            "DELETE FROM department_members WHERE user_id = $1 AND department_id = $2 AND source = 'idp'",
            user_id,
            d
        )
        .execute(&mut *conn)
        .await?;
    }
    for d in want_depts.difference(&have_depts) {
        sqlx::query!(
            "INSERT INTO department_members (department_id, user_id, is_primary, source)
             VALUES ($1, $2, NOT EXISTS (SELECT 1 FROM department_members WHERE user_id = $2 AND is_primary), 'idp')
             ON CONFLICT (department_id, user_id) DO NOTHING",
            d,
            user_id
        )
        .execute(&mut *conn)
        .await?;
    }
    for g in have_groups.difference(&want_groups) {
        sqlx::query!(
            "DELETE FROM group_members WHERE user_id = $1 AND group_id = $2 AND source = 'idp'",
            user_id,
            g
        )
        .execute(&mut *conn)
        .await?;
    }
    for g in want_groups.difference(&have_groups) {
        sqlx::query!(
            "INSERT INTO group_members (group_id, user_id, source) VALUES ($1, $2, 'idp')
             ON CONFLICT (group_id, user_id) DO NOTHING",
            g,
            user_id
        )
        .execute(&mut *conn)
        .await?;
    }
    for (role, dept) in have_roles.difference(&want_roles) {
        sqlx::query!(
            "DELETE FROM role_assignments
              WHERE user_id = $1 AND role_id = $2 AND department_id IS NOT DISTINCT FROM $3 AND source = 'idp'",
            user_id,
            role,
            *dept
        )
        .execute(&mut *conn)
        .await?;
    }
    for (role, dept) in want_roles.difference(&have_roles) {
        sqlx::query!(
            "INSERT INTO role_assignments (id, role_id, user_id, department_id, source)
             VALUES ($1, $2, $3, $4, 'idp')
             ON CONFLICT ON CONSTRAINT role_assignments_key DO NOTHING",
            Uuid::now_v7(),
            role,
            user_id,
            *dept
        )
        .execute(&mut *conn)
        .await?;
    }

    let after = snapshot(conn, user_id).await?;
    if before == after {
        return Ok(None);
    }
    Ok(Some(SyncChange {
        departments_before: before.0,
        departments_after: after.0,
        groups_before: before.1,
        groups_after: after.1,
        roles_before: before.2,
        roles_after: after.2,
    }))
}

type Snapshot = (Vec<String>, Vec<String>, Vec<String>);

async fn snapshot(conn: &mut PgConnection, user_id: Uuid) -> CoreResult<Snapshot> {
    let r = sqlx::query!(
        r#"SELECT
             ARRAY(SELECT d.code FROM department_members m JOIN departments d ON d.id = m.department_id
                    WHERE m.user_id = $1 ORDER BY d.code) AS "departments!",
             ARRAY(SELECT g.name FROM group_members m JOIN groups g ON g.id = m.group_id
                    WHERE m.user_id = $1 ORDER BY g.name) AS "groups!",
             ARRAY(SELECT r.key || coalesce('@' || d.code, '') FROM role_assignments ra
                     JOIN roles r ON r.id = ra.role_id LEFT JOIN departments d ON d.id = ra.department_id
                    WHERE ra.user_id = $1 ORDER BY 1) AS "roles!""#,
        user_id
    )
    .fetch_one(conn)
    .await?;
    Ok((r.departments, r.groups, r.roles))
}
