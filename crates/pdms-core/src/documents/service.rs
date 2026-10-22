// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document operations. Each one authorizes through [`super::access`], changes
//! data, appends a version where content or descriptive metadata changed, and
//! writes audit entries in the same transaction.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::{Capability, Permission, PermissionSet, Scope, Subject, Visibility};
use pdms_storage::{BlobHash, ByteStream};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::{self, DocRef};
use super::fields::{self, FieldDef, FieldType};
use super::files::{self, StoredFile, Upload};
use super::filestore;
use super::retention::{self, Eligibility};
use super::status::{self, DocumentStatus};
use super::tags::{self, TagRef};
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::model::UserStatus;
use crate::sharepoint::{
    self,
    layout::{Spot, narrows},
};
use crate::{Core, acl};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "version_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum VersionKind {
    Initial,
    Revision,
    Amendment,
    SignedCopy,
    Metadata,
    Restore,
}

impl VersionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionKind::Initial => "initial",
            VersionKind::Revision => "revision",
            VersionKind::Amendment => "amendment",
            VersionKind::SignedCopy => "signed_copy",
            VersionKind::Metadata => "metadata",
            VersionKind::Restore => "restore",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VersionKind::Initial => "Original file",
            VersionKind::Revision => "Revision",
            VersionKind::Amendment => "Amendment",
            VersionKind::SignedCopy => "Signed copy",
            VersionKind::Metadata => "Details changed",
            VersionKind::Restore => "Restored",
        }
    }

    /// Kinds a person chooses when uploading a new file.
    pub fn parse_upload(s: &str) -> Option<Self> {
        match s {
            "revision" => Some(VersionKind::Revision),
            "amendment" => Some(VersionKind::Amendment),
            "signed_copy" => Some(VersionKind::SignedCopy),
            _ => None,
        }
    }
}

/// A document with display labels.
#[derive(Clone, Debug, Serialize)]
pub struct Document {
    pub id: Uuid,
    pub external_id: String,
    pub document_type_id: Uuid,
    pub type_name: String,
    pub type_review_interval_months: Option<i32>,
    pub department_id: Uuid,
    pub department_name: String,
    pub owner_id: Uuid,
    pub owner_name: String,
    pub title: String,
    pub description: String,
    pub status: DocumentStatus,
    pub execution_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub last_review_date: Option<NaiveDate>,
    pub next_review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub review_interval_months: Option<i32>,
    pub custom_fields: Value,
    pub visibility: Visibility,
    pub legal_hold: bool,
    pub retention_policy_id: Option<Uuid>,
    pub retention_policy_name: Option<String>,
    pub current_version: i32,
    pub archived_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub row_version: i32,
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<TagRef>,
}

/// Descriptive metadata a person edits. Owner, status, access settings, and
/// retention have their own operations with their own permissions.
#[derive(Clone, Debug, Default)]
pub struct MetadataInput {
    pub title: String,
    pub description: String,
    pub document_type_id: Uuid,
    pub department_id: Uuid,
    pub execution_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub last_review_date: Option<NaiveDate>,
    pub next_review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub review_interval_months: Option<i32>,
    /// Raw form values by field key.
    pub custom: BTreeMap<String, Vec<String>>,
}

/// Validated metadata ready to store.
#[derive(Clone, Debug, PartialEq)]
struct Meta {
    title: String,
    description: String,
    document_type_id: Uuid,
    department_id: Uuid,
    execution_date: Option<NaiveDate>,
    effective_date: Option<NaiveDate>,
    last_review_date: Option<NaiveDate>,
    next_review_date: Option<NaiveDate>,
    renewal_date: Option<NaiveDate>,
    expiration_date: Option<NaiveDate>,
    review_interval_months: Option<i32>,
    custom_fields: Value,
}

impl From<&Document> for Meta {
    fn from(d: &Document) -> Self {
        Meta {
            title: d.title.clone(),
            description: d.description.clone(),
            document_type_id: d.document_type_id,
            department_id: d.department_id,
            execution_date: d.execution_date,
            effective_date: d.effective_date,
            last_review_date: d.last_review_date,
            next_review_date: d.next_review_date,
            renewal_date: d.renewal_date,
            expiration_date: d.expiration_date,
            review_interval_months: d.review_interval_months,
            custom_fields: d.custom_fields.clone(),
        }
    }
}

const DATE_FIELDS: [&str; 6] = [
    "execution_date",
    "effective_date",
    "last_review_date",
    "next_review_date",
    "renewal_date",
    "expiration_date",
];

impl Meta {
    fn dates(&self) -> [Option<NaiveDate>; 6] {
        [
            self.execution_date,
            self.effective_date,
            self.last_review_date,
            self.next_review_date,
            self.renewal_date,
            self.expiration_date,
        ]
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

pub(crate) async fn fetch(conn: &mut PgConnection, id: Uuid) -> CoreResult<Option<Document>> {
    let row = sqlx::query!(
        r#"SELECT d.id, d.external_id, d.document_type_id, t.name AS type_name,
                  t.review_interval_months AS type_review_interval_months,
                  d.department_id, dep.name AS department_name, d.owner_id, o.display_name AS owner_name,
                  d.title, d.description, d.status AS "status: DocumentStatus",
                  d.execution_date, d.effective_date, d.last_review_date, d.next_review_date,
                  d.renewal_date, d.expiration_date, d.review_interval_months, d.custom_fields,
                  d.visibility::text AS "visibility!", d.legal_hold, d.retention_policy_id,
                  coalesce(dr.name, tr.name) AS "retention_policy_name?",
                  d.current_version, d.archived_at, d.deleted_at, d.row_version,
                  c.display_name AS "created_by_name?", d.created_at, d.updated_at
             FROM documents d
             JOIN document_types t ON t.id = d.document_type_id
             JOIN departments dep ON dep.id = d.department_id
             JOIN users o ON o.id = d.owner_id
             LEFT JOIN users c ON c.id = d.created_by
             LEFT JOIN retention_policies dr ON dr.id = d.retention_policy_id
             LEFT JOIN retention_policies tr ON tr.id = t.retention_policy_id
            WHERE d.id = $1"#,
        id
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let tags = sqlx::query_as!(
        TagRef,
        "SELECT t.id, t.name, t.slug, t.color FROM document_tags dt JOIN tags t ON t.id = dt.tag_id
          WHERE dt.document_id = $1 ORDER BY t.name",
        id
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some(Document {
        id: r.id,
        external_id: r.external_id,
        document_type_id: r.document_type_id,
        type_name: r.type_name,
        type_review_interval_months: r.type_review_interval_months,
        department_id: r.department_id,
        department_name: r.department_name,
        owner_id: r.owner_id,
        owner_name: r.owner_name,
        title: r.title,
        description: r.description,
        status: r.status,
        execution_date: r.execution_date,
        effective_date: r.effective_date,
        last_review_date: r.last_review_date,
        next_review_date: r.next_review_date,
        renewal_date: r.renewal_date,
        expiration_date: r.expiration_date,
        review_interval_months: r.review_interval_months,
        custom_fields: r.custom_fields,
        visibility: r.visibility.parse().map_err(CoreError::internal)?,
        legal_hold: r.legal_hold,
        retention_policy_id: r.retention_policy_id,
        retention_policy_name: r.retention_policy_name,
        current_version: r.current_version,
        archived_at: r.archived_at,
        deleted_at: r.deleted_at,
        row_version: r.row_version,
        created_by_name: r.created_by_name,
        created_at: r.created_at,
        updated_at: r.updated_at,
        tags,
    }))
}

async fn fetch_live(conn: &mut PgConnection, id: Uuid) -> CoreResult<Document> {
    fetch(conn, id)
        .await?
        .filter(|d| d.deleted_at.is_none())
        .ok_or(CoreError::NotFound)
}

/// The ID of the live document with this external ID (e.g. `ILA-2026-000001`).
/// Finding a document says nothing about access: callers authorize next.
pub async fn id_by_external_id(core: &Core, external_id: &str) -> CoreResult<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM documents WHERE external_id = $1 AND deleted_at IS NULL",
        external_id
    )
    .fetch_optional(&core.db)
    .await?)
}

