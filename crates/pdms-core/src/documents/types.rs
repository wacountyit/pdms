// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document types and retention policies (administrator-managed, audited).

use pdms_audit::{AuditEvent, Category};
use pdms_authz::Capability;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::retention::RetentionBasis;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

#[derive(Clone, Debug, Serialize)]
pub struct DocumentType {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub description: String,
    pub id_prefix: String,
    pub review_interval_months: Option<i32>,
    pub requires_legal_review: bool,
    pub requires_signatures: bool,
    pub retention_policy_id: Option<Uuid>,
    pub retention_policy_name: Option<String>,
    pub is_active: bool,
    pub sort_order: i32,
    pub document_count: i64,
}

pub async fn list_types(db: &PgPool) -> CoreResult<Vec<DocumentType>> {
    Ok(sqlx::query_as!(
        DocumentType,
        r#"SELECT t.id, t.key, t.name, t.description, t.id_prefix, t.review_interval_months,
                  t.requires_legal_review, t.requires_signatures, t.retention_policy_id,
                  r.name AS "retention_policy_name?", t.is_active, t.sort_order,
                  (SELECT count(*) FROM documents d WHERE d.document_type_id = t.id AND d.deleted_at IS NULL)
                    AS "document_count!"
             FROM document_types t LEFT JOIN retention_policies r ON r.id = t.retention_policy_id
            ORDER BY t.sort_order, t.name"#
    )
    .fetch_all(db)
    .await?)
}

pub async fn get_type(db: &PgPool, id: Uuid) -> CoreResult<DocumentType> {
    list_types(db)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or(CoreError::NotFound)
}

pub async fn type_by_key(db: &PgPool, key: &str) -> CoreResult<DocumentType> {
    list_types(db)
        .await?
        .into_iter()
        .find(|t| t.key == key)
        .ok_or(CoreError::NotFound)
}

#[derive(Clone, Debug)]
pub struct TypeSpec {
    pub name: String,
    pub description: String,
    pub id_prefix: String,
    pub review_interval_months: Option<i32>,
    pub requires_legal_review: bool,
    pub requires_signatures: bool,
    pub retention_policy_id: Option<Uuid>,
    pub is_active: bool,
    pub sort_order: i32,
}

fn check_type(spec: &mut TypeSpec) -> CoreResult<()> {
    spec.name = spec.name.trim().to_owned();
    spec.description = spec.description.trim().chars().take(2000).collect();
    spec.id_prefix = spec.id_prefix.trim().to_ascii_uppercase();
    if spec.name.is_empty() || spec.name.chars().count() > 200 {
        return Err(CoreError::validation("name must be 1–200 characters"));
    }
    if !(2..=8).contains(&spec.id_prefix.len())
        || !spec.id_prefix.chars().all(|c| c.is_ascii_uppercase())
    {
        return Err(CoreError::validation("ID prefix must be 2–8 letters"));
    }
    if spec
        .review_interval_months
        .is_some_and(|m| !(1..=600).contains(&m))
    {
        return Err(CoreError::validation(
            "review interval must be 1–600 months",
        ));
    }
    Ok(())
}

fn type_changes(e: AuditEvent, before: Option<&DocumentType>, s: &TypeSpec) -> AuditEvent {
    e.change("name", before.map(|b| b.name.clone()), s.name.clone())
        .change(
            "description",
            before.map(|b| b.description.clone()),
            s.description.clone(),
        )
        .change(
            "id_prefix",
            before.map(|b| b.id_prefix.clone()),
            s.id_prefix.clone(),
        )
        .change(
            "review_interval_months",
            before.and_then(|b| b.review_interval_months),
            s.review_interval_months,
        )
        .change(
            "requires_legal_review",
            before.map(|b| b.requires_legal_review),
            s.requires_legal_review,
        )
        .change(
            "requires_signatures",
            before.map(|b| b.requires_signatures),
            s.requires_signatures,
        )
        .change(
            "retention_policy_id",
            before.and_then(|b| b.retention_policy_id),
            s.retention_policy_id,
        )
        .change("is_active", before.map(|b| b.is_active), s.is_active)
        .change("sort_order", before.map(|b| b.sort_order), s.sort_order)
}

