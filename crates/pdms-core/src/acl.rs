// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Grant storage and the bridge between `pdms-authz` and SQL.

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::{Capability, Grant, Permission, Scope, SqlFragment, SqlParam, SqlPart, Subject};
use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::model::{DbPermission, DbScope};

/// Append an authz predicate to a query, binding its parameters in order.
pub fn push_fragment(qb: &mut QueryBuilder<Postgres>, fragment: &SqlFragment) {
    for part in fragment.parts() {
        match part {
            SqlPart::Sql(s) => {
                qb.push(s.as_str());
            }
            SqlPart::Param(p) => match p {
                SqlParam::Uuid(u) => {
                    qb.push_bind(*u);
                }
                SqlParam::UuidArray(v) => {
                    qb.push_bind(v.clone());
                }
                SqlParam::NullableUuidArray(v) => {
                    qb.push_bind(v.clone());
                }
                SqlParam::PermissionArray(v) => {
                    qb.push_bind(v.iter().map(|p| p.as_str().to_owned()).collect::<Vec<_>>());
                }
                SqlParam::Timestamp(t) => {
                    qb.push_bind(*t);
                }
            },
        }
    }
}

struct GrantRow {
    id: Uuid,
    subject_user_id: Option<Uuid>,
    subject_group_id: Option<Uuid>,
    subject_department_id: Option<Uuid>,
    subject_role_id: Option<Uuid>,
    scope: DbScope,
    scope_department_id: Option<Uuid>,
    scope_document_id: Option<Uuid>,
    permission: DbPermission,
    expires_at: Option<DateTime<Utc>>,
}

impl GrantRow {
    fn into_grant(self) -> Option<Grant> {
        let subject = match (
            self.subject_user_id,
            self.subject_group_id,
            self.subject_department_id,
            self.subject_role_id,
        ) {
            (Some(u), None, None, None) => Subject::User(u),
            (None, Some(g), None, None) => Subject::Group(g),
            (None, None, Some(d), None) => Subject::Department(d),
            (None, None, None, Some(r)) => Subject::Role(r),
            _ => return None,
        };
        let scope = match (self.scope, self.scope_department_id, self.scope_document_id) {
            (DbScope::Global, None, None) => Scope::Global,
            (DbScope::Department, Some(d), None) => Scope::Department(d),
            (DbScope::Document, None, Some(d)) => Scope::Document(d),
            _ => return None,
        };
        Some(Grant {
            id: self.id,
            subject,
            scope,
            permission: self.permission.into(),
            expires_at: self.expires_at,
        })
    }
}

