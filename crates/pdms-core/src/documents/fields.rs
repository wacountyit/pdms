// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Custom fields: versioned definitions and typed values.
//!
//! A definition's `key` and `field_type` never change. Everything else
//! (label, help text, required, options, validation, default) is versioned:
//! an edit appends a row to `custom_field_versions`. Values in
//! `documents.custom_fields` are keyed by `key` and are never rewritten when a
//! definition changes, so renaming, adding, or deprecating a field cannot
//! break existing records or exports.
//!
//! Value encoding (JSON): text, long text, URL, date (`YYYY-MM-DD`), number
//! (decimal string, so money stays exact), user and department (UUID strings),
//! single select (option value) as strings; boolean as a JSON bool; multi
//! select as an array of option values.

use std::collections::BTreeMap;

use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::Capability;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "field_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    LongText,
    Number,
    Date,
    Boolean,
    SingleSelect,
    MultiSelect,
    User,
    Department,
    Url,
}

impl FieldType {
    pub const ALL: [FieldType; 10] = [
        FieldType::Text,
        FieldType::LongText,
        FieldType::Number,
        FieldType::Date,
        FieldType::Boolean,
        FieldType::SingleSelect,
        FieldType::MultiSelect,
        FieldType::User,
        FieldType::Department,
        FieldType::Url,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::LongText => "long_text",
            FieldType::Number => "number",
            FieldType::Date => "date",
            FieldType::Boolean => "boolean",
            FieldType::SingleSelect => "single_select",
            FieldType::MultiSelect => "multi_select",
            FieldType::User => "user",
            FieldType::Department => "department",
            FieldType::Url => "url",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FieldType::Text => "Text",
            FieldType::LongText => "Long text",
            FieldType::Number => "Number",
            FieldType::Date => "Date",
            FieldType::Boolean => "Yes/No",
            FieldType::SingleSelect => "Single select",
            FieldType::MultiSelect => "Multi select",
            FieldType::User => "User",
            FieldType::Department => "Department",
            FieldType::Url => "Link (URL)",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    pub fn has_options(self) -> bool {
        matches!(self, FieldType::SingleSelect | FieldType::MultiSelect)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// A field definition at its current version.
#[derive(Clone, Debug, Serialize)]
pub struct FieldDef {
    pub id: Uuid,
    pub key: String,
    pub field_type: FieldType,
    pub document_type_id: Option<Uuid>,
    pub document_type_name: Option<String>,
    pub version: i32,
    pub is_deprecated: bool,
    pub sort_order: i32,
    pub label: String,
    pub help_text: String,
    pub required: bool,
    pub options: Vec<SelectOption>,
    pub validation: Validation,
    pub default_value: Option<Value>,
}

impl FieldDef {
    /// Whether the field appears on documents of `type_id`.
    pub fn applies_to(&self, type_id: Uuid) -> bool {
        self.document_type_id.is_none_or(|t| t == type_id)
    }

    fn option_label(&self, value: &str) -> String {
        self.options
            .iter()
            .find(|o| o.value == value)
            .map_or_else(|| format!("{value} (retired option)"), |o| o.label.clone())
    }
}

const DEFAULT_TEXT_MAX: usize = 500;
const DEFAULT_LONG_TEXT_MAX: usize = 10_000;

fn number_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^-?\d{1,15}(\.\d{1,6})?$").unwrap_or_else(|_| unreachable!())
    })
}

/// Normalize a decimal: strip `$`, commas, and spaces; drop trailing zeros.
fn normalize_number(raw: &str) -> Option<String> {
    let s: String = raw
        .chars()
        .filter(|c| !matches!(c, ',' | '$' | ' '))
        .collect();
    if !number_re().is_match(&s) {
        return None;
    }
    Some(match s.split_once('.') {
        Some((i, f)) => {
            let f = f.trim_end_matches('0');
            if f.is_empty() {
                i.to_owned()
            } else {
                format!("{i}.{f}")
            }
        }
        None => s,
    })
}

fn decimal_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
    a.partial_cmp(&b)
}