/// A document, its permissions for the caller, and its current file. Audited as a view.
#[derive(Clone, Debug)]
pub struct DocumentView {
    pub document: Document,
    pub permissions: PermissionSet,
    pub current_file: Option<VersionInfo>,
    pub fields: Vec<FieldDef>,
}

pub async fn view(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<DocumentView> {
    let doc_ref = access::authorize(core, ctx, id, Permission::View).await?;
    let permissions = access::permissions(core, ctx, &doc_ref).await?;
    let mut conn = core.db.acquire().await?;
    let document = fetch_live(&mut conn, id).await?;
    drop(conn);
    let versions = list_versions(core, id).await?;
    let current_file = versions.into_iter().find(|v| v.blob_sha256.is_some());
    pdms_audit::record(
        &core.db,
        vec![doc_ref.event(ctx, Category::Access, "document.viewed")],
    )
    .await?;
    Ok(DocumentView {
        document,
        permissions,
        current_file,
        fields: fields::list(&core.db).await?,
    })
}

/// Load a document for an edit form (requires `edit`; not audited as a view).
pub async fn for_edit(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<(Document, Vec<FieldDef>)> {
    access::authorize(core, ctx, id, Permission::Edit).await?;
    let mut conn = core.db.acquire().await?;
    let d = fetch_live(&mut conn, id).await?;
    Ok((d, fields::list(&core.db).await?))
}

#[derive(Clone, Debug, Serialize)]
pub struct VersionInfo {
    pub version_no: i32,
    pub kind: VersionKind,
    pub file_changed: bool,
    pub blob_sha256: Option<String>,
    pub file_name: Option<String>,
    pub media_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub note: String,
    pub milestone_date: Option<NaiveDate>,
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub metadata: Value,
}

async fn list_versions(core: &Core, id: Uuid) -> CoreResult<Vec<VersionInfo>> {
    let rows = sqlx::query!(
        r#"SELECT v.version_no, v.kind AS "kind: VersionKind", v.file_changed, v.blob_sha256, v.file_name,
                  b.media_type AS "media_type?", b.size_bytes AS "size_bytes?", v.note, v.milestone_date,
                  u.display_name AS "created_by_name?", v.created_at, v.metadata
             FROM document_versions v
             LEFT JOIN blobs b ON b.sha256 = v.blob_sha256
             LEFT JOIN users u ON u.id = v.created_by
            WHERE v.document_id = $1 ORDER BY v.version_no DESC"#,
        id
    )
    .fetch_all(&core.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| VersionInfo {
            version_no: r.version_no,
            kind: r.kind,
            file_changed: r.file_changed,
            blob_sha256: r.blob_sha256.map(hex::encode),
            file_name: r.file_name,
            media_type: r.media_type,
            size_bytes: r.size_bytes,
            note: r.note,
            milestone_date: r.milestone_date,
            created_by_name: r.created_by_name,
            created_at: r.created_at,
            metadata: r.metadata,
        })
        .collect())
}

/// Version history (requires `view`).
pub async fn versions(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<Vec<VersionInfo>> {
    access::authorize(core, ctx, id, Permission::View).await?;
    list_versions(core, id).await
}

// ---------------------------------------------------------------------------
// Validation and snapshots
// ---------------------------------------------------------------------------

async fn validate(
    core: &Core,
    input: &MetadataInput,
    existing_custom: &Value,
    require_active_type: bool,
) -> CoreResult<(Meta, Vec<FieldDef>)> {
    let title: String = input.title.trim().to_owned();
    if title.is_empty() || title.chars().count() > 500 {
        return Err(CoreError::validation("title must be 1–500 characters"));
    }
    let description: String = input.description.trim().to_owned();
    if description.chars().count() > 20_000 {
        return Err(CoreError::validation("description is too long"));
    }
    let t = sqlx::query!(
        "SELECT is_active, review_interval_months FROM document_types WHERE id = $1",
        input.document_type_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| CoreError::validation("choose a document type"))?;
    if require_active_type && !t.is_active {
        return Err(CoreError::validation("that document type is retired"));
    }
    let active = sqlx::query_scalar!(
        "SELECT is_active FROM departments WHERE id = $1",
        input.department_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| CoreError::validation("choose a department"))?;
    if !active {
        return Err(CoreError::validation("that department is inactive"));
    }
    if input
        .review_interval_months
        .is_some_and(|m| !(1..=600).contains(&m))
    {
        return Err(CoreError::validation(
            "review interval must be 1–600 months",
        ));
    }
    if let (Some(e), Some(x)) = (input.effective_date, input.expiration_date)
        && x < e
    {
        return Err(CoreError::validation(
            "the expiration date is before the effective date",
        ));
    }
    // Leave "next review" empty to have it computed from the last review (or
    // the effective/execution date) plus the review interval.
    let interval = input.review_interval_months.or(t.review_interval_months);
    let next_review_date = input.next_review_date.or_else(|| {
        let base = input
            .last_review_date
            .or(input.effective_date)
            .or(input.execution_date)?;
        interval.map(|m| crate::schedule::next_review(base, m))
    });
    let defs = fields::list(&core.db).await?;
    let custom_fields = fields::merge_values(
        &defs,
        input.document_type_id,
        &input.custom,
        existing_custom,
    )?;
    check_references(core, &defs, &custom_fields).await?;
    Ok((
        Meta {
            title,
            description,
            document_type_id: input.document_type_id,
            department_id: input.department_id,
            execution_date: input.execution_date,
            effective_date: input.effective_date,
            last_review_date: input.last_review_date,
            next_review_date,
            renewal_date: input.renewal_date,
            expiration_date: input.expiration_date,
            review_interval_months: input.review_interval_months,
            custom_fields,
        },
        defs,
    ))
}

/// User and department custom values must point at existing records.
async fn check_references(core: &Core, defs: &[FieldDef], values: &Value) -> CoreResult<()> {
    let (mut users, mut depts) = (Vec::new(), Vec::new());
    for d in defs {
        if let Some(Value::String(s)) = values.get(&d.key)
            && let Ok(id) = s.parse::<Uuid>()
        {
            match d.field_type {
                FieldType::User => users.push(id),
                FieldType::Department => depts.push(id),
                _ => {}
            }
        }
    }
    let found_users = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM users WHERE id = ANY($1)"#,
        &users
    )
    .fetch_one(&core.db)
    .await?;
    let found_depts = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM departments WHERE id = ANY($1)"#,
        &depts
    )
    .fetch_one(&core.db)
    .await?;
    let distinct = |v: &mut Vec<Uuid>| {
        v.sort();
        v.dedup();
        v.len() as i64
    };
    if found_users != distinct(&mut users) || found_depts != distinct(&mut depts) {
        return Err(CoreError::validation(
            "a selected user or department no longer exists",
        ));
    }
    Ok(())
}