pub async fn create_type(
    db: &PgPool,
    ctx: &Ctx,
    key: &str,
    mut spec: TypeSpec,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDocumentSchema)?;
    let key = key.trim().to_ascii_lowercase();
    if !(2..=64).contains(&key.len())
        || !key.starts_with(|c: char| c.is_ascii_lowercase())
        || !key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(CoreError::validation(
            "key must be 2–64 lowercase letters, digits, or _",
        ));
    }
    check_type(&mut spec)?;
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO document_types (id, key, name, description, id_prefix, review_interval_months,
                                     requires_legal_review, requires_signatures, retention_policy_id,
                                     is_active, sort_order)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        id,
        key,
        spec.name,
        spec.description,
        spec.id_prefix,
        spec.review_interval_months,
        spec.requires_legal_review,
        spec.requires_signatures,
        spec.retention_policy_id,
        spec.is_active,
        spec.sort_order,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a document type with that key already exists"))?;
    let e = AuditEvent::new(
        Category::Admin,
        "document_type.created",
        ctx.actor.clone(),
        "document_type",
    )
    .object(id, Some(&spec.name))
    .context("key", key.as_str());
    pdms_audit::append(&mut tx, vec![type_changes(e, None, &spec)]).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update_type(db: &PgPool, ctx: &Ctx, id: Uuid, mut spec: TypeSpec) -> CoreResult<()> {
    ctx.require(Capability::ManageDocumentSchema)?;
    check_type(&mut spec)?;
    let before = get_type(db, id).await?;
    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE document_types SET name = $2, description = $3, id_prefix = $4, review_interval_months = $5,
                requires_legal_review = $6, requires_signatures = $7, retention_policy_id = $8,
                is_active = $9, sort_order = $10, updated_at = now()
          WHERE id = $1",
        id,
        spec.name,
        spec.description,
        spec.id_prefix,
        spec.review_interval_months,
        spec.requires_legal_review,
        spec.requires_signatures,
        spec.retention_policy_id,
        spec.is_active,
        spec.sort_order,
    )
    .execute(&mut *tx)
    .await?;
    let e = AuditEvent::new(
        Category::Admin,
        "document_type.updated",
        ctx.actor.clone(),
        "document_type",
    )
    .object(id, Some(&spec.name))
    .context("key", before.key.as_str());
    let e = type_changes(e, Some(&before), &spec);
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Retention policies
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct RetentionPolicy {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub retain_years: Option<i32>,
    pub basis: RetentionBasis,
    pub legal_citation: String,
    pub used_by_types: i64,
}

impl RetentionPolicy {
    pub fn summary(&self) -> String {
        match self.retain_years {
            None => "Permanent".to_owned(),
            Some(y) => format!("{y} year(s) after {}", self.basis.label()),
        }
    }
}

pub async fn list_policies(db: &PgPool) -> CoreResult<Vec<RetentionPolicy>> {
    Ok(sqlx::query_as!(
        RetentionPolicy,
        r#"SELECT p.id, p.name, p.description, p.retain_years, p.basis AS "basis: RetentionBasis",
                  p.legal_citation,
                  (SELECT count(*) FROM document_types t WHERE t.retention_policy_id = p.id) AS "used_by_types!"
             FROM retention_policies p ORDER BY p.name"#
    )
    .fetch_all(db)
    .await?)
}

#[derive(Clone, Debug)]
pub struct PolicySpec {
    pub name: String,
    pub description: String,
    pub retain_years: Option<i32>,
    pub basis: RetentionBasis,
    pub legal_citation: String,
}

fn check_policy(p: &mut PolicySpec) -> CoreResult<()> {
    p.name = p.name.trim().to_owned();
    p.description = p.description.trim().chars().take(2000).collect();
    p.legal_citation = p.legal_citation.trim().chars().take(500).collect();
    if p.name.is_empty() || p.name.chars().count() > 200 {
        return Err(CoreError::validation("name must be 1–200 characters"));
    }
    if p.retain_years.is_some_and(|y| !(0..=200).contains(&y)) {
        return Err(CoreError::validation("retention must be 0–200 years"));
    }
    Ok(())
}

fn policy_changes(e: AuditEvent, before: Option<&RetentionPolicy>, p: &PolicySpec) -> AuditEvent {
    e.change("name", before.map(|b| b.name.clone()), p.name.clone())
        .change(
            "description",
            before.map(|b| b.description.clone()),
            p.description.clone(),
        )
        .change(
            "retain_years",
            before.and_then(|b| b.retain_years),
            p.retain_years,
        )
        .change("basis", before.map(|b| b.basis.as_str()), p.basis.as_str())
        .change(
            "legal_citation",
            before.map(|b| b.legal_citation.clone()),
            p.legal_citation.clone(),
        )
}

pub async fn create_policy(db: &PgPool, ctx: &Ctx, mut p: PolicySpec) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageRetention)?;
    check_policy(&mut p)?;
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO retention_policies (id, name, description, retain_years, basis, legal_citation)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        p.name,
        p.description,
        p.retain_years,
        p.basis as RetentionBasis,
        p.legal_citation,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a policy with that name already exists"))?;
    let e = AuditEvent::new(
        Category::Admin,
        "retention_policy.created",
        ctx.actor.clone(),
        "retention_policy",
    )
    .object(id, Some(&p.name));
    pdms_audit::append(&mut tx, vec![policy_changes(e, None, &p)]).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update_policy(db: &PgPool, ctx: &Ctx, id: Uuid, mut p: PolicySpec) -> CoreResult<()> {
    ctx.require(Capability::ManageRetention)?;
    check_policy(&mut p)?;
    let before = list_policies(db)
        .await?
        .into_iter()
        .find(|x| x.id == id)
        .ok_or(CoreError::NotFound)?;
    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE retention_policies SET name = $2, description = $3, retain_years = $4, basis = $5,
                legal_citation = $6, updated_at = now() WHERE id = $1",
        id,
        p.name,
        p.description,
        p.retain_years,
        p.basis as RetentionBasis,
        p.legal_citation,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a policy with that name already exists"))?;
    let e = policy_changes(
        AuditEvent::new(
            Category::Admin,
            "retention_policy.updated",
            ctx.actor.clone(),
            "retention_policy",
        )
        .object(id, Some(&p.name)),
        Some(&before),
        &p,
    );
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}