/// Validate raw form input for one field. Returns `Ok(None)` for "no value".
pub fn parse_value(def: &FieldDef, raw: &[String]) -> Result<Option<Value>, String> {
    let first = raw.iter().map(|s| s.trim()).find(|s| !s.is_empty());
    let v = &def.validation;
    match def.field_type {
        FieldType::Boolean => Ok(Some(Value::Bool(
            raw.iter()
                .any(|s| matches!(s.trim(), "true" | "on" | "yes" | "1")),
        ))),
        FieldType::MultiSelect => {
            let chosen: Vec<&str> = raw
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if let Some(bad) = chosen
                .iter()
                .find(|c| !def.options.iter().any(|o| o.value == **c))
            {
                return Err(format!("“{bad}” is not one of the choices"));
            }
            // Keep option order and drop duplicates.
            let values: Vec<Value> = def
                .options
                .iter()
                .filter(|o| chosen.contains(&o.value.as_str()))
                .map(|o| Value::String(o.value.clone()))
                .collect();
            Ok((!values.is_empty()).then_some(Value::Array(values)))
        }
        _ => {
            let Some(s) = first else { return Ok(None) };
            let value = match def.field_type {
                FieldType::Text | FieldType::LongText => {
                    let max = v
                        .max_length
                        .unwrap_or(if def.field_type == FieldType::Text {
                            DEFAULT_TEXT_MAX
                        } else {
                            DEFAULT_LONG_TEXT_MAX
                        });
                    if s.chars().count() > max {
                        return Err(format!("must be at most {max} characters"));
                    }
                    if let Some(p) = &v.pattern {
                        let re = regex::Regex::new(p)
                            .map_err(|_| "has an invalid pattern rule".to_owned())?;
                        if !re.is_match(s) {
                            return Err("is not in the expected format".into());
                        }
                    }
                    s.to_owned()
                }
                FieldType::Number => {
                    let n =
                        normalize_number(s).ok_or("must be a number (up to 6 decimal places)")?;
                    if let Some(min) = &v.min
                        && decimal_cmp(&n, min) == Some(std::cmp::Ordering::Less)
                    {
                        return Err(format!("must be at least {min}"));
                    }
                    if let Some(max) = &v.max
                        && decimal_cmp(&n, max) == Some(std::cmp::Ordering::Greater)
                    {
                        return Err(format!("must be at most {max}"));
                    }
                    n
                }
                FieldType::Date => {
                    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .map_err(|_| "must be a date (YYYY-MM-DD)".to_owned())?;
                    let iso = d.format("%Y-%m-%d").to_string();
                    if v.min.as_deref().is_some_and(|m| iso.as_str() < m) {
                        return Err(format!(
                            "must be on or after {}",
                            v.min.as_deref().unwrap_or("")
                        ));
                    }
                    if v.max.as_deref().is_some_and(|m| iso.as_str() > m) {
                        return Err(format!(
                            "must be on or before {}",
                            v.max.as_deref().unwrap_or("")
                        ));
                    }
                    iso
                }
                FieldType::SingleSelect => {
                    if !def.options.iter().any(|o| o.value == s) {
                        return Err(format!("“{s}” is not one of the choices"));
                    }
                    s.to_owned()
                }
                FieldType::User | FieldType::Department => s
                    .parse::<Uuid>()
                    .map_err(|_| "is not a valid selection".to_owned())?
                    .to_string(),
                FieldType::Url => {
                    let u =
                        url::Url::parse(s).map_err(|_| "must be a full web address".to_owned())?;
                    if !matches!(u.scheme(), "http" | "https") || s.len() > 2000 {
                        return Err("must be an http(s) address".into());
                    }
                    u.to_string()
                }
                FieldType::Boolean | FieldType::MultiSelect => unreachable!("handled above"),
            };
            Ok(Some(Value::String(value)))
        }
    }
}