/// Every `create` grant at global or department scope, in one query: what
/// [`pdms_authz::decide_create`] needs for all departments at once. (A
/// document-scoped grant never allows creating.)
pub async fn create_grants(db: &PgPool) -> CoreResult<Vec<Grant>> {
    let rows = sqlx::query_as!(
        GrantRow,
        r#"SELECT id, subject_user_id, subject_group_id, subject_department_id, subject_role_id,
                  scope AS "scope: DbScope", scope_department_id, scope_document_id,
                  permission AS "permission: DbPermission", expires_at
             FROM acl_entries
            WHERE permission = 'create' AND scope IN ('global', 'department')"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().filter_map(GrantRow::into_grant).collect())
}

/// Every grant that could apply to a document in `department_id` (global,
/// that department, and the document itself), for [`pdms_authz::decide`].
pub async fn relevant_grants(
    db: &PgPool,
    department_id: Uuid,
    document_id: Option<Uuid>,
) -> CoreResult<Vec<Grant>> {
    let rows = sqlx::query_as!(
        GrantRow,
        r#"SELECT id, subject_user_id, subject_group_id, subject_department_id, subject_role_id,
                  scope AS "scope: DbScope", scope_department_id, scope_document_id,
                  permission AS "permission: DbPermission", expires_at
             FROM acl_entries
            WHERE scope = 'global'
               OR (scope = 'department' AND scope_department_id = $1)
               OR (scope = 'document' AND scope_document_id = $2)"#,
        department_id,
        document_id
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().filter_map(GrantRow::into_grant).collect())
}

/// A grant with human-readable labels, for admin screens and access reviews.
#[derive(Clone, Debug, Serialize)]
pub struct GrantView {
    pub id: Uuid,
    pub subject_kind: String,
    pub subject_label: String,
    pub scope_kind: String,
    pub scope_label: String,
    pub permission: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub granted_at: DateTime<Utc>,
    pub reason: String,
}

/// Grants at global or department scope (document grants are listed per document).
pub async fn list_grants(db: &PgPool, department: Option<Uuid>) -> CoreResult<Vec<GrantView>> {
    Ok(sqlx::query_as!(
        GrantView,
        r#"SELECT a.id,
                  CASE WHEN a.subject_user_id IS NOT NULL THEN 'user'
                       WHEN a.subject_group_id IS NOT NULL THEN 'group'
                       WHEN a.subject_department_id IS NOT NULL THEN 'department'
                       ELSE 'role' END AS "subject_kind!",
                  coalesce(su.display_name, sg.name, sd.name, sr.name, '?') AS "subject_label!",
                  a.scope::text AS "scope_kind!",
                  CASE a.scope WHEN 'global' THEN 'All documents'
                               WHEN 'department' THEN coalesce(scd.name, '?')
                               ELSE 'Document' END AS "scope_label!",
                  a.permission::text AS "permission!",
                  a.expires_at, gb.display_name AS "granted_by?", a.granted_at, a.reason
             FROM acl_entries a
             LEFT JOIN users su ON su.id = a.subject_user_id
             LEFT JOIN groups sg ON sg.id = a.subject_group_id
             LEFT JOIN departments sd ON sd.id = a.subject_department_id
             LEFT JOIN roles sr ON sr.id = a.subject_role_id
             LEFT JOIN departments scd ON scd.id = a.scope_department_id
             LEFT JOIN users gb ON gb.id = a.granted_by
            WHERE a.scope <> 'document'
              AND ($1::uuid IS NULL OR a.scope_department_id = $1)
            ORDER BY a.scope, scd.name NULLS FIRST, a.permission, 3"#,
        department
    )
    .fetch_all(db)
    .await?)
}

/// Grants on one document (for the document's sharing panel).
pub async fn list_document_grants(db: &PgPool, document: Uuid) -> CoreResult<Vec<GrantView>> {
    Ok(sqlx::query_as!(
        GrantView,
        r#"SELECT a.id,
                  CASE WHEN a.subject_user_id IS NOT NULL THEN 'user'
                       WHEN a.subject_group_id IS NOT NULL THEN 'group'
                       WHEN a.subject_department_id IS NOT NULL THEN 'department'
                       ELSE 'role' END AS "subject_kind!",
                  coalesce(su.display_name, sg.name, sd.name, sr.name, '?') AS "subject_label!",
                  a.scope::text AS "scope_kind!",
                  'This document' AS "scope_label!",
                  a.permission::text AS "permission!",
                  a.expires_at, gb.display_name AS "granted_by?", a.granted_at, a.reason
             FROM acl_entries a
             LEFT JOIN users su ON su.id = a.subject_user_id
             LEFT JOIN groups sg ON sg.id = a.subject_group_id
             LEFT JOIN departments sd ON sd.id = a.subject_department_id
             LEFT JOIN roles sr ON sr.id = a.subject_role_id
             LEFT JOIN users gb ON gb.id = a.granted_by
            WHERE a.scope_document_id = $1
            ORDER BY a.permission, 3"#,
        document
    )
    .fetch_all(db)
    .await?)
}

fn subject_columns(s: Subject) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) {
    match s {
        Subject::User(id) => (Some(id), None, None, None),
        Subject::Group(id) => (None, Some(id), None, None),
        Subject::Department(id) => (None, None, Some(id), None),
        Subject::Role(id) => (None, None, None, Some(id)),
    }
}

fn subject_str(s: Subject) -> String {
    match s {
        Subject::User(id) => format!("user:{id}"),
        Subject::Group(id) => format!("group:{id}"),
        Subject::Department(id) => format!("department:{id}"),
        Subject::Role(id) => format!("role:{id}"),
    }
}

fn scope_str(s: Scope) -> String {
    match s {
        Scope::Global => "global".to_owned(),
        Scope::Department(id) => format!("department:{id}"),
        Scope::Document(id) => format!("document:{id}"),
    }
}

/// Authorization for managing grants at a scope. Document scope arrives with
/// documents in Phase 1 (via `manage_permissions` on the document).
fn require_manage(ctx: &Ctx, scope: Scope) -> CoreResult<()> {
    match scope {
        Scope::Global | Scope::Department(_) => ctx.require(Capability::ManageDirectory),
        Scope::Document(_) => Err(CoreError::validation(
            "document grants are managed from the document",
        )),
    }
}

/// Grant at global or department scope (requires `ManageDirectory`).
/// Document sharing goes through `documents::share`, which checks
/// `manage_permissions` on the document.
pub async fn grant(
    db: &PgPool,
    ctx: &Ctx,
    subject: Subject,
    scope: Scope,
    permission: Permission,
    expires_at: Option<DateTime<Utc>>,
    reason: &str,
) -> CoreResult<Uuid> {
    require_manage(ctx, scope)?;
    grant_authorized(db, ctx, subject, scope, permission, expires_at, reason).await
}

