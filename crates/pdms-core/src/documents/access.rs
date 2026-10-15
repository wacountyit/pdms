// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The single entry point for document authorization decisions.
//!
//! Every document operation calls [`authorize`] (or [`authorize_create`])
//! before touching data; list-style queries use the SQL predicate in
//! `search`. Denials are recorded in the audit log.

use chrono::Utc;
use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::{Decision, DocumentAttrs, Permission, PermissionSet, decide, decide_create};
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::{Core, acl};

/// What authorization needs to know about a document, plus labels for audit entries.
#[derive(Clone, Debug)]
pub struct DocRef {
    pub attrs: DocumentAttrs,
    pub external_id: String,
    pub title: String,
}

impl DocRef {
    pub fn id(&self) -> Uuid {
        self.attrs.id
    }

    pub fn label(&self) -> String {
        format!("{} {}", self.external_id, self.title)
    }

    /// An audit event about this document with object, document, and department filled in.
    pub fn event(&self, ctx: &Ctx, category: Category, action: &str) -> AuditEvent {
        AuditEvent::new(category, action, ctx.actor.clone(), "document")
            .object(self.id(), Some(&self.label()))
            .document(self.id())
            .department(Some(self.attrs.department_id))
    }
}

/// Load a live (not deleted) document's authorization attributes.
pub(crate) async fn load(core: &Core, id: Uuid) -> CoreResult<Option<DocRef>> {
    let row = sqlx::query!(
        r#"SELECT id, department_id, owner_id, visibility::text AS "visibility!", external_id, title
             FROM documents WHERE id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(&core.db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(DocRef {
        attrs: DocumentAttrs {
            id: r.id,
            department_id: r.department_id,
            owner_id: r.owner_id,
            visibility: r.visibility.parse().map_err(CoreError::internal)?,
        },
        external_id: r.external_id,
        title: r.title,
    }))
}

async fn record_denial(core: &Core, event: AuditEvent, perm: &str) -> CoreResult<()> {
    pdms_audit::record(
        &core.db,
        vec![
            event
                .outcome(Outcome::Denied)
                .context("required_permission", perm),
        ],
    )
    .await?;
    Ok(())
}

/// Require `perm` on the document. Missing or deleted documents are `NotFound`;
/// documents the caller cannot use are `Forbidden` (and audited).
pub async fn authorize(core: &Core, ctx: &Ctx, id: Uuid, perm: Permission) -> CoreResult<DocRef> {
    let doc = load(core, id).await?.ok_or(CoreError::NotFound)?;
    let Some(principal) = ctx.principal() else {
        return Ok(doc); // trusted system context
    };
    let grants = acl::relevant_grants(&core.db, doc.attrs.department_id, Some(id)).await?;
    match decide(
        &core.policy,
        principal,
        perm,
        &doc.attrs,
        &grants,
        Utc::now(),
    ) {
        Decision::Allow(_) => Ok(doc),
        Decision::Deny(reason) => {
            let e = AuditEvent::new(
                Category::Access,
                "access.denied",
                ctx.actor.clone(),
                "document",
            )
            .object(id, Some(&doc.label()))
            .document(id)
            .department(Some(doc.attrs.department_id));
            record_denial(core, e, perm.as_str()).await?;
            Err(CoreError::Forbidden(Some(reason)))
        }
    }
}

/// Whether the caller holds `perm` on a live document. Not audited: for
/// filtering and display decisions, not for granting access to content.
pub async fn can(core: &Core, ctx: &Ctx, id: Uuid, perm: Permission) -> CoreResult<bool> {
    let Some(doc) = load(core, id).await? else {
        return Ok(false);
    };
    let Some(principal) = ctx.principal() else {
        return Ok(true);
    };
    let grants = acl::relevant_grants(&core.db, doc.attrs.department_id, Some(id)).await?;
    Ok(decide(
        &core.policy,
        principal,
        perm,
        &doc.attrs,
        &grants,
        Utc::now(),
    )
    .is_allowed())
}

/// Every permission the caller holds on a document (for showing the right buttons).
pub async fn permissions(core: &Core, ctx: &Ctx, doc: &DocRef) -> CoreResult<PermissionSet> {
    let Some(principal) = ctx.principal() else {
        return Ok(PermissionSet::of(&Permission::ALL));
    };
    let grants = acl::relevant_grants(&core.db, doc.attrs.department_id, Some(doc.id())).await?;
    let now = Utc::now();
    Ok(Permission::ALL
        .into_iter()
        .filter(|p| decide(&core.policy, principal, *p, &doc.attrs, &grants, now).is_allowed())
        .fold(PermissionSet::EMPTY, PermissionSet::with))
}

/// Require `create` in a department.
pub async fn authorize_create(core: &Core, ctx: &Ctx, department_id: Uuid) -> CoreResult<()> {
    let Some(principal) = ctx.principal() else {
        return Ok(());
    };
    let grants = acl::relevant_grants(&core.db, department_id, None).await?;
    if decide_create(principal, department_id, &grants, Utc::now()).is_allowed() {
        return Ok(());
    }
    let e = AuditEvent::new(
        Category::Access,
        "access.denied",
        ctx.actor.clone(),
        "department",
    )
    .object(department_id, None)
    .department(Some(department_id));
    record_denial(core, e, "create").await?;
    Err(CoreError::Forbidden(None))
}

/// Departments where the caller may create documents.
pub async fn creatable_departments(core: &Core, ctx: &Ctx) -> CoreResult<Vec<(Uuid, String)>> {
    let depts = sqlx::query!("SELECT id, name FROM departments WHERE is_active ORDER BY name")
        .fetch_all(&core.db)
        .await?;
    let Some(principal) = ctx.principal() else {
        return Ok(depts.into_iter().map(|d| (d.id, d.name)).collect());
    };
    let now = Utc::now();
    let mut out = Vec::new();
    for d in depts {
        let grants = acl::relevant_grants(&core.db, d.id, None).await?;
        if decide_create(principal, d.id, &grants, now).is_allowed() {
            out.push((d.id, d.name));
        }
    }
    Ok(out)
}
