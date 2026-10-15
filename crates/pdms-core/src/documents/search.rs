// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document search, filtering, sorting, and list export.
//!
//! Every list query in the system goes through [`query`], which always starts
//! from the authorization predicate built by `pdms-authz`, so nothing can list
//! a document the caller is not allowed to see.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::{Permission, Visibility, visible_documents_predicate};
use serde::Serialize;
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::fields::{self, FieldDef, FieldType};
use super::status::DocumentStatus;
use super::tags::TagRef;
use crate::acl::push_fragment;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::{Core, directory};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DateField {
    Execution,
    Effective,
    LastReview,
    NextReview,
    Renewal,
    Expiration,
}

impl DateField {
    pub const ALL: [DateField; 6] = [
        DateField::NextReview,
        DateField::Renewal,
        DateField::Expiration,
        DateField::Effective,
        DateField::Execution,
        DateField::LastReview,
    ];

    pub fn column(self) -> &'static str {
        match self {
            DateField::Execution => "execution_date",
            DateField::Effective => "effective_date",
            DateField::LastReview => "last_review_date",
            DateField::NextReview => "next_review_date",
            DateField::Renewal => "renewal_date",
            DateField::Expiration => "expiration_date",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DateField::Execution => "Execution date",
            DateField::Effective => "Effective date",
            DateField::LastReview => "Last review",
            DateField::NextReview => "Next review",
            DateField::Renewal => "Renewal date",
            DateField::Expiration => "Expiration date",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.column() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SortField {
    Relevance,
    Title,
    ExternalId,
    Status,
    Updated,
    Created,
    Date(DateField),
    /// A custom field key.
    Custom(String),
}

impl SortField {
    pub fn parse(s: &str) -> Self {
        match s {
            "title" => SortField::Title,
            "external_id" => SortField::ExternalId,
            "status" => SortField::Status,
            "updated" => SortField::Updated,
            "created" => SortField::Created,
            "relevance" => SortField::Relevance,
            other => match (DateField::parse(other), other.strip_prefix("cf.")) {
                (Some(d), _) => SortField::Date(d),
                (None, Some(k)) => SortField::Custom(k.to_owned()),
                _ => SortField::Updated,
            },
        }
    }

    pub fn key(&self) -> String {
        match self {
            SortField::Relevance => "relevance".into(),
            SortField::Title => "title".into(),
            SortField::ExternalId => "external_id".into(),
            SortField::Status => "status".into(),
            SortField::Updated => "updated".into(),
            SortField::Created => "created".into(),
            SortField::Date(d) => d.column().into(),
            SortField::Custom(k) => format!("cf.{k}"),
        }
    }
}

/// A filter on one custom field. Text fields match by substring, numbers and
/// dates by range, everything else by equality.
#[derive(Clone, Debug, Default)]
pub struct CustomFilter {
    pub key: String,
    pub value: Option<String>,
    pub min: Option<String>,
    pub max: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DocumentFilter {
    pub q: Option<String>,
    pub document_type_id: Option<Uuid>,
    pub department_id: Option<Uuid>,
    pub status: Option<DocumentStatus>,
    pub owner_id: Option<Uuid>,
    pub tags: Vec<Uuid>,
    /// Require every tag (AND) instead of any (OR).
    pub all_tags: bool,
    pub date_field: Option<DateField>,
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub custom: Option<CustomFilter>,
    /// Archived documents are hidden unless asked for (or filtered by status).
    pub include_archived: bool,
    pub sort: SortField,
    pub descending: bool,
    pub page: i64,
    pub per_page: i64,
}

impl Default for DocumentFilter {
    fn default() -> Self {
        DocumentFilter {
            q: None,
            document_type_id: None,
            department_id: None,
            status: None,
            owner_id: None,
            tags: Vec::new(),
            all_tags: false,
            date_field: None,
            date_from: None,
            date_to: None,
            custom: None,
            include_archived: false,
            sort: SortField::Updated,
            descending: true,
            page: 1,
            per_page: 50,
        }
    }
}

impl DocumentFilter {
    fn describe(&self) -> AuditValue {
        let mut m = std::collections::BTreeMap::new();
        let mut put = |k: &str, v: AuditValue| {
            if v != AuditValue::Null {
                m.insert(k.to_owned(), v);
            }
        };
        put("q", self.q.clone().into());
        put("document_type_id", self.document_type_id.into());
        put("department_id", self.department_id.into());
        put("status", self.status.map(|s| s.as_str()).into());
        put("owner_id", self.owner_id.into());
        if !self.tags.is_empty() {
            put("tags", self.tags.clone().into());
            put("all_tags", self.all_tags.into());
        }
        put("date_field", self.date_field.map(|d| d.column()).into());
        put("date_from", self.date_from.into());
        put("date_to", self.date_to.into());
        if let Some(c) = &self.custom {
            put("custom_key", c.key.clone().into());
            put("custom_value", c.value.clone().into());
            put("custom_min", c.min.clone().into());
            put("custom_max", c.max.clone().into());
        }
        put("include_archived", self.include_archived.into());
        AuditValue::Map(m)
    }
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct DocumentSummary {
    pub id: Uuid,
    pub external_id: String,
    pub title: String,
    pub type_name: String,
    pub department_name: String,
    pub owner_name: String,
    pub status: DocumentStatus,
    pub execution_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub last_review_date: Option<NaiveDate>,
    pub next_review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    /// Effective review interval (the document's own, else its type's).
    pub review_interval_months: Option<i32>,
    #[sqlx(try_from = "String")]
    pub visibility: Visibility,
    pub custom_fields: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: Json<Vec<TagRef>>,
    #[serde(skip)]
    pub total: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResults {
    pub items: Vec<DocumentSummary>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

fn custom_expr(def: &FieldDef) -> &'static str {
    match def.field_type {
        FieldType::Number | FieldType::Date => "(NULLIF(d.custom_fields->>",
        _ => "(d.custom_fields->>",
    }
}

fn custom_cast(def: &FieldDef) -> &'static str {
    match def.field_type {
        FieldType::Number => ", '')::numeric)",
        FieldType::Date => ", '')::date)",
        _ => ")",
    }
}

/// Build the filtered, authorized query. `limit` = None means no paging.
fn query(
    core: &Core,
    ctx: &Ctx,
    f: &DocumentFilter,
    perm: Permission,
    defs: &[FieldDef],
    limit: Option<(i64, i64)>,
) -> CoreResult<QueryBuilder<Postgres>> {
    let mut qb = QueryBuilder::new(
        "SELECT d.id, d.external_id, d.title, t.name AS type_name, dep.name AS department_name,
                o.display_name AS owner_name, d.status, d.execution_date, d.effective_date,
                d.last_review_date, d.next_review_date, d.renewal_date, d.expiration_date,
                coalesce(d.review_interval_months, t.review_interval_months) AS review_interval_months,
                d.visibility::text AS visibility, d.custom_fields, d.created_at, d.updated_at,
                COALESCE((SELECT json_agg(json_build_object('id', tg.id, 'name', tg.name, 'slug', tg.slug,
                                                            'color', tg.color) ORDER BY tg.name)
                            FROM document_tags dt JOIN tags tg ON tg.id = dt.tag_id
                           WHERE dt.document_id = d.id), '[]'::json) AS tags,
                count(*) OVER () AS total
           FROM documents d
           JOIN document_types t ON t.id = d.document_type_id
           JOIN departments dep ON dep.id = d.department_id
           JOIN users o ON o.id = d.owner_id
          WHERE d.deleted_at IS NULL AND ",
    );
    // Authorization first: system contexts see everything, everyone else
    // only what the policy engine allows.
    match ctx.principal() {
        Some(p) => {
            let frag = visible_documents_predicate(&core.policy, p, perm, Utc::now(), "d")
                .map_err(CoreError::internal)?;
            push_fragment(&mut qb, &frag);
        }
        None => {
            qb.push("TRUE");
        }
    }

    let q = f.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(q) = q {
        qb.push(" AND (d.search @@ websearch_to_tsquery('english', ")
            .push_bind(q.to_owned())
            .push(") OR d.external_id ILIKE ")
            .push_bind(format!("{}%", directory::escape_like(q)))
            .push(")");
    }
    if let Some(v) = f.document_type_id {
        qb.push(" AND d.document_type_id = ").push_bind(v);
    }
    if let Some(v) = f.department_id {
        qb.push(" AND d.department_id = ").push_bind(v);
    }
    if let Some(v) = f.owner_id {
        qb.push(" AND d.owner_id = ").push_bind(v);
    }
    match f.status {
        Some(s) => {
            qb.push(" AND d.status = ").push_bind(s);
        }
        None if !f.include_archived => {
            qb.push(" AND d.status <> 'archived'");
        }
        None => {}
    }
    if !f.tags.is_empty() {
        if f.all_tags {
            let mut tags = f.tags.clone();
            tags.sort();
            tags.dedup();
            qb.push(" AND (SELECT count(DISTINCT dt.tag_id) FROM document_tags dt WHERE dt.document_id = d.id AND dt.tag_id = ANY(")
                .push_bind(tags.clone())
                .push(")) = ")
                .push_bind(i64::try_from(tags.len()).unwrap_or(i64::MAX));
        } else {
            qb.push(" AND EXISTS (SELECT 1 FROM document_tags dt WHERE dt.document_id = d.id AND dt.tag_id = ANY(")
                .push_bind(f.tags.clone())
                .push("))");
        }
    }
    if let Some(df) = f.date_field {
        if let Some(from) = f.date_from {
            qb.push(format!(" AND d.{} >= ", df.column()))
                .push_bind(from);
        }
        if let Some(to) = f.date_to {
            qb.push(format!(" AND d.{} <= ", df.column())).push_bind(to);
        }
        if f.date_from.is_none() && f.date_to.is_none() {
            qb.push(format!(" AND d.{} IS NOT NULL", df.column()));
        }
    }
    if let Some(c) = &f.custom {
        let def = defs
            .iter()
            .find(|d| d.key == c.key)
            .ok_or_else(|| CoreError::validation("unknown custom field"))?;
        let field = |qb: &mut QueryBuilder<Postgres>| {
            qb.push(custom_expr(def))
                .push_bind(def.key.clone())
                .push(custom_cast(def));
        };
        match def.field_type {
            FieldType::Number | FieldType::Date => {
                let parse_bound = |s: &Option<String>| -> CoreResult<Option<String>> {
                    let Some(s) = s.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                        return Ok(None);
                    };
                    fields::parse_value(def, &[s.to_owned()])
                        .map_err(|e| CoreError::validation(format!("{} filter {e}", def.label)))
                        .map(|v| v.and_then(|v| v.as_str().map(str::to_owned)))
                };
                let (min, max) = (parse_bound(&c.min)?, parse_bound(&c.max)?);
                let cast = if def.field_type == FieldType::Number {
                    "::numeric"
                } else {
                    "::date"
                };
                if let Some(min) = min {
                    qb.push(" AND ");
                    field(&mut qb);
                    qb.push(" >= ").push_bind(min).push(cast);
                }
                if let Some(max) = max {
                    qb.push(" AND ");
                    field(&mut qb);
                    qb.push(" <= ").push_bind(max).push(cast);
                }
            }
            FieldType::Text | FieldType::LongText | FieldType::Url => {
                if let Some(v) = c.value.as_deref().filter(|v| !v.trim().is_empty()) {
                    qb.push(" AND ");
                    field(&mut qb);
                    qb.push(" ILIKE ")
                        .push_bind(format!("%{}%", directory::escape_like(v.trim())));
                }
            }
            FieldType::MultiSelect => {
                if let Some(v) = c.value.as_deref().filter(|v| !v.is_empty()) {
                    qb.push(" AND d.custom_fields -> ")
                        .push_bind(def.key.clone())
                        .push(" @> jsonb_build_array(")
                        .push_bind(v.to_owned())
                        .push("::text)");
                }
            }
            FieldType::Boolean => {
                if let Some(v) = c.value.as_deref().filter(|v| !v.is_empty()) {
                    qb.push(" AND coalesce((d.custom_fields->>")
                        .push_bind(def.key.clone())
                        .push(")::boolean, false) = ")
                        .push_bind(v == "true");
                }
            }
            FieldType::SingleSelect | FieldType::User | FieldType::Department => {
                if let Some(v) = c.value.as_deref().filter(|v| !v.is_empty()) {
                    qb.push(" AND ");
                    field(&mut qb);
                    qb.push(" = ").push_bind(v.to_owned());
                }
            }
        }
    }

    let dir = if f.descending { " DESC" } else { " ASC" };
    qb.push(" ORDER BY ");
    match &f.sort {
        SortField::Relevance if q.is_some() => {
            qb.push("ts_rank(d.search, websearch_to_tsquery('english', ")
                .push_bind(q.unwrap_or_default().to_owned())
                .push(")) DESC");
        }
        SortField::Relevance | SortField::Updated => {
            qb.push("d.updated_at").push(dir);
        }
        SortField::Created => {
            qb.push("d.created_at").push(dir);
        }
        SortField::Title => {
            qb.push("lower(d.title)").push(dir);
        }
        SortField::ExternalId => {
            qb.push("d.external_id").push(dir);
        }
        SortField::Status => {
            qb.push("d.status").push(dir);
        }
        SortField::Date(df) => {
            qb.push(format!("d.{}", df.column()))
                .push(dir)
                .push(" NULLS LAST");
        }
        SortField::Custom(k) => match defs.iter().find(|d| &d.key == k) {
            Some(def) => {
                qb.push(custom_expr(def))
                    .push_bind(def.key.clone())
                    .push(custom_cast(def))
                    .push(dir)
                    .push(" NULLS LAST");
            }
            None => {
                qb.push("d.updated_at DESC");
            }
        },
    }
    qb.push(", d.id");
    if let Some((limit, offset)) = limit {
        qb.push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
    }
    Ok(qb)
}

/// Every matching document the caller holds `perm` on (up to `limit`), unpaged.
pub async fn all(
    core: &Core,
    ctx: &Ctx,
    f: &DocumentFilter,
    perm: Permission,
    limit: i64,
) -> CoreResult<Vec<DocumentSummary>> {
    let defs = fields::list(&core.db).await?;
    let mut qb = query(core, ctx, f, perm, &defs, Some((limit, 0)))?;
    Ok(qb.build_query_as().fetch_all(&core.db).await?)
}

/// Documents the caller can see (`view`), filtered and sorted.
pub async fn search(core: &Core, ctx: &Ctx, f: &DocumentFilter) -> CoreResult<SearchResults> {
    let defs = fields::list(&core.db).await?;
    let per_page = f.per_page.clamp(1, 200);
    let page = f.page.max(1);
    let mut qb = query(
        core,
        ctx,
        f,
        Permission::View,
        &defs,
        Some((per_page, (page - 1) * per_page)),
    )?;
    let items: Vec<DocumentSummary> = qb.build_query_as().fetch_all(&core.db).await?;
    Ok(SearchResults {
        total: items.first().map_or(0, |i| i.total),
        items,
        page,
        per_page,
    })
}

pub const MAX_EXPORT: i64 = 10_000;

/// CSV of matching documents the caller may `export` (Restricted documents
/// appear only with an explicit document grant). Audited with the filter
/// and the exported IDs.
pub async fn export_csv(core: &Core, ctx: &Ctx, f: &DocumentFilter) -> CoreResult<Vec<u8>> {
    let defs = fields::list(&core.db).await?;
    let mut qb = query(
        core,
        ctx,
        f,
        Permission::Export,
        &defs,
        Some((MAX_EXPORT, 0)),
    )?;
    let items: Vec<DocumentSummary> = qb.build_query_as().fetch_all(&core.db).await?;

    let names: HashMap<String, String> = sqlx::query!(
        r#"SELECT id::text AS "id!", display_name AS "name!" FROM users
           UNION ALL SELECT id::text, name FROM departments"#
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r.name))
    .collect();
    let lookup = |s: &str| names.get(s).cloned();

    let mut header: Vec<String> = [
        "ID",
        "Title",
        "Type",
        "Department",
        "Owner",
        "Status",
        "Execution date",
        "Effective date",
        "Last review",
        "Next review",
        "Renewal date",
        "Expiration date",
        "Tags",
        "Visibility",
        "Created",
        "Updated",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for d in &defs {
        let mut h = format!("{} [{}]", d.label, d.key);
        if d.is_deprecated {
            h.push_str(" (deprecated)");
        }
        header.push(h);
    }
    let mut out = String::new();
    let mut row = |cells: &[String]| {
        for (i, c) in cells.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            crate::audit_log::csv_field(&mut out, c);
        }
        out.push_str("\r\n");
    };
    row(&header);
    let date = |d: Option<NaiveDate>| d.map(|d| d.to_string()).unwrap_or_default();
    for it in &items {
        let mut cells = vec![
            it.external_id.clone(),
            it.title.clone(),
            it.type_name.clone(),
            it.department_name.clone(),
            it.owner_name.clone(),
            it.status.label().to_owned(),
            date(it.execution_date),
            date(it.effective_date),
            date(it.last_review_date),
            date(it.next_review_date),
            date(it.renewal_date),
            date(it.expiration_date),
            it.tags
                .0
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            it.visibility.label().to_owned(),
            it.created_at.to_rfc3339(),
            it.updated_at.to_rfc3339(),
        ];
        for d in &defs {
            cells.push(
                it.custom_fields
                    .get(&d.key)
                    .map(|v| fields::display(Some(d), v, &lookup))
                    .unwrap_or_default(),
            );
        }
        row(&cells);
    }

    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Access,
                "document.list_exported",
                ctx.actor.clone(),
                "document",
            )
            .context("filter", f.describe())
            .context("rows", i64::try_from(items.len()).unwrap_or(i64::MAX))
            .context(
                "external_ids",
                items
                    .iter()
                    .take(1000)
                    .map(|i| i.external_id.clone())
                    .collect::<Vec<_>>(),
            ),
        ],
    )
    .await?;
    Ok(out.into_bytes())
}
