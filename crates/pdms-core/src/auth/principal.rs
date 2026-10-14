// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::collections::BTreeSet;

use pdms_authz::{Principal, RoleHolding};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{CoreError, CoreResult};
use crate::model::UserStatus;

/// Load a user's authorization subject. Called on every request: nothing is
/// cached, so membership and role changes apply to the next request.
pub async fn load(db: &PgPool, user_id: Uuid) -> CoreResult<Principal> {
    let row = sqlx::query!(
        r#"SELECT u.status AS "status: UserStatus",
                  ARRAY(SELECT department_id FROM department_members WHERE user_id = u.id) AS "departments!",
                  ARRAY(SELECT group_id FROM group_members WHERE user_id = u.id) AS "groups!",
                  ARRAY(SELECT ra.role_id FROM role_assignments ra WHERE ra.user_id = u.id ORDER BY ra.id) AS "role_ids!",
                  ARRAY(SELECT r.key FROM role_assignments ra JOIN roles r ON r.id = ra.role_id
                         WHERE ra.user_id = u.id ORDER BY ra.id) AS "role_keys!",
                  ARRAY(SELECT ra.department_id FROM role_assignments ra WHERE ra.user_id = u.id ORDER BY ra.id)
                    AS "role_departments!: Vec<Option<Uuid>>"
             FROM users u
            WHERE u.id = $1"#,
        user_id
    )
    .fetch_optional(db)
    .await?
    .ok_or(CoreError::Unauthenticated)?;

    let roles = row
        .role_ids
        .into_iter()
        .zip(row.role_keys)
        .zip(row.role_departments)
        .map(|((role_id, role_key), department_id)| RoleHolding {
            role_id,
            role_key,
            department_id,
        })
        .collect();

    Ok(Principal {
        user_id,
        active: row.status == UserStatus::Active,
        departments: row.departments.into_iter().collect::<BTreeSet<_>>(),
        groups: row.groups.into_iter().collect::<BTreeSet<_>>(),
        roles,
    })
}