/// Insert a grant. The caller must already have authorized the change.
pub(crate) async fn grant_authorized(
    db: &PgPool,
    ctx: &Ctx,
    subject: Subject,
    scope: Scope,
    permission: Permission,
    expires_at: Option<DateTime<Utc>>,
    reason: &str,
) -> CoreResult<Uuid> {
    if matches!(scope, Scope::Document(_)) && permission == Permission::Create {
        return Err(CoreError::validation(
            "`create` applies to departments, not documents",
        ));
    }
    if expires_at.is_some_and(|t| t <= Utc::now()) {
        return Err(CoreError::validation("expiry must be in the future"));
    }
    let reason = reason.trim().chars().take(500).collect::<String>();
    let (u, g, d, r) = subject_columns(subject);
    let (db_scope, scope_dept, scope_doc) = match scope {
        Scope::Global => (DbScope::Global, None, None),
        Scope::Department(id) => (DbScope::Department, Some(id), None),
        Scope::Document(id) => (DbScope::Document, None, Some(id)),
    };
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO acl_entries (id, subject_user_id, subject_group_id, subject_department_id, subject_role_id,
                                  scope, scope_department_id, scope_document_id, permission, granted_by, expires_at, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        id,
        u,
        g,
        d,
        r,
        db_scope as DbScope,
        scope_dept,
        scope_doc,
        DbPermission::from(permission) as DbPermission,
        ctx.user_id(),
        expires_at,
        reason,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(x) if x.is_foreign_key_violation() => CoreError::NotFound,
        _ => CoreError::conflict_on_unique(e, "that grant already exists"),
    })?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Permission,
                "acl.granted",
                ctx.actor.clone(),
                "acl_entry",
            )
            .object(id, None)
            .department(scope_dept)
            .document_opt(scope_doc)
            .change("subject", AuditValue::Null, subject_str(subject))
            .change("scope", AuditValue::Null, scope_str(scope))
            .change("permission", AuditValue::Null, permission.as_str())
            .change("expires_at", AuditValue::Null, expires_at)
            .context("reason", reason),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Revoke a grant. Effective on the next request: grants are never cached.
pub async fn revoke(db: &PgPool, ctx: &Ctx, grant_id: Uuid) -> CoreResult<()> {
    revoke_inner(db, ctx, grant_id, None).await
}

/// Revoke a document-scoped grant. The caller must have checked
/// `manage_permissions` on `document`; the grant must belong to it.
pub(crate) async fn revoke_document_grant(
    db: &PgPool,
    ctx: &Ctx,
    document: Uuid,
    grant_id: Uuid,
) -> CoreResult<()> {
    revoke_inner(db, ctx, grant_id, Some(document)).await
}

async fn revoke_inner(
    db: &PgPool,
    ctx: &Ctx,
    grant_id: Uuid,
    document: Option<Uuid>,
) -> CoreResult<()> {
    let mut tx = db.begin().await?;
    let row = sqlx::query_as!(
        GrantRow,
        r#"SELECT id, subject_user_id, subject_group_id, subject_department_id, subject_role_id,
                  scope AS "scope: DbScope", scope_department_id, scope_document_id,
                  permission AS "permission: DbPermission", expires_at
             FROM acl_entries WHERE id = $1 FOR UPDATE"#,
        grant_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    let g = row
        .into_grant()
        .ok_or_else(|| CoreError::internal("malformed grant row"))?;
    match (document, g.scope) {
        (Some(doc), Scope::Document(scope_doc)) if doc == scope_doc => {}
        (Some(_), _) => return Err(CoreError::NotFound),
        (None, scope) => require_manage(ctx, scope)?,
    }
    sqlx::query!("DELETE FROM acl_entries WHERE id = $1", grant_id)
        .execute(&mut *tx)
        .await?;
    let dept = match g.scope {
        Scope::Department(d) => Some(d),
        _ => None,
    };
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Permission,
                "acl.revoked",
                ctx.actor.clone(),
                "acl_entry",
            )
            .object(grant_id, None)
            .department(dept)
            .document_opt(document)
            .change("subject", subject_str(g.subject), AuditValue::Null)
            .change("scope", scope_str(g.scope), AuditValue::Null)
            .change("permission", g.permission.as_str(), AuditValue::Null)
            .change("expires_at", g.expires_at, AuditValue::Null),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