fn search_extra(defs: &[FieldDef], values: &Value) -> String {
    defs.iter()
        .filter_map(|d| values.get(&d.key).and_then(|v| fields::search_text(d, v)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn field_versions(defs: &[FieldDef], values: &Value) -> Value {
    Value::Object(
        defs.iter()
            .filter(|d| values.get(&d.key).is_some())
            .map(|d| (d.key.clone(), json!(d.version)))
            .collect(),
    )
}

fn snapshot(d: &Document) -> Value {
    json!({
        "title": d.title,
        "description": d.description,
        "document_type_id": d.document_type_id,
        "department_id": d.department_id,
        "owner_id": d.owner_id,
        "status": d.status.as_str(),
        "execution_date": d.execution_date,
        "effective_date": d.effective_date,
        "last_review_date": d.last_review_date,
        "next_review_date": d.next_review_date,
        "renewal_date": d.renewal_date,
        "expiration_date": d.expiration_date,
        "review_interval_months": d.review_interval_months,
        "custom_fields": d.custom_fields,
        "tags": d.tags.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        "visibility": d.visibility.as_str(),
    })
}

async fn refresh_search(conn: &mut PgConnection, id: Uuid) -> CoreResult<()> {
    sqlx::query("SELECT refresh_document_search($1)")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

struct NewVersion<'a> {
    kind: VersionKind,
    file: Option<(&'a StoredFile, &'a str)>,
    note: &'a str,
    milestone_date: Option<NaiveDate>,
}

/// Append a version. Without a new file, the current file carries over.
async fn append_version(
    conn: &mut PgConnection,
    ctx: &Ctx,
    doc_id: Uuid,
    v: NewVersion<'_>,
    defs: &[FieldDef],
) -> CoreResult<i32> {
    let version_no = sqlx::query_scalar!(
        "UPDATE documents SET current_version = current_version + 1, updated_at = now()
          WHERE id = $1 RETURNING current_version",
        doc_id
    )
    .fetch_one(&mut *conn)
    .await?;
    let (blob, file_name, file_changed): (Option<Vec<u8>>, Option<String>, bool) = match v.file {
        Some((f, name)) => (Some(f.sha256.0.to_vec()), Some(name.to_owned()), true),
        None => {
            let prev = sqlx::query!(
                "SELECT blob_sha256, file_name FROM document_versions
                  WHERE document_id = $1 ORDER BY version_no DESC LIMIT 1",
                doc_id
            )
            .fetch_optional(&mut *conn)
            .await?;
            prev.map_or((None, None, false), |p| (p.blob_sha256, p.file_name, false))
        }
    };
    let doc = fetch_live(conn, doc_id).await?;
    sqlx::query!(
        "INSERT INTO document_versions (id, document_id, version_no, kind, file_changed, blob_sha256,
                                        file_name, metadata, field_versions, note, milestone_date, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        Uuid::now_v7(),
        doc_id,
        version_no,
        v.kind as VersionKind,
        file_changed,
        blob,
        file_name,
        snapshot(&doc),
        field_versions(defs, &doc.custom_fields),
        v.note.trim().chars().take(1000).collect::<String>(),
        v.milestone_date,
        ctx.user_id(),
    )
    .execute(&mut *conn)
    .await?;
    if file_changed {
        // Read the new file's text in the background (skipped when disabled).
        crate::ocr::enqueue(conn, doc_id, version_no, false).await?;
    }
    Ok(version_no)
}

/// Field-level differences, with names instead of IDs where it helps readers.
fn metadata_changes(
    mut e: AuditEvent,
    before: Option<&Meta>,
    after: &Meta,
    names: &BTreeMap<Uuid, String>,
    defs: &[FieldDef],
) -> AuditEvent {
    let name = |id: Uuid| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    e = e
        .change(
            "title",
            before.map(|b| b.title.clone()),
            after.title.clone(),
        )
        .change(
            "description",
            before.map(|b| b.description.clone()),
            after.description.clone(),
        )
        .change(
            "document_type",
            before.map(|b| name(b.document_type_id)),
            name(after.document_type_id),
        )
        .change(
            "department",
            before.map(|b| name(b.department_id)),
            name(after.department_id),
        );
    let before_dates = before.map(Meta::dates).unwrap_or_default();
    for ((field, b), a) in DATE_FIELDS.iter().zip(before_dates).zip(after.dates()) {
        e = e.change(field, b, a);
    }
    e = e.change(
        "review_interval_months",
        before.and_then(|b| b.review_interval_months),
        after.review_interval_months,
    );
    let empty = Value::Object(Default::default());
    let b_cf = before.map_or(&empty, |b| &b.custom_fields);
    let mut keys: Vec<&String> = b_cf
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k)
        .collect();
    keys.extend(
        after
            .custom_fields
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, _)| k),
    );
    keys.sort();
    keys.dedup();
    for k in keys {
        let label = defs
            .iter()
            .find(|d| &d.key == k)
            .map_or(k.as_str(), |d| d.label.as_str());
        let field = format!("custom.{k}");
        let (bv, av) = (
            fields::audit_value(b_cf.get(k)),
            fields::audit_value(after.custom_fields.get(k)),
        );
        if bv != av {
            e = e
                .change(&field, bv, av)
                .context(&format!("label.{field}"), label);
        }
    }
    e
}

async fn names_for(core: &Core, ids: &[Uuid]) -> CoreResult<BTreeMap<Uuid, String>> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!", name AS "name!" FROM (
             SELECT id, name FROM document_types UNION ALL SELECT id, name FROM departments
             UNION ALL SELECT id, display_name FROM users) x WHERE id = ANY($1)"#,
        ids
    )
    .fetch_all(&core.db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.name)).collect())
}

fn file_context(e: AuditEvent, f: &StoredFile) -> AuditEvent {
    e.context("file_name", f.file_name.clone())
        .context("sha256", f.sha256.hex())
        .context("size_bytes", i64::try_from(f.size).unwrap_or(i64::MAX))
        .context("media_type", f.media_type)
        .context("virus_scan", f.scan)
}

// ---------------------------------------------------------------------------
// Create and edit
// ---------------------------------------------------------------------------

pub struct NewDocument {
    pub metadata: MetadataInput,
    pub owner_id: Uuid,
    /// `Draft` or `Active`.
    pub status: DocumentStatus,
    /// Who can see it (D-085); chosen at upload.
    pub visibility: Visibility,
    pub tags: Vec<String>,
    pub file: Option<Upload>,
    /// Kind of the first file (normally `Initial`).
    pub note: String,
}

async fn require_active_user(core: &Core, id: Uuid) -> CoreResult<String> {
    let u = sqlx::query!(
        r#"SELECT display_name, status AS "status: UserStatus" FROM users WHERE id = $1"#,
        id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| CoreError::validation("choose an owner"))?;
    if u.status != UserStatus::Active {
        return Err(CoreError::validation("the owner must be an active user"));
    }
    Ok(u.display_name)
}

