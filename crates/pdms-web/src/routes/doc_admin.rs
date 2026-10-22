// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration of document types, custom fields, tags, retention, and deleted documents.
//! Each handler checks its own capability (records officers manage retention
//! without being directory administrators).

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::CoreError;
use pdms_core::documents::fields::{
    self, FieldDef, FieldSpec, FieldType, FieldVersion, SelectOption, Validation,
};
use pdms_core::documents::retention::RetentionBasis;
use pdms_core::documents::tags::{self, Tag};
use pdms_core::documents::types::{self, DocumentType, PolicySpec, RetentionPolicy, TypeSpec};
use pdms_core::documents::{self, DeletedDocument};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, tag_class};

fn checked(v: &Option<String>) -> bool {
    v.as_deref() == Some("true")
}

fn opt_int(s: &str, what: &str) -> Result<Option<i32>, CoreError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse()
        .map(Some)
        .map_err(|_| CoreError::validation(format!("{what} must be a whole number")))
}

fn opt_uuid(s: &str) -> AppResult<Option<Uuid>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse()
        .map(Some)
        .map_err(|_| AppError::BadRequest("invalid identifier".into()))
}

// ---------------------------------------------------------------------------
// Document types
// ---------------------------------------------------------------------------

/// Values for the shared document-type form.
pub struct TypeFormView {
    pub name: String,
    pub prefix: String,
    pub desc: String,
    pub interval: String,
    pub legal: bool,
    pub sign: bool,
    pub policy: Option<Uuid>,
    pub sort: String,
    pub active: bool,
}

impl TypeFormView {
    pub fn policy_selected(&self, id: &Uuid) -> bool {
        self.policy == Some(*id)
    }

    fn empty() -> Self {
        TypeFormView {
            name: String::new(),
            prefix: String::new(),
            desc: String::new(),
            interval: String::new(),
            legal: false,
            sign: false,
            policy: None,
            sort: "0".into(),
            active: true,
        }
    }

    fn of(t: &DocumentType) -> Self {
        TypeFormView {
            name: t.name.clone(),
            prefix: t.id_prefix.clone(),
            desc: t.description.clone(),
            interval: t
                .review_interval_months
                .map(|m| m.to_string())
                .unwrap_or_default(),
            legal: t.requires_legal_review,
            sign: t.requires_signatures,
            policy: t.retention_policy_id,
            sort: t.sort_order.to_string(),
            active: t.is_active,
        }
    }
}

/// Values for the shared custom-field form.
pub struct FieldFormView {
    pub help: String,
    pub required: bool,
    pub options: String,
    pub min: String,
    pub max: String,
    pub max_length: String,
    pub pattern: String,
    pub default: String,
    pub sort: String,
}

impl FieldFormView {
    fn empty() -> Self {
        FieldFormView {
            help: String::new(),
            required: false,
            options: String::new(),
            min: String::new(),
            max: String::new(),
            max_length: String::new(),
            pattern: String::new(),
            default: String::new(),
            sort: "0".into(),
        }
    }