/// Validate every applicable field from form input and merge into the
/// existing values. Values of deprecated fields and of fields that belong to
/// other document types are kept untouched.
pub fn merge_values(
    defs: &[FieldDef],
    type_id: Uuid,
    raw: &BTreeMap<String, Vec<String>>,
    existing: &Value,
) -> CoreResult<Value> {
    let mut out = existing.as_object().cloned().unwrap_or_default();
    let mut errors = Vec::new();
    for def in defs
        .iter()
        .filter(|d| d.applies_to(type_id) && !d.is_deprecated)
    {
        let input = raw.get(&def.key).map(Vec::as_slice).unwrap_or(&[]);
        match parse_value(def, input) {
            Ok(Some(v)) => {
                out.insert(def.key.clone(), v);
            }
            Ok(None) if def.required => errors.push(format!("{} is required", def.label)),
            Ok(None) => {
                out.remove(&def.key);
            }
            Err(e) => errors.push(format!("{} {e}", def.label)),
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(CoreError::Validation(errors.join("; ")))
    }
}

/// Display text for a stored value. `names` resolves user and department IDs.
pub fn display(
    def: Option<&FieldDef>,
    value: &Value,
    names: &dyn Fn(&str) -> Option<String>,
) -> String {
    match (def.map(|d| d.field_type), value) {
        (_, Value::Bool(b)) => if *b { "Yes" } else { "No" }.to_owned(),
        (Some(FieldType::SingleSelect), Value::String(s)) => {
            def.map_or_else(|| s.clone(), |d| d.option_label(s))
        }
        (Some(FieldType::MultiSelect), Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|s| def.map_or_else(|| s.to_owned(), |d| d.option_label(s)))
            .collect::<Vec<_>>()
            .join(", "),
        (Some(FieldType::User | FieldType::Department), Value::String(s)) => {
            names(s).unwrap_or_else(|| format!("{s} (unknown)"))
        }
        (_, Value::String(s)) => s.clone(),
        (_, Value::Array(items)) => items
            .iter()
            .map(|i| i.as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
            .join(", "),
        (_, Value::Null) => String::new(),
        (_, other) => other.to_string(),
    }
}

/// Text that full-text search should see for this value.
pub fn search_text(def: &FieldDef, value: &Value) -> Option<String> {
    match def.field_type {
        FieldType::Text | FieldType::LongText | FieldType::Url | FieldType::Number => {
            value.as_str().map(str::to_owned)
        }
        FieldType::SingleSelect | FieldType::MultiSelect => {
            Some(display(Some(def), value, &|_| None))
        }
        _ => None,
    }
}

/// Audit representation (no floats: numbers are already strings).
pub fn audit_value(value: Option<&Value>) -> AuditValue {
    value.map_or(AuditValue::Null, |v| {
        AuditValue::from_json(v).unwrap_or_else(|_| AuditValue::Str(v.to_string()))
    })
}

// ---------------------------------------------------------------------------
// Definitions: storage
// ---------------------------------------------------------------------------

struct DefRow {
    id: Uuid,
    key: String,
    field_type: FieldType,
    document_type_id: Option<Uuid>,
    document_type_name: Option<String>,
    version: i32,
    is_deprecated: bool,
    sort_order: i32,
    label: String,
    help_text: String,
    required: bool,
    options: Value,
    validation: Value,
    default_value: Option<Value>,
}

impl From<DefRow> for FieldDef {
    fn from(r: DefRow) -> Self {
        FieldDef {
            id: r.id,
            key: r.key,
            field_type: r.field_type,
            document_type_id: r.document_type_id,
            document_type_name: r.document_type_name,
            version: r.version,
            is_deprecated: r.is_deprecated,
            sort_order: r.sort_order,
            label: r.label,
            help_text: r.help_text,
            required: r.required,
            options: serde_json::from_value(r.options).unwrap_or_default(),
            validation: serde_json::from_value(r.validation).unwrap_or_default(),
            default_value: r.default_value,
        }
    }
}

/// Every definition at its current version (including deprecated ones).
pub async fn list(db: &PgPool) -> CoreResult<Vec<FieldDef>> {
    let rows = sqlx::query_as!(
        DefRow,
        r#"SELECT f.id, f.key, f.field_type AS "field_type: FieldType", f.document_type_id,
                  t.name AS "document_type_name?", f.current_version AS version, f.is_deprecated,
                  f.sort_order, v.label, v.help_text, v.required, v.options, v.validation, v.default_value
             FROM custom_field_definitions f
             JOIN custom_field_versions v ON v.field_id = f.id AND v.version = f.current_version
             LEFT JOIN document_types t ON t.id = f.document_type_id
            ORDER BY f.document_type_id NULLS FIRST, f.sort_order, v.label"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(FieldDef::from).collect())
}

pub async fn get(db: &PgPool, id: Uuid) -> CoreResult<FieldDef> {
    list(db)
        .await?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or(CoreError::NotFound)
}

/// One historical version of a definition.
#[derive(Clone, Debug, Serialize)]
pub struct FieldVersion {
    pub version: i32,
    pub label: String,
    pub help_text: String,
    pub required: bool,
    pub options: Value,
    pub validation: Value,
    pub created_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn history(db: &PgPool, id: Uuid) -> CoreResult<Vec<FieldVersion>> {
    Ok(sqlx::query_as!(
        FieldVersion,
        r#"SELECT v.version, v.label, v.help_text, v.required, v.options, v.validation,
                  u.display_name AS "created_by?", v.created_at
             FROM custom_field_versions v LEFT JOIN users u ON u.id = v.created_by
            WHERE v.field_id = $1 ORDER BY v.version DESC"#,
        id
    )
    .fetch_all(db)
    .await?)
}

/// The editable part of a definition.
#[derive(Clone, Debug, Default)]
pub struct FieldSpec {
    pub label: String,
    pub help_text: String,
    pub required: bool,
    pub options: Vec<SelectOption>,
    pub validation: Validation,
    pub default_value: Option<Value>,
    pub sort_order: i32,
}

fn check_spec(field_type: FieldType, spec: &mut FieldSpec) -> CoreResult<()> {
    spec.label = spec.label.trim().to_owned();
    if spec.label.is_empty() || spec.label.chars().count() > 200 {
        return Err(CoreError::validation("label must be 1–200 characters"));
    }
    spec.help_text = spec.help_text.trim().chars().take(1000).collect();
    if field_type.has_options() {
        if spec.options.is_empty() {
            return Err(CoreError::validation(
                "select fields need at least one option",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for o in &mut spec.options {
            o.value = o.value.trim().to_owned();
            o.label = o.label.trim().to_owned();
            if o.value.is_empty() || o.value.len() > 100 || !seen.insert(o.value.clone()) {
                return Err(CoreError::validation(
                    "options must have unique, non-empty values",
                ));
            }
            if o.label.is_empty() {
                o.label = o.value.clone();
            }
        }
    } else {
        spec.options.clear();
    }
    if let Some(p) = &spec.validation.pattern
        && regex::Regex::new(p).is_err()
    {
        return Err(CoreError::validation(
            "the pattern is not a valid regular expression",
        ));
    }
    if field_type == FieldType::Number {
        for b in [&mut spec.validation.min, &mut spec.validation.max]
            .into_iter()
            .flatten()
        {
            *b = normalize_number(b)
                .ok_or_else(|| CoreError::validation("min/max must be numbers"))?;
        }
    }
    if let Some(dv) = &spec.default_value {
        let probe = FieldDef {
            id: Uuid::nil(),
            key: String::new(),
            field_type,
            document_type_id: None,
            document_type_name: None,
            version: 0,
            is_deprecated: false,
            sort_order: 0,
            label: spec.label.clone(),
            help_text: String::new(),
            required: false,
            options: spec.options.clone(),
            validation: spec.validation.clone(),
            default_value: None,
        };
        let raw: Vec<String> = match dv {
            Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Value::Bool(b) => vec![b.to_string()],
            Value::String(s) => vec![s.clone()],
            other => vec![other.to_string()],
        };
        spec.default_value = parse_value(&probe, &raw)
            .map_err(|e| CoreError::validation(format!("default value {e}")))?;
    }
    Ok(())
}

fn spec_changes(e: AuditEvent, before: Option<&FieldDef>, spec: &FieldSpec) -> AuditEvent {
    let opts = |o: &[SelectOption]| serde_json::to_string(o).unwrap_or_default();
    let val = |v: &Validation| serde_json::to_string(v).unwrap_or_default();
    let dv = |v: &Option<Value>| v.as_ref().map(Value::to_string);
    e.change("label", before.map(|b| b.label.clone()), spec.label.clone())
        .change(
            "help_text",
            before.map(|b| b.help_text.clone()),
            spec.help_text.clone(),
        )
        .change("required", before.map(|b| b.required), spec.required)
        .change(
            "options",
            before.map(|b| opts(&b.options)),
            opts(&spec.options),
        )
        .change(
            "validation",
            before.map(|b| val(&b.validation)),
            val(&spec.validation),
        )
        .change(
            "default_value",
            before.and_then(|b| dv(&b.default_value)),
            dv(&spec.default_value),
        )
        .change("sort_order", before.map(|b| b.sort_order), spec.sort_order)
}

pub async fn create(
    db: &PgPool,
    ctx: &Ctx,
    key: &str,
    field_type: FieldType,
    document_type_id: Option<Uuid>,
    mut spec: FieldSpec,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDocumentSchema)?;
    let key = key.trim().to_ascii_lowercase();
    let key_ok = key.len() <= 63
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !key_ok {
        return Err(CoreError::validation(
            "key must start with a letter and use only lowercase letters, digits, and _",
        ));
    }
    check_spec(field_type, &mut spec)?;
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO custom_field_definitions (id, key, field_type, document_type_id, sort_order)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        key,
        field_type as FieldType,
        document_type_id,
        spec.sort_order,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => CoreError::NotFound,
        _ => CoreError::conflict_on_unique(e, "a field with that key already exists"),
    })?;
    insert_version(&mut tx, id, 1, &spec, ctx.user_id()).await?;
    let e = AuditEvent::new(
        Category::Admin,
        "custom_field.created",
        ctx.actor.clone(),
        "custom_field",
    )
    .object(id, Some(&spec.label))
    .context("key", key.as_str())
    .context("field_type", field_type.as_str())
    .context("document_type_id", document_type_id)
    .context("version", 1);
    pdms_audit::append(&mut tx, vec![spec_changes(e, None, &spec)]).await?;
    tx.commit().await?;
    Ok(id)
}

async fn insert_version(
    conn: &mut sqlx::PgConnection,
    id: Uuid,
    version: i32,
    spec: &FieldSpec,
    by: Option<Uuid>,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO custom_field_versions (field_id, version, label, help_text, required, options,
                                            validation, default_value, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        id,
        version,
        spec.label,
        spec.help_text,
        spec.required,
        serde_json::to_value(&spec.options).unwrap_or(Value::Array(vec![])),
        serde_json::to_value(&spec.validation).unwrap_or(Value::Object(Default::default())),
        spec.default_value,
        by,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Save a new version of a definition. Existing values are not touched.
pub async fn update(db: &PgPool, ctx: &Ctx, id: Uuid, mut spec: FieldSpec) -> CoreResult<i32> {
    ctx.require(Capability::ManageDocumentSchema)?;
    let before = get(db, id).await?;
    check_spec(before.field_type, &mut spec)?;
    let mut tx = db.begin().await?;
    let version = sqlx::query_scalar!(
        "UPDATE custom_field_definitions SET current_version = current_version + 1, sort_order = $2,
                updated_at = now()
          WHERE id = $1 RETURNING current_version",
        id,
        spec.sort_order
    )
    .fetch_one(&mut *tx)
    .await?;
    insert_version(&mut tx, id, version, &spec, ctx.user_id()).await?;
    let e = AuditEvent::new(
        Category::Admin,
        "custom_field.updated",
        ctx.actor.clone(),
        "custom_field",
    )
    .object(id, Some(&spec.label))
    .context("key", before.key.as_str())
    .context("version", version);
    pdms_audit::append(&mut tx, vec![spec_changes(e, Some(&before), &spec)]).await?;
    tx.commit().await?;
    Ok(version)
}

/// Deprecated fields disappear from forms but their values stay visible,
/// searchable, and exported.
pub async fn set_deprecated(db: &PgPool, ctx: &Ctx, id: Uuid, deprecated: bool) -> CoreResult<()> {
    ctx.require(Capability::ManageDocumentSchema)?;
    let before = get(db, id).await?;
    if before.is_deprecated == deprecated {
        return Ok(());
    }
    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE custom_field_definitions SET is_deprecated = $2, updated_at = now() WHERE id = $1",
        id,
        deprecated
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                if deprecated {
                    "custom_field.deprecated"
                } else {
                    "custom_field.reactivated"
                },
                ctx.actor.clone(),
                "custom_field",
            )
            .object(id, Some(&before.label))
            .context("key", before.key.as_str())
            .change("is_deprecated", before.is_deprecated, deprecated),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(t: FieldType) -> FieldDef {
        FieldDef {
            id: Uuid::nil(),
            key: "k".into(),
            field_type: t,
            document_type_id: None,
            document_type_name: None,
            version: 1,
            is_deprecated: false,
            sort_order: 0,
            label: "Field".into(),
            help_text: String::new(),
            required: false,
            options: vec![
                SelectOption {
                    value: "a".into(),
                    label: "Alpha".into(),
                },
                SelectOption {
                    value: "b".into(),
                    label: "Beta".into(),
                },
            ],
            validation: Validation::default(),
            default_value: None,
        }
    }

    fn p(t: FieldType, raw: &[&str]) -> Result<Option<Value>, String> {
        parse_value(
            &def(t),
            &raw.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn numbers_are_exact_strings() {
        assert_eq!(
            p(FieldType::Number, &["$1,234.50"]),
            Ok(Some(Value::String("1234.5".into())))
        );
        assert_eq!(
            p(FieldType::Number, &["-7"]),
            Ok(Some(Value::String("-7".into())))
        );
        assert!(p(FieldType::Number, &["1e5"]).is_err());
        assert!(p(FieldType::Number, &["1.1234567"]).is_err());
        let mut d = def(FieldType::Number);
        d.validation.min = Some("0".into());
        assert!(parse_value(&d, &["-1".into()]).is_err());
    }

    #[test]
    fn selects_validate_choices_and_keep_option_order() {
        assert!(p(FieldType::SingleSelect, &["z"]).is_err());
        assert_eq!(
            p(FieldType::MultiSelect, &["b", "a", "b"]),
            Ok(Some(serde_json::json!(["a", "b"])))
        );
        assert_eq!(p(FieldType::MultiSelect, &[]), Ok(None));
    }

    #[test]
    fn dates_urls_booleans_and_ids() {
        assert_eq!(
            p(FieldType::Date, &["2014-03-01"]),
            Ok(Some(Value::String("2014-03-01".into())))
        );
        assert!(p(FieldType::Date, &["03/01/2014"]).is_err());
        assert!(p(FieldType::Url, &["javascript:alert(1)"]).is_err());
        assert!(p(FieldType::Url, &["https://example.org/a"]).is_ok());
        assert_eq!(p(FieldType::Boolean, &[]), Ok(Some(Value::Bool(false))));
        assert_eq!(
            p(FieldType::Boolean, &["true"]),
            Ok(Some(Value::Bool(true)))
        );
        assert!(p(FieldType::User, &["not-a-uuid"]).is_err());
    }

    #[test]
    fn text_limits_and_patterns() {
        let mut d = def(FieldType::Text);
        d.validation.pattern = Some(r"^[A-Z]{3}-\d+$".into());
        assert!(parse_value(&d, &["ABC-12".into()]).is_ok());
        assert!(parse_value(&d, &["abc".into()]).is_err());
        assert!(p(FieldType::Text, &[&"x".repeat(501)]).is_err());
    }

    #[test]
    fn merge_keeps_deprecated_and_foreign_values_and_enforces_required() {
        let t = Uuid::from_u128(1);
        let mut req = def(FieldType::Text);
        req.key = "vendor".into();
        req.label = "Vendor".into();
        req.required = true;
        let mut old = def(FieldType::Text);
        old.key = "legacy".into();
        old.is_deprecated = true;
        let existing = serde_json::json!({"legacy": "kept", "other_type_field": "kept too"});
        let defs = vec![req, old];
        let err = merge_values(&defs, t, &BTreeMap::new(), &existing).unwrap_err();
        assert!(err.to_string().contains("Vendor is required"));
        let raw = BTreeMap::from([("vendor".to_owned(), vec!["Acme".to_owned()])]);
        let merged = merge_values(&defs, t, &raw, &existing).unwrap();
        assert_eq!(merged["vendor"], "Acme");
        assert_eq!(merged["legacy"], "kept");
        assert_eq!(merged["other_type_field"], "kept too");
    }

    #[test]
    fn retired_options_still_display() {
        let d = def(FieldType::SingleSelect);
        assert_eq!(
            display(Some(&d), &Value::String("a".into()), &|_| None),
            "Alpha"
        );
        assert_eq!(
            display(Some(&d), &Value::String("gone".into()), &|_| None),
            "gone (retired option)"
        );
        assert_eq!(
            display(None, &Value::String("raw".into()), &|_| None),
            "raw"
        );
    }
}