pub async fn create(core: &Core, ctx: &Ctx, new: NewDocument) -> CoreResult<Uuid> {
    access::authorize_create(core, ctx, new.metadata.department_id).await?;
    if !matches!(new.status, DocumentStatus::Draft | DocumentStatus::Active) {
        return Err(CoreError::validation(
            "new documents start as Draft or Active",
        ));
    }
    let (meta, defs) = validate(
        core,
        &new.metadata,
        &Value::Object(Default::default()),
        true,
    )
    .await?;
    require_active_user(core, new.owner_id).await?;
    let stored = match &new.file {
        Some(u) => Some(files::ingest(core, ctx, None, u).await?),
        None => None,
    };

    let id = Uuid::now_v7();
    // The document ID is allocated before the file is stored, so the file can
    // be filed under it (spec §7.1). A number left unused by a failure is harmless.
    let prefix = sqlx::query_scalar!(
        "SELECT id_prefix FROM document_types WHERE id = $1",
        meta.document_type_id
    )
    .fetch_one(&core.db)
    .await?;
    let seq = sqlx::query_scalar!(r#"SELECT nextval('document_external_seq') AS "n!""#)
        .fetch_one(&core.db)
        .await?;
    let year = Utc::now()
        .with_timezone(&core.config.timezone())
        .format("%Y");
    let external_id = format!("{prefix}-{year}-{seq:06}");
    let location = match (&new.file, &stored) {
        (Some(u), Some(f)) => Some(
            filestore::put_version_file(
                core,
                &filestore::PutTarget {
                    document_id: None,
                    external_id: &external_id,
                    title: &meta.title,
                    visibility: new.visibility,
                    department_id: meta.department_id,
                },
                f,
                &u.file.path,
            )
            .await?,
        ),
        _ => None,
    };
    let names = names_for(
        core,
        &[meta.document_type_id, meta.department_id, new.owner_id],
    )
    .await?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO documents (id, external_id, document_type_id, department_id, owner_id, title, description,
                                status, execution_date, effective_date, last_review_date, next_review_date,
                                renewal_date, expiration_date, review_interval_months, custom_fields,
                                search_extra, created_by, visibility, archived_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19::text::document_visibility, NULL)",
        id,
        external_id,
        meta.document_type_id,
        meta.department_id,
        new.owner_id,
        meta.title,
        meta.description,
        new.status as DocumentStatus,
        meta.execution_date,
        meta.effective_date,
        meta.last_review_date,
        meta.next_review_date,
        meta.renewal_date,
        meta.expiration_date,
        meta.review_interval_months,
        meta.custom_fields,
        search_extra(&defs, &meta.custom_fields),
        ctx.user_id(),
        new.visibility.as_str(),
    )
    .execute(&mut *tx)
    .await?;

    let mut events = Vec::new();
    for tag in &new.tags {
        if tag.trim().is_empty() {
            continue;
        }
        let allow = core.config.documents.free_form_tags || ctx.can(Capability::ManageTags);
        let (t, created) = tags::find_or_create(&mut tx, ctx, tag, allow).await?;
        events.extend(created);
        sqlx::query!(
            "INSERT INTO document_tags (document_id, tag_id, added_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            id,
            t.id,
            ctx.user_id()
        )
        .execute(&mut *tx)
        .await?;
    }
    if let Some(f) = &stored {
        files::record_blob(&mut tx, f, filestore::backend_name(core)).await?;
    }
    let kind = if stored.is_some() {
        VersionKind::Initial
    } else {
        VersionKind::Metadata
    };
    let version = append_version(
        &mut tx,
        ctx,
        id,
        NewVersion {
            kind,
            file: stored.as_ref().map(|f| (f, f.file_name.as_str())),
            note: &new.note,
            milestone_date: None,
        },
        &defs,
    )
    .await?;
    if let Some(loc) = &location {
        filestore::record_location(&mut tx, id, version, loc).await?;
    }
    refresh_search(&mut tx, id).await?;

    let doc = fetch_live(&mut tx, id).await?;
    let r = DocRef {
        attrs: pdms_authz::DocumentAttrs {
            id,
            department_id: doc.department_id,
            owner_id: doc.owner_id,
            visibility: new.visibility,
        },
        external_id: doc.external_id.clone(),
        title: doc.title.clone(),
    };
    let e = r
        .event(ctx, Category::Content, "document.created")
        .change("owner", AuditValue::Null, names.get(&new.owner_id).cloned())
        .change("status", AuditValue::Null, new.status.as_str())
        .change("visibility", AuditValue::Null, new.visibility.as_str())
        .change(
            "tags",
            AuditValue::Null,
            doc.tags.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        )
        .context("external_id", external_id.as_str());
    events.push(metadata_changes(e, None, &meta, &names, &defs));
    if let Some(f) = &stored {
        events.push(file_context(
            r.event(ctx, Category::Content, "document.file_uploaded")
                .context("version", 1)
                .context("kind", "initial"),
            f,
        ));
    }
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(id)
}