    fn of(f: &FieldDef) -> Self {
        FieldFormView {
            help: f.help_text.clone(),
            required: f.required,
            options: f
                .options
                .iter()
                .map(|o| {
                    if o.value == o.label {
                        o.value.clone()
                    } else {
                        format!("{} | {}", o.value, o.label)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            min: f.validation.min.clone().unwrap_or_default(),
            max: f.validation.max.clone().unwrap_or_default(),
            max_length: f
                .validation
                .max_length
                .map(|n| n.to_string())
                .unwrap_or_default(),
            pattern: f.validation.pattern.clone().unwrap_or_default(),
            default: f
                .default_value
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            sort: f.sort_order.to_string(),
        }
    }
}

#[derive(Template)]
#[template(path = "admin/document_types.html")]
struct TypesPage {
    l: Layout,
    types: Vec<DocumentType>,
    policies: Vec<RetentionPolicy>,
    tf: TypeFormView,
}

pub async fn types_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(
        &state,
        Capability::ManageDocumentSchema,
        "/admin/document-types",
    )
    .await?;
    let page = TypesPage {
        l: Layout::new(&state, &user, flash, "Document types", "admin").admin_tab("document-types"),
        types: types::list_types(&state.core.db).await?,
        policies: types::list_policies(&state.core.db).await?,
        tf: TypeFormView::empty(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct TypeForm {
    #[serde(default)]
    key: String,
    name: String,
    #[serde(default)]
    description: String,
    id_prefix: String,
    #[serde(default)]
    review_interval_months: String,
    requires_legal_review: Option<String>,
    requires_signatures: Option<String>,
    #[serde(default)]
    retention_policy_id: String,
    is_active: Option<String>,
    #[serde(default)]
    sort_order: String,
}

impl TypeForm {
    fn spec(&self) -> AppResult<Result<TypeSpec, CoreError>> {
        let policy = opt_uuid(&self.retention_policy_id)?;
        Ok((|| {
            Ok(TypeSpec {
                name: self.name.clone(),
                description: self.description.clone(),
                id_prefix: self.id_prefix.clone(),
                review_interval_months: opt_int(&self.review_interval_months, "Review interval")?,
                requires_legal_review: checked(&self.requires_legal_review),
                requires_signatures: checked(&self.requires_signatures),
                retention_policy_id: policy,
                is_active: checked(&self.is_active),
                sort_order: opt_int(&self.sort_order, "Sort order")?.unwrap_or(0),
            })
        })())
    }
}

pub async fn create_type(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<TypeForm>,
) -> AppResult<Response> {
    let r = match f.spec()? {
        Ok(spec) => types::create_type(&state.core.db, &user.ctx, &f.key, spec)
            .await
            .map(|_| ()),
        Err(e) => Err(e),
    };
    done(&state, "/admin/document-types", r, "Document type added.")
}

#[derive(Template)]
#[template(path = "admin/document_type.html")]
struct TypePage {
    l: Layout,
    t: DocumentType,
    policies: Vec<RetentionPolicy>,
    tf: TypeFormView,
}

pub async fn type_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    user.require(
        &state,
        Capability::ManageDocumentSchema,
        "/admin/document-types",
    )
    .await?;
    let t = types::get_type(&state.core.db, id).await?;
    let page = TypePage {
        l: Layout::new(&state, &user, flash, &t.name, "admin").admin_tab("document-types"),
        policies: types::list_policies(&state.core.db).await?,
        tf: TypeFormView::of(&t),
        t,
    };
    Ok(Html(page.render()?))
}

pub async fn update_type(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<TypeForm>,
) -> AppResult<Response> {
    let r = match f.spec()? {
        Ok(spec) => types::update_type(&state.core.db, &user.ctx, id, spec).await,
        Err(e) => Err(e),
    };
    done(
        &state,
        &format!("/admin/document-types/{id}"),
        r,
        "Document type saved.",
    )
}

// ---------------------------------------------------------------------------
// Custom fields
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/fields.html")]
struct FieldsPage {
    l: Layout,
    fields: Vec<FieldDef>,
    types: Vec<DocumentType>,
    field_types: Vec<(&'static str, &'static str)>,
    ff: FieldFormView,
}

pub async fn fields_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageDocumentSchema, "/admin/fields")
        .await?;
    let page = FieldsPage {
        l: Layout::new(&state, &user, flash, "Custom fields", "admin").admin_tab("fields"),
        fields: fields::list(&state.core.db).await?,
        types: types::list_types(&state.core.db).await?,
        field_types: FieldType::ALL
            .iter()
            .map(|t| (t.as_str(), t.label()))
            .collect(),
        ff: FieldFormView::empty(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct FieldForm {
    #[serde(default)]
    key: String,
    #[serde(default)]
    field_type: String,
    #[serde(default)]
    document_type_id: String,
    label: String,
    #[serde(default)]
    help_text: String,
    required: Option<String>,
    #[serde(default)]
    options: String,
    #[serde(default)]
    min: String,
    #[serde(default)]
    max: String,
    #[serde(default)]
    max_length: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    default_value: String,
    #[serde(default)]
    sort_order: String,
}

impl FieldForm {
    /// Options are one per line: `value` or `value | Label`.
    fn spec(&self) -> Result<FieldSpec, CoreError> {
        let non_empty = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
        let options = self
            .options
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| match l.split_once('|') {
                Some((v, lab)) => SelectOption {
                    value: v.trim().to_owned(),
                    label: lab.trim().to_owned(),
                },
                None => SelectOption {
                    value: l.to_owned(),
                    label: l.to_owned(),
                },
            })
            .collect();
        Ok(FieldSpec {
            label: self.label.clone(),
            help_text: self.help_text.clone(),
            required: checked(&self.required),
            options,
            validation: Validation {
                min: non_empty(&self.min),
                max: non_empty(&self.max),
                max_length: opt_int(&self.max_length, "Maximum length")?
                    .and_then(|n| usize::try_from(n).ok()),
                pattern: non_empty(&self.pattern),
            },
            default_value: non_empty(&self.default_value).map(serde_json::Value::String),
            sort_order: opt_int(&self.sort_order, "Sort order")?.unwrap_or(0),
        })
    }
}

pub async fn create_field(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<FieldForm>,
) -> AppResult<Response> {
    let field_type = FieldType::parse(&f.field_type)
        .ok_or_else(|| AppError::BadRequest("unknown field type".into()))?;
    let doc_type = opt_uuid(&f.document_type_id)?;
    let r = match f.spec() {
        Ok(spec) => {
            fields::create(
                &state.core.db,
                &user.ctx,
                &f.key,
                field_type,
                doc_type,
                spec,
            )
            .await
        }
        Err(e) => Err(e),
    };
    match r {
        Ok(id) => done(
            &state,
            &format!("/admin/fields/{id}"),
            Ok(()),
            "Field added.",
        ),
        Err(e) => done::<()>(&state, "/admin/fields", Err(e), ""),
    }
}

#[derive(Template)]
#[template(path = "admin/field.html")]
struct FieldPage {
    l: Layout,
    f: FieldDef,
    ff: FieldFormView,
    history: Vec<(FieldVersion, String)>,
}

pub async fn field_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageDocumentSchema, "/admin/fields")
        .await?;
    let f = fields::get(&state.core.db, id).await?;
    let history = fields::history(&state.core.db, id)
        .await?
        .into_iter()
        .map(|v| {
            let when = fmt_time(state.tz, v.created_at);
            (v, when)
        })
        .collect();
    let page = FieldPage {
        l: Layout::new(
            &state,
            &user,
            flash,
            &format!("Field: {}", f.label),
            "admin",
        )
        .admin_tab("fields"),
        ff: FieldFormView::of(&f),
        history,
        f,
    };
    Ok(Html(page.render()?))
}

pub async fn update_field(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<FieldForm>,
) -> AppResult<Response> {
    let r = match f.spec() {
        Ok(spec) => fields::update(&state.core.db, &user.ctx, id, spec)
            .await
            .map(|_| ()),
        Err(e) => Err(e),
    };
    done(
        &state,
        &format!("/admin/fields/{id}"),
        r,
        "Field saved as a new version. Existing values are unchanged.",
    )
}

#[derive(Deserialize)]
pub struct DeprecateForm {
    deprecated: String,
}

pub async fn deprecate_field(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<DeprecateForm>,
) -> AppResult<Response> {
    let on = f.deprecated == "true";
    let r = fields::set_deprecated(&state.core.db, &user.ctx, id, on).await;
    done(
        &state,
        &format!("/admin/fields/{id}"),
        r,
        if on {
            "Field deprecated."
        } else {
            "Field reactivated."
        },
    )
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/tags.html")]
struct TagsPage {
    l: Layout,
    tags: Vec<(Tag, String)>,
}

pub async fn tags_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageTags, "/admin/tags")
        .await?;
    let page = TagsPage {
        l: Layout::new(&state, &user, flash, "Tags", "admin").admin_tab("tags"),
        tags: tags::list(&state.core.db)
            .await?
            .into_iter()
            .map(|t| {
                let c = tag_class(t.id);
                (t, c)
            })
            .collect(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct TagAdminForm {
    name: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    description: String,
    is_curated: Option<String>,
}

pub async fn create_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<TagAdminForm>,
) -> AppResult<Response> {
    let r =
        tags::create_curated(&state.core.db, &user.ctx, &f.name, &f.color, &f.description).await;
    done(&state, "/admin/tags", r, "Tag added.")
}

pub async fn update_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<TagAdminForm>,
) -> AppResult<Response> {
    let r = tags::update(
        &state.core.db,
        &user.ctx,
        id,
        &f.name,
        &f.color,
        &f.description,
        checked(&f.is_curated),
    )
    .await;
    done(&state, "/admin/tags", r, "Tag saved.")
}

// ---------------------------------------------------------------------------
// Retention policies
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/retention.html")]
struct RetentionPage {
    l: Layout,
    policies: Vec<RetentionPolicy>,
    bases: Vec<(&'static str, &'static str)>,
}

pub async fn retention_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageRetention, "/admin/retention")
        .await?;
    let page = RetentionPage {
        l: Layout::new(&state, &user, flash, "Retention policies", "admin").admin_tab("retention"),
        policies: types::list_policies(&state.core.db).await?,
        bases: RetentionBasis::ALL
            .iter()
            .map(|b| (b.as_str(), b.label()))
            .collect(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct PolicyForm {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    retain_years: String,
    basis: String,
    #[serde(default)]
    legal_citation: String,
}

impl PolicyForm {
    fn spec(&self) -> Result<PolicySpec, CoreError> {
        Ok(PolicySpec {
            name: self.name.clone(),
            description: self.description.clone(),
            retain_years: opt_int(&self.retain_years, "Years")?,
            basis: RetentionBasis::parse(&self.basis)
                .ok_or_else(|| CoreError::validation("choose what retention is measured from"))?,
            legal_citation: self.legal_citation.clone(),
        })
    }
}

pub async fn create_policy(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<PolicyForm>,
) -> AppResult<Response> {
    let r = match f.spec() {
        Ok(spec) => types::create_policy(&state.core.db, &user.ctx, spec)
            .await
            .map(|_| ()),
        Err(e) => Err(e),
    };
    done(&state, "/admin/retention", r, "Retention policy added.")
}

pub async fn update_policy(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<PolicyForm>,
) -> AppResult<Response> {
    let r = match f.spec() {
        Ok(spec) => types::update_policy(&state.core.db, &user.ctx, id, spec).await,
        Err(e) => Err(e),
    };
    done(&state, "/admin/retention", r, "Retention policy saved.")
}

// ---------------------------------------------------------------------------
// Deleted documents
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/deleted.html")]
struct DeletedPage {
    l: Layout,
    docs: Vec<(DeletedDocument, String)>,
}

pub async fn deleted_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(
        &state,
        Capability::RestoreDeletedDocuments,
        "/admin/deleted",
    )
    .await?;
    let page = DeletedPage {
        l: Layout::new(&state, &user, flash, "Deleted documents", "admin").admin_tab("deleted"),
        docs: documents::list_deleted(&state.core, &user.ctx)
            .await?
            .into_iter()
            .map(|d| {
                let when = d
                    .deleted_at
                    .map(|t| fmt_time(state.tz, t))
                    .unwrap_or_default();
                (d, when)
            })
            .collect(),
    };
    Ok(Html(page.render()?))
}

pub async fn restore_deleted(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    match documents::restore_deleted(&state.core, &user.ctx, id).await {
        Ok(()) => Ok(crate::views::redirect_with(
            &state,
            &format!("/documents/{id}"),
            true,
            "Document restored.",
        )),
        Err(e) => done::<()>(&state, "/admin/deleted", Err(e), ""),
    }
}