/// Edit descriptive metadata. `row_version` must match the version the editor loaded.
pub async fn update(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    input: MetadataInput,
    row_version: i32,
) -> CoreResult<()> {
    let doc_ref = access::authorize(core, ctx, id, Permission::Edit).await?;
    let mut conn = core.db.acquire().await?;
    let before_doc = fetch_live(&mut conn, id).await?;
    drop(conn);
    if input.department_id != before_doc.department_id {
        access::authorize_create(core, ctx, input.department_id).await?;
    }
    let require_active_type = input.document_type_id != before_doc.document_type_id;
    let (meta, defs) =
        validate(core, &input, &before_doc.custom_fields, require_active_type).await?;
    let before = Meta::from(&before_doc);
    if meta == before {
        return Ok(());
    }
    let names = names_for(
        core,
        &[
            before.document_type_id,
            meta.document_type_id,
            before.department_id,
            meta.department_id,
        ],
    )
    .await?;

    if core.sharepoint.is_some() {
        let v = before_doc.visibility;
        let target = Spot::of(v, meta.department_id, false);
        if narrows(Spot::of(v, before_doc.department_id, false), target) {
            sharepoint::apply_placement_now(core, id, target, "department").await?;
        }
    }
    let mut tx = core.db.begin().await?;
    let updated = sqlx::query!(
        "UPDATE documents SET title = $3, description = $4, document_type_id = $5, department_id = $6,
                execution_date = $7, effective_date = $8, last_review_date = $9, next_review_date = $10,
                renewal_date = $11, expiration_date = $12, review_interval_months = $13,
                custom_fields = $14, search_extra = $15, row_version = row_version + 1, updated_at = now()
          WHERE id = $1 AND row_version = $2 AND deleted_at IS NULL",
        id,
        row_version,
        meta.title,
        meta.description,
        meta.document_type_id,
        meta.department_id,
        meta.execution_date,
        meta.effective_date,
        meta.last_review_date,
        meta.next_review_date,
        meta.renewal_date,
        meta.expiration_date,
        meta.review_interval_months,
        meta.custom_fields,
        search_extra(&defs, &meta.custom_fields),
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(CoreError::Conflict(
            "Someone else changed this document after you opened it. Reload the page and make your changes again."
                .into(),
        ));
    }
    let version = append_version(
        &mut tx,
        ctx,
        id,
        NewVersion {
            kind: VersionKind::Metadata,
            file: None,
            note: "",
            milestone_date: None,
        },
        &defs,
    )
    .await?;
    refresh_search(&mut tx, id).await?;
    let mut events = vec![metadata_changes(
        doc_ref
            .event(ctx, Category::Metadata, "document.metadata_updated")
            .context("version", version),
        Some(&before),
        &meta,
        &names,
        &defs,
    )];
    if meta.department_id != before.department_id {
        events.push(
            doc_ref
                .event(ctx, Category::Permission, "document.department_transferred")
                .department(Some(meta.department_id))
                .change(
                    "department",
                    names.get(&before.department_id).cloned(),
                    names.get(&meta.department_id).cloned(),
                )
                .context("from_department_id", before.department_id)
                .context("to_department_id", meta.department_id),
        );
    }
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Status, ownership, access settings, retention
// ---------------------------------------------------------------------------

pub async fn change_status(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    to: DocumentStatus,
    note: &str,
) -> CoreResult<()> {
    let mut conn = core.db.acquire().await?;
    let current = fetch_live(&mut conn, id).await?.status;
    drop(conn);
    let actor = if ctx.is_system() {
        status::Actor::System
    } else {
        status::Actor::Person
    };
    let perm = status::transition(current, to, actor).ok_or_else(|| {
        CoreError::validation(format!(
            "a document cannot move from {} to {}",
            current.label(),
            to.label()
        ))
    })?;
    let doc_ref = access::authorize(core, ctx, id, perm).await?;
    if current == DocumentStatus::InReview
        && crate::workflow::in_progress(&core.db, id).await?.is_some()
    {
        return Err(CoreError::validation(
            "this document is in an approval workflow; approve, reject, or cancel it there",
        ));
    }
    let mut tx = core.db.begin().await?;
    let e = set_status(&mut tx, ctx, &doc_ref, current, to, note).await?;
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

/// Move a document from `from` to `to` inside a transaction. The caller has
/// authorized the change; returns the audit entry to append.
pub(crate) async fn set_status(
    conn: &mut PgConnection,
    ctx: &Ctx,
    doc_ref: &access::DocRef,
    from: DocumentStatus,
    to: DocumentStatus,
    note: &str,
) -> CoreResult<AuditEvent> {
    let r = sqlx::query!(
        "UPDATE documents SET status = $3, updated_at = now(),
                archived_at = CASE WHEN $3 = 'archived'::document_status THEN now() ELSE NULL END
          WHERE id = $1 AND status = $2 AND deleted_at IS NULL",
        doc_ref.id(),
        from as DocumentStatus,
        to as DocumentStatus,
    )
    .execute(&mut *conn)
    .await?;
    if r.rows_affected() != 1 {
        return Err(CoreError::Conflict(
            "The status changed while you were working. Reload and try again.".into(),
        ));
    }
    let note: String = note.trim().chars().take(1000).collect();
    let mut e = doc_ref
        .event(ctx, Category::Workflow, "document.status_changed")
        .change("status", from.as_str(), to.as_str());
    if !note.is_empty() {
        e = e.context("note", note);
    }
    Ok(e)
}

/// Transfer primary ownership (requires `manage_permissions`).
pub async fn transfer_owner(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    new_owner: Uuid,
    reason: &str,
) -> CoreResult<()> {
    let doc_ref = access::authorize(core, ctx, id, Permission::ManagePermissions).await?;
    if doc_ref.attrs.owner_id == new_owner {
        return Ok(());
    }
    let new_name = require_active_user(core, new_owner).await?;
    let names = names_for(core, &[doc_ref.attrs.owner_id]).await?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE documents SET owner_id = $2, updated_at = now() WHERE id = $1",
        id,
        new_owner
    )
    .execute(&mut *tx)
    .await?;
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc_ref
                .event(ctx, Category::Permission, "document.owner_transferred")
                .change(
                    "owner",
                    names.get(&doc_ref.attrs.owner_id).cloned(),
                    new_name,
                )
                .context("from_user_id", doc_ref.attrs.owner_id)
                .context("to_user_id", new_owner)
                .context(
                    "reason",
                    reason.trim().chars().take(500).collect::<String>(),
                ),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Who can see the document without an explicit share (D-085). Allowed for
/// the document's owner or anyone with `manage_permissions` on it.
pub async fn set_visibility(core: &Core, ctx: &Ctx, id: Uuid, to: Visibility) -> CoreResult<()> {
    // The owner chooses visibility (D-085); otherwise `manage_permissions` is needed.
    let is_owner = match (access::load(core, id).await?, ctx.user_id()) {
        (Some(d), Some(user)) => d.attrs.owner_id == user,
        (None, _) => return Err(CoreError::NotFound),
        _ => false,
    };
    let doc_ref = if is_owner {
        access::authorize(core, ctx, id, Permission::View).await?
    } else {
        access::authorize(core, ctx, id, Permission::ManagePermissions).await?
    };
    let mut tx = core.db.begin().await?;
    // Read the current value under a row lock so concurrent changes record
    // the right "from" and never skip a real change.
    let current = sqlx::query_scalar!(
        r#"SELECT visibility::text AS "visibility!" FROM documents WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    let from: Visibility = current.parse().map_err(CoreError::internal)?;
    if from == to {
        return Ok(());
    }
    if core.sharepoint.is_some() {
        let dept = doc_ref.attrs.department_id;
        let target = Spot::of(to, dept, false);
        if narrows(Spot::of(from, dept, false), target) {
            sharepoint::apply_placement_now(core, id, target, "visibility").await?;
        }
    }
    sqlx::query!(
        "UPDATE documents SET visibility = $2::text::document_visibility, updated_at = now() WHERE id = $1",
        id,
        to.as_str()
    )
    .execute(&mut *tx)
    .await?;
    refresh_search(&mut tx, id).await?;
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc_ref
                .event(ctx, Category::Permission, "document.visibility_changed")
                .change("visibility", from.as_str(), to.as_str()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Legal hold and per-document retention policy (requires `ManageRetention`).
pub async fn set_retention(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    legal_hold: bool,
    retention_policy_id: Option<Uuid>,
    reason: &str,
) -> CoreResult<()> {
    ctx.require(Capability::ManageRetention)?;
    let mut conn = core.db.acquire().await?;
    let d = fetch_live(&mut conn, id).await?;
    drop(conn);
    if d.legal_hold == legal_hold && d.retention_policy_id == retention_policy_id {
        return Ok(());
    }
    let reason: String = reason.trim().chars().take(500).collect();
    if reason.is_empty() {
        return Err(CoreError::validation(
            "enter a reason for the retention change",
        ));
    }
    let doc_ref = access::load(core, id).await?.ok_or(CoreError::NotFound)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE documents SET legal_hold = $2, retention_policy_id = $3, updated_at = now() WHERE id = $1",
        id,
        legal_hold,
        retention_policy_id
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(x) if x.is_foreign_key_violation() => CoreError::validation("unknown retention policy"),
        _ => CoreError::Db(e),
    })?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc_ref
                .event(ctx, Category::Admin, "document.retention_changed")
                .change("legal_hold", d.legal_hold, legal_hold)
                .change(
                    "retention_policy_id",
                    d.retention_policy_id,
                    retention_policy_id,
                )
                .context("reason", reason),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Whether the document could be deleted today under its retention rule.
pub async fn deletion_eligibility(core: &Core, id: Uuid) -> CoreResult<Eligibility> {
    let r = sqlx::query!(
        r#"SELECT d.legal_hold, d.created_at, d.effective_date, d.expiration_date, d.archived_at,
                  p.name AS "policy_name?", p.retain_years, p.basis AS "basis?: retention::RetentionBasis"
             FROM documents d
             JOIN document_types t ON t.id = d.document_type_id
             LEFT JOIN retention_policies p ON p.id = coalesce(d.retention_policy_id, t.retention_policy_id)
            WHERE d.id = $1"#,
        id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let rule = match (r.policy_name, r.basis) {
        (Some(policy_name), Some(basis)) => Some(retention::Rule {
            policy_name,
            retain_years: r.retain_years,
            basis,
        }),
        _ => None,
    };
    let today = Utc::now()
        .with_timezone(&core.config.timezone())
        .date_naive();
    Ok(retention::eligibility(
        rule.as_ref(),
        r.legal_hold,
        &retention::Dates {
            created_at: Some(r.created_at),
            effective_date: r.effective_date,
            expiration_date: r.expiration_date,
            archived_at: r.archived_at,
        },
        today,
    ))
}

/// Soft-delete (requires `delete`; blocked by legal hold and retention). The
/// row stays as a tombstone and the audit entry records what was deleted.
pub async fn delete(core: &Core, ctx: &Ctx, id: Uuid, reason: &str) -> CoreResult<()> {
    let doc_ref = access::authorize(core, ctx, id, Permission::Delete).await?;
    let reason: String = reason.trim().chars().take(1000).collect();
    if reason.is_empty() {
        return Err(CoreError::validation(
            "enter a reason for deleting the document",
        ));
    }
    if let Some(why) = deletion_eligibility(core, id).await?.reason() {
        return Err(CoreError::Conflict(why));
    }
    if core.sharepoint.is_some() {
        let a = &doc_ref.attrs;
        let target = Spot::of(a.visibility, a.department_id, true);
        if narrows(Spot::of(a.visibility, a.department_id, false), target) {
            sharepoint::apply_placement_now(core, id, target, "deleted").await?;
        }
    }
    let mut tx = core.db.begin().await?;
    let d = fetch_live(&mut tx, id).await?;
    sqlx::query!(
        "UPDATE documents SET deleted_at = now(), deleted_by = $2, updated_at = now() WHERE id = $1",
        id,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc_ref
                .event(ctx, Category::Content, "document.deleted")
                .context("reason", reason)
                .context(
                    "tombstone",
                    AuditValue::from_json(&snapshot(&d)).unwrap_or(AuditValue::Null),
                )
                .context("external_id", d.external_id.clone())
                .context("current_version", d.current_version),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletedDocument {
    pub id: Uuid,
    pub external_id: String,
    pub title: String,
    pub department_name: String,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by: Option<String>,
}

pub async fn list_deleted(core: &Core, ctx: &Ctx) -> CoreResult<Vec<DeletedDocument>> {
    ctx.require(Capability::RestoreDeletedDocuments)?;
    Ok(sqlx::query_as!(
        DeletedDocument,
        r#"SELECT d.id, d.external_id, d.title, dep.name AS department_name, d.deleted_at,
                  u.display_name AS "deleted_by?"
             FROM documents d JOIN departments dep ON dep.id = d.department_id
             LEFT JOIN users u ON u.id = d.deleted_by
            WHERE d.deleted_at IS NOT NULL ORDER BY d.deleted_at DESC"#
    )
    .fetch_all(&core.db)
    .await?)
}

pub async fn restore_deleted(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::RestoreDeletedDocuments)?;
    let mut tx = core.db.begin().await?;
    let d = fetch(&mut tx, id).await?.ok_or(CoreError::NotFound)?;
    if d.deleted_at.is_none() {
        return Ok(());
    }
    sqlx::query!(
        "UPDATE documents SET deleted_at = NULL, deleted_by = NULL, updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Content,
                "document.undeleted",
                ctx.actor.clone(),
                "document",
            )
            .object(id, Some(&format!("{} {}", d.external_id, d.title)))
            .document(id)
            .department(Some(d.department_id))
            .change("deleted_at", d.deleted_at, AuditValue::Null),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

pub async fn add_tag(core: &Core, ctx: &Ctx, id: Uuid, name: &str) -> CoreResult<()> {
    let doc_ref = access::authorize(core, ctx, id, Permission::Edit).await?;
    let allow = core.config.documents.free_form_tags || ctx.can(Capability::ManageTags);
    let mut tx = core.db.begin().await?;
    let before = fetch_live(&mut tx, id).await?.tags;
    let (tag, created) = tags::find_or_create(&mut tx, ctx, name, allow).await?;
    if before.iter().any(|t| t.id == tag.id) {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO document_tags (document_id, tag_id, added_by) VALUES ($1, $2, $3)",
        id,
        tag.id,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    refresh_search(&mut tx, id).await?;
    let names = |ts: &[TagRef]| ts.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    let mut after = names(&before);
    after.push(tag.name.clone());
    after.sort();
    let mut events: Vec<AuditEvent> = created.into_iter().collect();
    events.push(
        doc_ref
            .event(ctx, Category::Metadata, "document.tag_added")
            .change("tags", names(&before), after)
            .context("tag", tag.name.clone()),
    );
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_tag(core: &Core, ctx: &Ctx, id: Uuid, tag_id: Uuid) -> CoreResult<()> {
    let doc_ref = access::authorize(core, ctx, id, Permission::Edit).await?;
    let mut tx = core.db.begin().await?;
    let before = fetch_live(&mut tx, id).await?.tags;
    let Some(tag) = before.iter().find(|t| t.id == tag_id) else {
        return Err(CoreError::NotFound);
    };
    sqlx::query!(
        "DELETE FROM document_tags WHERE document_id = $1 AND tag_id = $2",
        id,
        tag_id
    )
    .execute(&mut *tx)
    .await?;
    refresh_search(&mut tx, id).await?;
    let names: Vec<String> = before.iter().map(|t| t.name.clone()).collect();
    let after: Vec<String> = before
        .iter()
        .filter(|t| t.id != tag_id)
        .map(|t| t.name.clone())
        .collect();
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc_ref
                .event(ctx, Category::Metadata, "document.tag_removed")
                .change("tags", names, after)
                .context("tag", tag.name.clone()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Files and versions
// ---------------------------------------------------------------------------

/// Upload a new file version (requires `edit`).
pub async fn add_version(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    upload: Upload,
    kind: VersionKind,
    note: &str,
    milestone_date: Option<NaiveDate>,
) -> CoreResult<i32> {
    let doc_ref = access::authorize(core, ctx, id, Permission::Edit).await?;
    if !matches!(
        kind,
        VersionKind::Revision | VersionKind::Amendment | VersionKind::SignedCopy
    ) {
        return Err(CoreError::validation(
            "choose revision, amendment, or signed copy",
        ));
    }
    let stored = files::ingest(core, ctx, Some(id), &upload).await?;
    let location = filestore::put_version_file(
        core,
        &filestore::PutTarget {
            document_id: Some(id),
            external_id: &doc_ref.external_id,
            title: &doc_ref.title,
            visibility: doc_ref.attrs.visibility,
            department_id: doc_ref.attrs.department_id,
        },
        &stored,
        &upload.file.path,
    )
    .await?;
    let defs = fields::list(&core.db).await?;
    let mut tx = core.db.begin().await?;
    sqlx::query!("SELECT id FROM documents WHERE id = $1 FOR UPDATE", id)
        .fetch_one(&mut *tx)
        .await?;
    let previous = sqlx::query_scalar!(
        "SELECT blob_sha256 FROM document_versions WHERE document_id = $1 ORDER BY version_no DESC LIMIT 1",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let kind = if previous.is_none() {
        VersionKind::Initial
    } else {
        kind
    };
    files::record_blob(&mut tx, &stored, filestore::backend_name(core)).await?;
    let version = append_version(
        &mut tx,
        ctx,
        id,
        NewVersion {
            kind,
            file: Some((&stored, stored.file_name.as_str())),
            note,
            milestone_date,
        },
        &defs,
    )
    .await?;
    filestore::record_location(&mut tx, id, version, &location).await?;
    refresh_search(&mut tx, id).await?;
    let mut e = file_context(
        doc_ref
            .event(ctx, Category::Content, "document.file_uploaded")
            .context("version", version)
            .context("kind", kind.as_str())
            .context("previous_sha256", previous.map(hex::encode)),
        &stored,
    );
    if !note.trim().is_empty() {
        e = e.context("note", note.trim().chars().take(1000).collect::<String>());
    }
    if let Some(m) = milestone_date {
        e = e.context("milestone_date", m);
    }
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(version)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RestorePart {
    /// Make an earlier file current again.
    File,
    /// Restore title, description, dates, custom fields, and tags. Owner,
    /// department, status, and access settings are not restored; they have
    /// their own permissions.
    Metadata,
}

pub async fn restore_version(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    version_no: i32,
    part: RestorePart,
) -> CoreResult<i32> {
    let doc_ref = access::authorize(core, ctx, id, Permission::Edit).await?;
    let old = sqlx::query!(
        "SELECT v.blob_sha256, v.file_name, v.metadata, b.size_bytes AS \"size_bytes?\", b.media_type AS \"media_type?\"
           FROM document_versions v LEFT JOIN blobs b ON b.sha256 = v.blob_sha256
          WHERE v.document_id = $1 AND v.version_no = $2",
        id,
        version_no
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let defs = fields::list(&core.db).await?;
    // With SharePoint, the restored file becomes a new file in the document's
    // folder: copy the earlier one out (verified) and upload it again.
    let restored = match (
        part,
        core.sharepoint.is_some(),
        old.blob_sha256.as_deref(),
        old.file_name.as_deref(),
    ) {
        (RestorePart::File, true, Some(sha), Some(name)) => {
            let hash =
                BlobHash::from_slice(sha).ok_or_else(|| CoreError::internal("bad blob hash"))?;
            let size = u64::try_from(old.size_bytes.unwrap_or(0)).unwrap_or(0);
            let copy = filestore::copy_for_restore(core, id, version_no, hash, size).await?;
            Some(
                filestore::put_version_file(
                    core,
                    &filestore::PutTarget {
                        document_id: Some(id),
                        external_id: &doc_ref.external_id,
                        title: &doc_ref.title,
                        visibility: doc_ref.attrs.visibility,
                        department_id: doc_ref.attrs.department_id,
                    },
                    &StoredFile {
                        sha256: hash,
                        size,
                        media_type: filestore::known_media_type(
                            old.media_type.as_deref().unwrap_or(""),
                        ),
                        file_name: name.to_owned(),
                        scan: "previously_accepted",
                    },
                    &copy.path,
                )
                .await?,
            )
        }
        _ => None,
    };
    let mut tx = core.db.begin().await?;
    let before_doc = fetch_live(&mut tx, id).await?;
    let note = format!(
        "Restored {} from version {version_no}",
        if part == RestorePart::File {
            "file"
        } else {
            "details"
        }
    );
    let mut events = Vec::new();
    let version = match part {
        RestorePart::File => {
            let (Some(sha), Some(name)) = (old.blob_sha256, old.file_name) else {
                return Err(CoreError::validation("that version has no file"));
            };
            let hash =
                BlobHash::from_slice(&sha).ok_or_else(|| CoreError::internal("bad blob hash"))?;
            let stored = StoredFile {
                sha256: hash,
                size: u64::try_from(old.size_bytes.unwrap_or(0)).unwrap_or(0),
                media_type: "",
                file_name: name.clone(),
                scan: "previously_accepted",
            };
            let v = append_version(
                &mut tx,
                ctx,
                id,
                NewVersion {
                    kind: VersionKind::Restore,
                    file: Some((&stored, &name)),
                    note: &note,
                    milestone_date: None,
                },
                &defs,
            )
            .await?;
            if let Some(loc) = &restored {
                filestore::record_location(&mut tx, id, v, loc).await?;
            }
            events.push(
                doc_ref
                    .event(ctx, Category::Content, "document.version_restored")
                    .context("restored_from_version", version_no)
                    .context("part", "file")
                    .context("sha256", hash.hex())
                    .context("file_name", name)
                    .context("version", v),
            );
            v
        }
        RestorePart::Metadata => {
            let m = &old.metadata;
            let date = |k: &str| {
                m.get(k)
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<NaiveDate>().ok())
            };
            let after = Meta {
                title: m
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(&before_doc.title)
                    .to_owned(),
                description: m
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                document_type_id: before_doc.document_type_id,
                department_id: before_doc.department_id,
                execution_date: date("execution_date"),
                effective_date: date("effective_date"),
                last_review_date: date("last_review_date"),
                next_review_date: date("next_review_date"),
                renewal_date: date("renewal_date"),
                expiration_date: date("expiration_date"),
                review_interval_months: m
                    .get("review_interval_months")
                    .and_then(Value::as_i64)
                    .and_then(|v| i32::try_from(v).ok()),
                custom_fields: m
                    .get("custom_fields")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default())),
            };
            let before = Meta::from(&before_doc);
            sqlx::query!(
                "UPDATE documents SET title = $2, description = $3, execution_date = $4, effective_date = $5,
                        last_review_date = $6, next_review_date = $7, renewal_date = $8, expiration_date = $9,
                        review_interval_months = $10, custom_fields = $11, search_extra = $12,
                        row_version = row_version + 1, updated_at = now()
                  WHERE id = $1",
                id,
                after.title,
                after.description,
                after.execution_date,
                after.effective_date,
                after.last_review_date,
                after.next_review_date,
                after.renewal_date,
                after.expiration_date,
                after.review_interval_months,
                after.custom_fields,
                search_extra(&defs, &after.custom_fields),
            )
            .execute(&mut *tx)
            .await?;
            // Tags: restore the snapshot's set (only tags that still exist).
            let wanted: Vec<String> = m
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(tags::slugify))
                        .collect()
                })
                .unwrap_or_default();
            sqlx::query!("DELETE FROM document_tags WHERE document_id = $1", id)
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "INSERT INTO document_tags (document_id, tag_id, added_by)
                 SELECT $1, id, $3 FROM tags WHERE slug = ANY($2)",
                id,
                &wanted,
                ctx.user_id()
            )
            .execute(&mut *tx)
            .await?;
            let v = append_version(
                &mut tx,
                ctx,
                id,
                NewVersion {
                    kind: VersionKind::Restore,
                    file: None,
                    note: &note,
                    milestone_date: None,
                },
                &defs,
            )
            .await?;
            let after_tags: Vec<String> = fetch_live(&mut tx, id)
                .await?
                .tags
                .into_iter()
                .map(|t| t.name)
                .collect();
            let names = names_for(core, &[before.document_type_id, before.department_id]).await?;
            events.push(metadata_changes(
                doc_ref
                    .event(ctx, Category::Metadata, "document.version_restored")
                    .context("restored_from_version", version_no)
                    .context("part", "metadata")
                    .context("version", v)
                    .change(
                        "tags",
                        before_doc
                            .tags
                            .iter()
                            .map(|t| t.name.clone())
                            .collect::<Vec<_>>(),
                        after_tags,
                    ),
                Some(&before),
                &after,
                &names,
                &defs,
            ));
            v
        }
    };
    refresh_search(&mut tx, id).await?;
    crate::sharepoint::mirror_later(core, &mut tx, id).await?;
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(version)
}

pub struct FileDownload {
    pub stream: ByteStream,
    pub file_name: String,
    pub media_type: String,
    pub size: i64,
    pub version_no: i32,
}

/// Open a version's file (latest file if `version_no` is `None`). Requires `view`; audited.
pub async fn open_file(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    version_no: Option<i32>,
) -> CoreResult<FileDownload> {
    let doc_ref = access::authorize(core, ctx, id, Permission::View).await?;
    let v = sqlx::query!(
        r#"SELECT v.version_no, v.blob_sha256 AS "blob_sha256!", v.file_name AS "file_name!",
                  b.media_type, b.size_bytes
             FROM document_versions v JOIN blobs b ON b.sha256 = v.blob_sha256
            WHERE v.document_id = $1 AND ($2::int IS NULL OR v.version_no = $2)
            ORDER BY v.version_no DESC LIMIT 1"#,
        id,
        version_no
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let hash =
        BlobHash::from_slice(&v.blob_sha256).ok_or_else(|| CoreError::internal("bad blob hash"))?;
    let stream = filestore::open(
        core,
        id,
        v.version_no,
        hash,
        u64::try_from(v.size_bytes).unwrap_or(0),
    )
    .await?;
    pdms_audit::record(
        &core.db,
        vec![
            doc_ref
                .event(ctx, Category::Access, "document.downloaded")
                .context("version", v.version_no)
                .context("sha256", hash.hex())
                .context("file_name", v.file_name.clone()),
        ],
    )
    .await?;
    Ok(FileDownload {
        stream,
        file_name: v.file_name,
        media_type: v.media_type,
        size: v.size_bytes,
        version_no: v.version_no,
    })
}

// ---------------------------------------------------------------------------
// Sharing
// ---------------------------------------------------------------------------

/// Grant access to this one document (requires `manage_permissions`).
pub async fn share(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    subject: Subject,
    permission: Permission,
    expires_at: Option<DateTime<Utc>>,
    reason: &str,
) -> CoreResult<Uuid> {
    access::authorize(core, ctx, id, Permission::ManagePermissions).await?;
    acl::grant_authorized(
        &core.db,
        ctx,
        subject,
        Scope::Document(id),
        permission,
        expires_at,
        reason,
    )
    .await
}

pub async fn unshare(core: &Core, ctx: &Ctx, id: Uuid, grant_id: Uuid) -> CoreResult<()> {
    access::authorize(core, ctx, id, Permission::ManagePermissions).await?;
    acl::revoke_document_grant(&core.db, ctx, id, grant_id).await
}

pub async fn grants(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<Vec<acl::GrantView>> {
    access::authorize(core, ctx, id, Permission::ManagePermissions).await?;
    acl::list_document_grants(&core.db, id).await
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

/// The document's audit timeline, oldest first. Visible to the owner, anyone
/// with `manage_permissions` on it, and audit viewers. Reading it is audited.
pub async fn timeline(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
) -> CoreResult<Vec<pdms_audit::SealedEvent>> {
    let doc_ref = access::authorize(core, ctx, id, Permission::View).await?;
    let is_owner = ctx.user_id() == Some(doc_ref.attrs.owner_id);
    let allowed = ctx.can(Capability::ViewAudit)
        || is_owner
        || access::permissions(core, ctx, &doc_ref)
            .await?
            .contains(Permission::ManagePermissions);
    if !allowed {
        return Err(CoreError::Forbidden(None));
    }
    let filter = crate::audit_log::AuditFilter {
        document_id: Some(id),
        ..Default::default()
    };
    let mut entries =
        crate::audit_log::fetch(&core.db, &filter, crate::audit_log::MAX_EXPORT_ROWS).await?;
    entries.reverse();
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Access,
                "audit.searched",
                ctx.actor.clone(),
                "audit_log",
            )
            .document(id)
            .department(Some(doc_ref.attrs.department_id))
            .context("scope", "document_timeline")
            .context("results", i64::try_from(entries.len()).unwrap_or(i64::MAX)),
        ],
    )
    .await?;
    Ok(entries)
}

/// New schedule dates for a document (from completing a review or renewal).
#[derive(Clone, Debug)]
pub(crate) struct ScheduleChange {
    pub last_review_date: Option<NaiveDate>,
    pub next_review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
}

/// Apply schedule dates inside the caller's transaction: updates the
/// document, appends a version, and returns the audit event with the diffs.
/// Set one date (or the review interval) inside a transaction, as an
/// ordinary metadata version, e.g. when a person confirms a date found in
/// the file. The caller has authorized `edit`; returns the audit entry.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_date_field(
    conn: &mut PgConnection,
    ctx: &Ctx,
    doc_ref: &DocRef,
    field: crate::ocr::dates::Field,
    date: Option<NaiveDate>,
    months: Option<i32>,
    note: &str,
    defs: &[FieldDef],
) -> CoreResult<AuditEvent> {
    use crate::ocr::dates::Field;
    let before = fetch_live(conn, doc_ref.id()).await?;
    let id = doc_ref.id();
    let (old, new): (AuditValue, AuditValue) = match field {
        Field::ReviewInterval => {
            let m = months
                .filter(|m| (1..=600).contains(m))
                .ok_or_else(|| CoreError::validation("the review interval must be 1–600 months"))?;
            sqlx::query!(
                "UPDATE documents SET review_interval_months = $2 WHERE id = $1",
                id,
                m
            )
            .execute(&mut *conn)
            .await?;
            (before.review_interval_months.into(), Some(m).into())
        }
        _ => {
            let d = date.ok_or_else(|| CoreError::validation("a date is required"))?;
            let old = match field {
                Field::Execution => {
                    sqlx::query!(
                        "UPDATE documents SET execution_date = $2 WHERE id = $1",
                        id,
                        d
                    )
                    .execute(&mut *conn)
                    .await?;
                    before.execution_date
                }
                Field::Effective => {
                    sqlx::query!(
                        "UPDATE documents SET effective_date = $2 WHERE id = $1",
                        id,
                        d
                    )
                    .execute(&mut *conn)
                    .await?;
                    before.effective_date
                }
                Field::Renewal => {
                    sqlx::query!(
                        "UPDATE documents SET renewal_date = $2 WHERE id = $1",
                        id,
                        d
                    )
                    .execute(&mut *conn)
                    .await?;
                    before.renewal_date
                }
                Field::Expiration => {
                    sqlx::query!(
                        "UPDATE documents SET expiration_date = $2 WHERE id = $1",
                        id,
                        d
                    )
                    .execute(&mut *conn)
                    .await?;
                    before.expiration_date
                }
                Field::NextReview | Field::ReviewInterval => {
                    sqlx::query!(
                        "UPDATE documents SET next_review_date = $2 WHERE id = $1",
                        id,
                        d
                    )
                    .execute(&mut *conn)
                    .await?;
                    before.next_review_date
                }
            };
            (old.into(), Some(d).into())
        }
    };
    sqlx::query!(
        "UPDATE documents SET row_version = row_version + 1, updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut *conn)
    .await?;
    let version = append_version(
        conn,
        ctx,
        id,
        NewVersion {
            kind: VersionKind::Metadata,
            file: None,
            note,
            milestone_date: None,
        },
        defs,
    )
    .await?;
    Ok(doc_ref
        .event(ctx, Category::Metadata, "document.metadata_updated")
        .context("version", version)
        .context("reason", note.to_owned())
        .change(field.as_str(), old, new))
}

pub(crate) async fn apply_schedule_change(
    conn: &mut PgConnection,
    ctx: &Ctx,
    doc_ref: &DocRef,
    change: &ScheduleChange,
    note: &str,
    defs: &[FieldDef],
) -> CoreResult<AuditEvent> {
    let before = fetch_live(conn, doc_ref.id()).await?;
    sqlx::query!(
        "UPDATE documents SET last_review_date = $2, next_review_date = $3, renewal_date = $4, expiration_date = $5,
                row_version = row_version + 1, updated_at = now()
          WHERE id = $1",
        doc_ref.id(),
        change.last_review_date,
        change.next_review_date,
        change.renewal_date,
        change.expiration_date,
    )
    .execute(&mut *conn)
    .await?;
    let version = append_version(
        conn,
        ctx,
        doc_ref.id(),
        NewVersion {
            kind: VersionKind::Metadata,
            file: None,
            note,
            milestone_date: None,
        },
        defs,
    )
    .await?;
    Ok(doc_ref
        .event(ctx, Category::Metadata, "document.metadata_updated")
        .context("version", version)
        .context("reason", note.to_owned())
        .change(
            "last_review_date",
            before.last_review_date,
            change.last_review_date,
        )
        .change(
            "next_review_date",
            before.next_review_date,
            change.next_review_date,
        )
        .change("renewal_date", before.renewal_date, change.renewal_date)
        .change(
            "expiration_date",
            before.expiration_date,
            change.expiration_date,
        ))
}
