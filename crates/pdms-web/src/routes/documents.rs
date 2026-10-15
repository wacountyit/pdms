// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document pages: search, create, view, edit, versions, sharing, lifecycle.

use std::collections::BTreeMap;

use askama::Template;
use axum::Extension;
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use chrono::{NaiveDate, Utc};
use pdms_authz::{Capability, Permission, Subject, Visibility};
use pdms_core::directory::{self, Department, Group, Role, UserSummary};
use pdms_core::documents::fields::{FieldDef, FieldType};
use pdms_core::documents::search::{self, CustomFilter, DateField, DocumentFilter, SortField};
use pdms_core::documents::status::{DocumentStatus, next_statuses};
use pdms_core::documents::tags::{self, Tag};
use pdms_core::documents::types::{self, DocumentType, RetentionPolicy};
use pdms_core::documents::{self, MetadataInput, NewDocument, RestorePart, VersionKind};
use pdms_core::model::UserStatus;
use pdms_core::{CoreError, CoreResult};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::middleware::DeferredCsrf;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::upload;
use crate::views::{FlashMsg, Layout, done, fmt_date, fmt_time, human_size, iso_date, tag_class};

// ---------------------------------------------------------------------------
// Form helpers
// ---------------------------------------------------------------------------

/// Submitted (or prefilled) form values, for re-rendering a form after an error.
#[derive(Clone, Debug, Default)]
pub struct FormValues(pub BTreeMap<String, Vec<String>>);

impl FormValues {
    pub fn v(&self, k: &str) -> &str {
        self.0
            .get(k)
            .and_then(|v| v.first())
            .map_or("", String::as_str)
    }

    pub fn has(&self, k: &str, val: &str) -> bool {
        self.0.get(k).is_some_and(|v| v.iter().any(|x| x == val))
    }

    fn set(&mut self, k: &str, v: impl Into<String>) {
        self.0.insert(k.to_owned(), vec![v.into()]);
    }
}

pub struct Choice {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// One custom field, ready to render as a form control.
pub struct FieldInput {
    pub name: String,
    pub label: String,
    pub help: String,
    pub kind: &'static str,
    pub required: bool,
    pub value: String,
    pub checked: bool,
    pub choices: Vec<Choice>,
}

fn stored_as_strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Bool(true) => vec!["true".into()],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        _ => vec![],
    }
}

/// Values for custom fields: from a submitted form if present, otherwise
/// from stored values, otherwise (new documents) from definition defaults.
fn field_inputs(
    defs: &[FieldDef],
    type_id: Option<Uuid>,
    form: &FormValues,
    stored: Option<&Value>,
    users: &[UserSummary],
    departments: &[Department],
) -> Vec<FieldInput> {
    let Some(type_id) = type_id else {
        return Vec::new();
    };
    defs.iter()
        .filter(|d| d.applies_to(type_id) && !d.is_deprecated)
        .map(|d| {
            let name = format!("cf.{}", d.key);
            let values: Vec<String> = if let Some(v) = form.0.get(&name) {
                v.clone()
            } else if let Some(stored) = stored {
                stored
                    .get(&d.key)
                    .map(stored_as_strings)
                    .unwrap_or_default()
            } else {
                d.default_value
                    .as_ref()
                    .map(stored_as_strings)
                    .unwrap_or_default()
            };
            let choices = match d.field_type {
                FieldType::SingleSelect | FieldType::MultiSelect => d
                    .options
                    .iter()
                    .map(|o| Choice {
                        value: o.value.clone(),
                        label: o.label.clone(),
                        selected: values.contains(&o.value),
                    })
                    .collect(),
                FieldType::User => users
                    .iter()
                    .filter(|u| {
                        u.status == UserStatus::Active || values.contains(&u.id.to_string())
                    })
                    .map(|u| Choice {
                        value: u.id.to_string(),
                        label: format!("{} ({})", u.display_name, u.email),
                        selected: values.contains(&u.id.to_string()),
                    })
                    .collect(),
                FieldType::Department => departments
                    .iter()
                    .map(|x| Choice {
                        value: x.id.to_string(),
                        label: x.name.clone(),
                        selected: values.contains(&x.id.to_string()),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            FieldInput {
                label: d.label.clone(),
                help: d.help_text.clone(),
                kind: d.field_type.as_str(),
                required: d.required,
                checked: values.iter().any(|v| v == "true"),
                value: values.first().cloned().unwrap_or_default(),
                choices,
                name,
            }
        })
        .collect()
}

pub(crate) fn parse_date(label: &str, s: &str) -> CoreResult<Option<NaiveDate>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(Some)
        .map_err(|_| CoreError::validation(format!("{label} must be a date")))
}

fn parse_uuid(label: &str, s: &str) -> CoreResult<Uuid> {
    s.trim()
        .parse()
        .map_err(|_| CoreError::validation(format!("choose a {label}")))
}

fn metadata_from(form: &FormValues) -> CoreResult<MetadataInput> {
    let interval = form.v("review_interval_months").trim();
    Ok(MetadataInput {
        title: form.v("title").to_owned(),
        description: form.v("description").to_owned(),
        document_type_id: parse_uuid("document type", form.v("document_type_id"))?,
        department_id: parse_uuid("department", form.v("department_id"))?,
        execution_date: parse_date("Execution date", form.v("execution_date"))?,
        effective_date: parse_date("Effective date", form.v("effective_date"))?,
        last_review_date: parse_date("Last review date", form.v("last_review_date"))?,
        next_review_date: parse_date("Next review date", form.v("next_review_date"))?,
        renewal_date: parse_date("Renewal date", form.v("renewal_date"))?,
        expiration_date: parse_date("Expiration date", form.v("expiration_date"))?,
        review_interval_months: if interval.is_empty() {
            None
        } else {
            Some(interval.parse().map_err(|_| {
                CoreError::validation("review interval must be a whole number of months")
            })?)
        },
        custom: form
            .0
            .iter()
            .filter_map(|(k, v)| k.strip_prefix("cf.").map(|key| (key.to_owned(), v.clone())))
            .collect(),
    })
}

fn active_users(users: Vec<UserSummary>) -> Vec<UserSummary> {
    users
        .into_iter()
        .filter(|u| u.status == UserStatus::Active)
        .collect()
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    doc_type: String,
    #[serde(default)]
    department: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    mine: String,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    tag_mode: String,
    #[serde(default)]
    date_field: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    cf: String,
    #[serde(default)]
    cf_value: String,
    #[serde(default)]
    cf_min: String,
    #[serde(default)]
    cf_max: String,
    #[serde(default)]
    archived: String,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    page: Option<i64>,
}

impl ListQuery {
    pub fn sorts_by_field(&self, key: &str) -> bool {
        self.sort.strip_prefix("cf.") == Some(key)
    }

    fn filter(&self, me: Uuid) -> AppResult<DocumentFilter> {
        let uuid = |s: &str| -> AppResult<Option<Uuid>> {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse()
                    .map(Some)
                    .map_err(|_| AppError::BadRequest("invalid filter".into()))
            }
        };
        let date = |s: &str| parse_date("filter date", s).map_err(AppError::Core);
        let q = self.q.trim();
        let sort = if self.sort.is_empty() {
            if q.is_empty() {
                SortField::Updated
            } else {
                SortField::Relevance
            }
        } else {
            SortField::parse(&self.sort)
        };
        Ok(DocumentFilter {
            q: (!q.is_empty()).then(|| q.to_owned()),
            document_type_id: uuid(&self.doc_type)?,
            department_id: uuid(&self.department)?,
            status: DocumentStatus::parse(&self.status),
            owner_id: (self.mine == "1").then_some(me),
            tags: self.tag.iter().filter_map(|t| t.parse().ok()).collect(),
            all_tags: self.tag_mode == "all",
            date_field: DateField::parse(&self.date_field),
            date_from: date(&self.from)?,
            date_to: date(&self.to)?,
            custom: (!self.cf.is_empty()).then(|| CustomFilter {
                key: self.cf.clone(),
                value: Some(self.cf_value.clone()).filter(|s| !s.is_empty()),
                min: Some(self.cf_min.clone()).filter(|s| !s.is_empty()),
                max: Some(self.cf_max.clone()).filter(|s| !s.is_empty()),
            }),
            include_archived: self.archived == "1" || self.status == "archived",
            // Text and date sorts default to ascending; recency sorts to descending.
            descending: if self.dir.is_empty() {
                !matches!(
                    sort,
                    SortField::Title | SortField::ExternalId | SortField::Date(_)
                )
            } else {
                self.dir == "desc"
            },
            sort,
            page: self.page.unwrap_or(1),
            per_page: 50,
        })
    }

    /// The query string without paging, for export and page links.
    fn base_query(&self) -> String {
        let mut pairs: Vec<(&str, String)> = vec![];
        let mut add = |k: &'static str, v: &str| {
            if !v.is_empty() {
                pairs.push((k, v.to_owned()));
            }
        };
        add("q", &self.q);
        add("type", &self.doc_type);
        add("department", &self.department);
        add("status", &self.status);
        add("mine", &self.mine);
        add("tag_mode", &self.tag_mode);
        add("date_field", &self.date_field);
        add("from", &self.from);
        add("to", &self.to);
        add("cf", &self.cf);
        add("cf_value", &self.cf_value);
        add("cf_min", &self.cf_min);
        add("cf_max", &self.cf_max);
        add("archived", &self.archived);
        add("sort", &self.sort);
        add("dir", &self.dir);
        for t in &self.tag {
            pairs.push(("tag", t.clone()));
        }
        serde_urlencoded::to_string(pairs).unwrap_or_default()
    }
}

pub struct Row {
    pub id: Uuid,
    pub external_id: String,
    pub title: String,
    pub type_name: String,
    pub department_name: String,
    pub owner_name: String,
    pub status: &'static str,
    pub status_label: &'static str,
    pub next_review: String,
    pub renewal: String,
    pub expiration: String,
    pub confidential: bool,
    pub tags: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "documents/list.html")]
struct ListPage {
    l: Layout,
    q: ListQuery,
    query: String,
    rows: Vec<Row>,
    total: i64,
    page: i64,
    pages: i64,
    types: Vec<DocumentType>,
    departments: Vec<Department>,
    tags: Vec<Tag>,
    fields: Vec<FieldDef>,
    statuses: Vec<(&'static str, &'static str)>,
    date_fields: Vec<(&'static str, &'static str)>,
    can_create: bool,
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<ListQuery>,
) -> AppResult<Html<String>> {
    let filter = q.filter(user.session.user_id)?;
    let results = search::search(&state.core, &user.ctx, &filter).await?;
    let db = &state.core.db;
    let pages = ((results.total + results.per_page - 1) / results.per_page).max(1);
    let page = ListPage {
        l: Layout::new(&state, &user, flash, "Documents", "documents"),
        query: q.base_query(),
        rows: results
            .items
            .into_iter()
            .map(|d| Row {
                id: d.id,
                external_id: d.external_id,
                title: d.title,
                type_name: d.type_name,
                department_name: d.department_name,
                owner_name: d.owner_name,
                status: d.status.as_str(),
                status_label: d.status.label(),
                next_review: fmt_date(d.next_review_date),
                renewal: fmt_date(d.renewal_date),
                expiration: fmt_date(d.expiration_date),
                confidential: d.visibility == Visibility::Restricted,
                tags: d
                    .tags
                    .0
                    .into_iter()
                    .map(|t| (tag_class(t.id), t.name))
                    .collect(),
            })
            .collect(),
        total: results.total,
        page: results.page,
        pages,
        types: types::list_types(db).await?,
        departments: directory::list_departments(db).await?,
        tags: tags::list(db).await?,
        fields: pdms_core::documents::fields::list(db).await?,
        statuses: DocumentStatus::ALL
            .iter()
            .map(|s| (s.as_str(), s.label()))
            .collect(),
        date_fields: DateField::ALL
            .iter()
            .map(|d| (d.column(), d.label()))
            .collect(),
        can_create: !documents::access::creatable_departments(&state.core, &user.ctx)
            .await?
            .is_empty(),
        q,
    };
    Ok(Html(page.render()?))
}

pub async fn export_csv(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Response> {
    let mut filter = q.filter(user.session.user_id)?;
    filter.page = 1;
    let body = search::export_csv(&state.core, &user.ctx, &filter).await?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"documents-{stamp}.csv\""),
            ),
        ],
        body,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "documents/new.html")]
struct NewPage {
    l: Layout,
    form: FormValues,
    error: Option<String>,
    types: Vec<DocumentType>,
    departments: Vec<(Uuid, String)>,
    users: Vec<UserSummary>,
    fields: Vec<FieldInput>,
    max_upload_mb: u64,
}

async fn render_new(
    state: &AppState,
    user: &CurrentUser,
    flash: FlashMsg,
    form: FormValues,
    error: Option<String>,
) -> AppResult<Response> {
    let db = &state.core.db;
    let departments = documents::access::creatable_departments(&state.core, &user.ctx).await?;
    if departments.is_empty() {
        return Err(AppError::Core(CoreError::Forbidden(None)));
    }
    let types: Vec<DocumentType> = types::list_types(db)
        .await?
        .into_iter()
        .filter(|t| t.is_active)
        .collect();
    let all_users = directory::list_users(db, "", 5000).await?;
    let all_depts = directory::list_departments(db).await?;
    let type_id = form
        .v("document_type_id")
        .parse()
        .ok()
        .or(types.first().map(|t| t.id));
    let defs = pdms_core::documents::fields::list(db).await?;
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let page = NewPage {
        l: Layout::new(state, user, flash, "New document", "documents"),
        fields: field_inputs(&defs, type_id, &form, None, &all_users, &all_depts),
        users: active_users(all_users),
        types,
        departments,
        form,
        error,
        max_upload_mb: state.core.config.storage.max_upload_bytes / (1024 * 1024),
    };
    Ok((status, Html(page.render()?)).into_response())
}

pub async fn new_form(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Response> {
    let mut form = FormValues::default();
    form.set("owner_id", user.session.user_id.to_string());
    form.set("status", "active");
    let primary = directory::get_user(&state.core.db, user.session.user_id)
        .await?
        .memberships
        .into_iter()
        .find(|m| m.is_primary)
        .map(|m| m.department_id.to_string());
    if let Some(d) = primary {
        form.set("department_id", d);
    }
    render_new(&state, &user, flash, form, None).await
}

#[derive(Template)]
#[template(path = "documents/fields.html")]
struct FieldsPartial {
    fields: Vec<FieldInput>,
}

#[derive(Deserialize)]
pub struct FieldsQuery {
    document_type_id: Uuid,
    #[serde(default)]
    document: Option<Uuid>,
}

/// HTMX partial: the custom fields for a document type.
pub async fn fields_partial(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<FieldsQuery>,
) -> AppResult<Html<String>> {
    let db = &state.core.db;
    let stored = match q.document {
        Some(id) => Some(
            documents::for_edit(&state.core, &user.ctx, id)
                .await?
                .0
                .custom_fields,
        ),
        None => None,
    };
    let defs = pdms_core::documents::fields::list(db).await?;
    let users = directory::list_users(db, "", 5000).await?;
    let depts = directory::list_departments(db).await?;
    let page = FieldsPartial {
        fields: field_inputs(
            &defs,
            Some(q.document_type_id),
            &FormValues::default(),
            stored.as_ref(),
            &users,
            &depts,
        ),
    };
    Ok(Html(page.render()?))
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    deferred: Option<Extension<DeferredCsrf>>,
    multipart: Multipart,
) -> AppResult<Response> {
    let up = upload::read(&state, deferred.map(|e| e.0), multipart).await?;
    let form = FormValues(up.fields.clone());
    let result = async {
        let metadata = metadata_from(&form)?;
        let owner_id = parse_uuid("owner", form.v("owner_id"))?;
        let status = match form.v("status") {
            "draft" => DocumentStatus::Draft,
            _ => DocumentStatus::Active,
        };
        let tags: Vec<String> = form
            .v("tags")
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        documents::create(
            &state.core,
            &user.ctx,
            NewDocument {
                visibility: Default::default(),
                metadata,
                owner_id,
                status,
                tags,
                file: up.file,
                note: form.v("note").to_owned(),
            },
        )
        .await
    }
    .await;
    match result {
        Ok(id) => Ok(crate::views::redirect_with(
            &state,
            &format!("/documents/{id}"),
            true,
            "Document created.",
        )),
        Err(CoreError::Validation(m) | CoreError::Conflict(m)) => {
            let msg = format!("{m}. If you chose a file, choose it again.");
            render_new(&state, &user, FlashMsg(None), form, Some(msg)).await
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub struct CustomRow {
    pub label: String,
    pub value: String,
    pub note: &'static str,
}

pub struct VersionRow {
    pub version_no: i32,
    pub kind: &'static str,
    pub file_changed: bool,
    pub has_file: bool,
    pub file_name: String,
    pub size: String,
    pub sha: String,
    pub note: String,
    pub milestone: String,
    pub by: String,
    pub when: String,
}

pub struct GrantRow {
    pub id: Uuid,
    pub subject: String,
    pub permission: String,
    pub expires: String,
    pub reason: String,
}

#[derive(Template)]
#[template(path = "documents/show.html")]
struct ShowPage {
    l: Layout,
    d: documents::Document,
    status_class: &'static str,
    status_label: &'static str,
    dates: Vec<(&'static str, String)>,
    review_interval: String,
    custom: Vec<CustomRow>,
    tags: Vec<(Uuid, String, String)>,
    versions: Vec<VersionRow>,
    current_file: Option<VersionRow>,
    next: Vec<(&'static str, &'static str)>,
    can_edit: bool,
    can_delete: bool,
    can_manage: bool,
    can_retention: bool,
    can_timeline: bool,
    grants: Vec<GrantRow>,
    share_users: Vec<UserSummary>,
    share_groups: Vec<Group>,
    share_departments: Vec<Department>,
    share_roles: Vec<Role>,
    permissions: Vec<&'static str>,
    policies: Vec<RetentionPolicy>,
    deletion_block: Option<String>,
    created: String,
    updated: String,
    max_upload_mb: u64,
    open_tasks: Vec<TaskRow>,
    task_history: Vec<TaskRow>,
    missed: String,
    missed_count: usize,
    rule_sections: Vec<RuleSection>,
    opts: Option<crate::routes::schedule::RecipientOptions>,
    events: Vec<(&'static str, &'static str)>,
    new_rule: crate::routes::schedule::RuleFormView,
    wf: crate::routes::workflow::WorkflowView,
    collab: crate::routes::collab::CollabView,
}

pub struct TaskRow {
    pub id: Uuid,
    pub kind: &'static str,
    pub kind_label: &'static str,
    pub due: String,
    pub status: &'static str,
    pub status_label: &'static str,
    pub detail: String,
    pub note: String,
    pub can_start: bool,
    pub can_complete: bool,
    pub can_approve: bool,
}

pub struct RuleSection {
    pub event_label: &'static str,
    pub source_label: &'static str,
    pub is_document: bool,
    pub rules: Vec<crate::routes::schedule::RuleView>,
}

impl ShowPage {
    fn policy_selected(&self, id: &Uuid) -> bool {
        self.d.retention_policy_id == Some(*id)
    }
}

fn version_row(state: &AppState, v: documents::VersionInfo) -> VersionRow {
    VersionRow {
        version_no: v.version_no,
        kind: v.kind.label(),
        file_changed: v.file_changed,
        has_file: v.blob_sha256.is_some(),
        file_name: v.file_name.unwrap_or_default(),
        size: human_size(v.size_bytes),
        sha: v
            .blob_sha256
            .map(|s| s[..12].to_owned())
            .unwrap_or_default(),
        note: v.note,
        milestone: fmt_date(v.milestone_date),
        by: v.created_by_name.unwrap_or_else(|| "System".into()),
        when: fmt_time(state.tz, v.created_at),
    }
}

pub async fn show(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    let core = &state.core;
    let view = documents::view(core, &user.ctx, id).await?;
    let d = view.document;
    let perms = view.permissions;
    let can_manage = perms.contains(Permission::ManagePermissions);
    let can_edit = perms.contains(Permission::Edit);
    let names: std::collections::HashMap<String, String> = {
        let users = directory::list_users(&core.db, "", 5000).await?;
        let depts = directory::list_departments(&core.db).await?;
        users
            .into_iter()
            .map(|u| (u.id.to_string(), u.display_name))
            .chain(depts.into_iter().map(|x| (x.id.to_string(), x.name)))
            .collect()
    };
    let lookup = |s: &str| names.get(s).cloned();
    let mut custom = Vec::new();
    if let Some(obj) = d.custom_fields.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort_by_key(|k| {
            view.fields
                .iter()
                .position(|f| &f.key == *k)
                .unwrap_or(usize::MAX)
        });
        for k in keys {
            let def = view.fields.iter().find(|f| &f.key == k);
            custom.push(CustomRow {
                label: def.map_or_else(|| k.clone(), |f| f.label.clone()),
                value: pdms_core::documents::fields::display(def, &obj[k], &lookup),
                note: match def {
                    Some(f) if f.is_deprecated => "deprecated field",
                    Some(f) if !f.applies_to(d.document_type_id) => "from a previous document type",
                    None => "field no longer defined",
                    _ => "",
                },
            });
        }
    }
    let versions = documents::versions(core, &user.ctx, id).await?;
    let is_owner = d.owner_id == user.session.user_id;
    let (grants, share_users, share_groups, share_departments, share_roles) = if can_manage {
        (
            documents::grants(core, &user.ctx, id)
                .await?
                .into_iter()
                .map(|g| GrantRow {
                    id: g.id,
                    subject: format!("{} ({})", g.subject_label, g.subject_kind),
                    permission: g.permission,
                    expires: g
                        .expires_at
                        .map(|t| fmt_time(state.tz, t))
                        .unwrap_or_default(),
                    reason: g.reason,
                })
                .collect(),
            active_users(directory::list_users(&core.db, "", 5000).await?),
            directory::list_groups(&core.db).await?,
            directory::list_departments(&core.db).await?,
            directory::list_roles(&core.db).await?,
        )
    } else {
        Default::default()
    };
    let can_retention = user.can(Capability::ManageRetention);
    let sched = crate::routes::schedule::doc_schedule(&state, &user, &d, can_edit).await?;
    let can_approve = perms.contains(Permission::Approve);
    let task_row = |t: &pdms_core::reviews::Task| {
        use pdms_core::reviews::TaskStatus;
        let mut detail = Vec::new();
        if let Some(by) = &t.initiated_by {
            detail.push(format!("started by {by}"));
        }
        if let (Some(by), Some(on)) = (&t.completed_by, t.completed_on) {
            detail.push(format!("completed {} by {by}", fmt_date(Some(on))));
        }
        if let (Some(by), Some(at)) = (&t.approved_by, t.approved_at) {
            detail.push(format!("approved {} by {by}", fmt_time(state.tz, at)));
        }
        TaskRow {
            id: t.id,
            kind: t.kind.as_str(),
            kind_label: t.kind.label(),
            due: fmt_date(Some(t.due_date)),
            status: t.status.as_str(),
            status_label: t.status.label(),
            detail: detail.join("; "),
            note: t.outcome_note.clone(),
            can_start: can_edit
                && matches!(t.status, TaskStatus::Open | TaskStatus::Overdue)
                && t.initiated_at.is_none(),
            can_complete: can_edit && t.status.is_open(),
            can_approve: can_approve && t.status == TaskStatus::Completed,
        }
    };
    let deletion_block = if perms.contains(Permission::Delete) {
        documents::deletion_eligibility(core, id).await?.reason()
    } else {
        None
    };
    let wf = crate::routes::workflow::panel_view(&state, &user, id, d.document_type_id, can_edit)
        .await?;
    let collab = crate::routes::collab::collab_view(&state, &user, id, can_edit).await?;
    let page = ShowPage {
        l: Layout::new(&state, &user, flash, &d.title, "documents"),
        status_class: d.status.as_str(),
        status_label: d.status.label(),
        dates: vec![
            ("Execution date", fmt_date(d.execution_date)),
            ("Effective date", fmt_date(d.effective_date)),
            ("Last review", fmt_date(d.last_review_date)),
            ("Next review", fmt_date(d.next_review_date)),
            ("Renewal date", fmt_date(d.renewal_date)),
            (
                "Expiration date",
                if d.expiration_date.is_some() {
                    fmt_date(d.expiration_date)
                } else {
                    "None (stays active)".into()
                },
            ),
        ],
        review_interval: match (d.review_interval_months, d.type_review_interval_months) {
            (Some(m), _) => format!("Every {m} months"),
            (None, Some(m)) => format!("Every {m} months (document type default)"),
            (None, None) => "No recurring review".into(),
        },
        custom,
        tags: d
            .tags
            .iter()
            .map(|t| (t.id, tag_class(t.id), t.name.clone()))
            .collect(),
        current_file: view.current_file.map(|v| version_row(&state, v)),
        versions: versions
            .into_iter()
            .map(|v| version_row(&state, v))
            .collect(),
        // Documents enter review by being submitted (the workflow section),
        // and leave it through the workflow while one is in progress.
        next: next_statuses(d.status)
            .into_iter()
            .filter(|(_, p)| perms.contains(*p) || (*p == Permission::Edit && can_edit))
            .filter(|(s, _)| *s != DocumentStatus::InReview)
            .filter(|_| wf.active.is_none())
            .map(|(s, _)| (s.as_str(), s.label()))
            .collect(),
        can_edit,
        can_delete: perms.contains(Permission::Delete),
        can_manage,
        can_retention,
        can_timeline: user.can(Capability::ViewAudit) || is_owner || can_manage,
        grants,
        share_users,
        share_groups,
        share_departments,
        share_roles,
        permissions: Permission::ALL
            .iter()
            .filter(|p| **p != Permission::Create)
            .map(|p| p.as_str())
            .collect(),
        policies: if can_retention {
            types::list_policies(&core.db).await?
        } else {
            Vec::new()
        },
        deletion_block,
        created: format!(
            "{} by {}",
            fmt_time(state.tz, d.created_at),
            d.created_by_name.clone().unwrap_or_else(|| "System".into())
        ),
        updated: fmt_time(state.tz, d.updated_at),
        max_upload_mb: core.config.storage.max_upload_bytes / (1024 * 1024),
        open_tasks: sched.open_tasks.iter().map(&task_row).collect(),
        task_history: sched.history.iter().map(&task_row).collect(),
        missed_count: sched.missed.len(),
        missed: sched
            .missed
            .iter()
            .map(|m| fmt_date(Some(*m)))
            .collect::<Vec<_>>()
            .join(", "),
        rule_sections: sched
            .rules
            .into_iter()
            .map(|(e, source, rules)| RuleSection {
                event_label: e.label(),
                source_label: source.label(),
                is_document: source == pdms_core::notifications::rules::RuleSource::Document,
                rules,
            })
            .collect(),
        opts: sched.opts,
        events: pdms_core::notifications::rules::DueEvent::ALL
            .iter()
            .map(|e| (e.as_str(), e.label()))
            .collect(),
        new_rule: crate::routes::schedule::RuleFormView::blank(
            "doc-new",
            pdms_core::notifications::rules::DueEvent::Review,
        ),
        wf,
        collab,
        d,
    };
    Ok(Html(page.render()?))
}

// ---------------------------------------------------------------------------
// Edit
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "documents/edit.html")]
struct EditPage {
    l: Layout,
    id: Uuid,
    external_id: String,
    form: FormValues,
    error: Option<String>,
    types: Vec<DocumentType>,
    departments: Vec<Department>,
    fields: Vec<FieldInput>,
}

async fn render_edit(
    state: &AppState,
    user: &CurrentUser,
    id: Uuid,
    form: Option<FormValues>,
    error: Option<String>,
) -> AppResult<Response> {
    let db = &state.core.db;
    let (d, defs) = documents::for_edit(&state.core, &user.ctx, id).await?;
    let form = form.unwrap_or_else(|| {
        let mut f = FormValues::default();
        f.set("title", d.title.clone());
        f.set("description", d.description.clone());
        f.set("document_type_id", d.document_type_id.to_string());
        f.set("department_id", d.department_id.to_string());
        f.set("execution_date", iso_date(d.execution_date));
        f.set("effective_date", iso_date(d.effective_date));
        f.set("last_review_date", iso_date(d.last_review_date));
        f.set("next_review_date", iso_date(d.next_review_date));
        f.set("renewal_date", iso_date(d.renewal_date));
        f.set("expiration_date", iso_date(d.expiration_date));
        f.set(
            "review_interval_months",
            d.review_interval_months
                .map(|m| m.to_string())
                .unwrap_or_default(),
        );
        f.set("row_version", d.row_version.to_string());
        f
    });
    let users = directory::list_users(db, "", 5000).await?;
    let departments = directory::list_departments(db).await?;
    let type_id = form.v("document_type_id").parse().ok();
    let stored_for_fields = if error.is_some() {
        None
    } else {
        Some(&d.custom_fields)
    };
    let page = EditPage {
        l: Layout::new(
            state,
            user,
            FlashMsg(None),
            &format!("Edit {}", d.title),
            "documents",
        ),
        id,
        external_id: d.external_id.clone(),
        fields: field_inputs(
            &defs,
            type_id,
            &form,
            stored_for_fields,
            &users,
            &departments,
        ),
        types: types::list_types(db)
            .await?
            .into_iter()
            .filter(|t| t.is_active || t.id == d.document_type_id)
            .collect(),
        departments: departments
            .into_iter()
            .filter(|x| x.is_active || x.id == d.department_id)
            .collect(),
        form,
        error,
    };
    let status = if page.error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    Ok((status, Html(page.render()?)).into_response())
}

pub async fn edit_form(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    render_edit(&state, &user, id, None, None).await
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(raw): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let mut form = FormValues::default();
    for (k, v) in raw {
        if k != "_csrf" {
            form.0.entry(k).or_default().push(v);
        }
    }
    let result = async {
        let input = metadata_from(&form)?;
        let row_version: i32 = form
            .v("row_version")
            .parse()
            .map_err(|_| CoreError::validation("reload the page"))?;
        documents::update(&state.core, &user.ctx, id, input, row_version).await
    }
    .await;
    match result {
        Ok(()) => Ok(crate::views::redirect_with(
            &state,
            &format!("/documents/{id}"),
            true,
            "Changes saved.",
        )),
        Err(CoreError::Validation(m) | CoreError::Conflict(m)) => {
            render_edit(&state, &user, id, Some(form), Some(m)).await
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle, ownership, access, retention, tags
// ---------------------------------------------------------------------------

fn back(id: Uuid) -> String {
    format!("/documents/{id}")
}

#[derive(Deserialize)]
pub struct StatusForm {
    status: String,
    #[serde(default)]
    note: String,
}

pub async fn change_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<StatusForm>,
) -> AppResult<Response> {
    let to = DocumentStatus::parse(&f.status)
        .ok_or_else(|| AppError::BadRequest("unknown status".into()))?;
    let r = documents::change_status(&state.core, &user.ctx, id, to, &f.note).await;
    done(
        &state,
        &back(id),
        r,
        &format!("Status changed to {}.", to.label()),
    )
}

#[derive(Deserialize)]
pub struct OwnerForm {
    owner_id: Uuid,
    #[serde(default)]
    reason: String,
}

pub async fn transfer_owner(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<OwnerForm>,
) -> AppResult<Response> {
    let r = documents::transfer_owner(&state.core, &user.ctx, id, f.owner_id, &f.reason).await;
    done(&state, &back(id), r, "Owner changed.")
}

#[derive(Deserialize)]
pub struct VisibilityForm {
    #[serde(default)]
    visibility: String,
}

pub async fn set_visibility(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<VisibilityForm>,
) -> AppResult<Response> {
    let r = match f.visibility.parse::<Visibility>() {
        Ok(v) => documents::set_visibility(&state.core, &user.ctx, id, v).await,
        Err(_) => Err(CoreError::validation("choose who can see the document")),
    };
    done(&state, &back(id), r, "Visibility saved.")
}

#[derive(Deserialize)]
pub struct RetentionForm {
    legal_hold: Option<String>,
    #[serde(default)]
    retention_policy_id: String,
    #[serde(default)]
    reason: String,
}

pub async fn retention(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<RetentionForm>,
) -> AppResult<Response> {
    let policy = if f.retention_policy_id.is_empty() {
        None
    } else {
        Some(
            f.retention_policy_id
                .parse()
                .map_err(|_| AppError::BadRequest("invalid policy".into()))?,
        )
    };
    let r = documents::set_retention(
        &state.core,
        &user.ctx,
        id,
        f.legal_hold.as_deref() == Some("true"),
        policy,
        &f.reason,
    )
    .await;
    done(&state, &back(id), r, "Retention settings saved.")
}

#[derive(Deserialize)]
pub struct TagForm {
    name: String,
}

pub async fn add_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<TagForm>,
) -> AppResult<Response> {
    let mut result = Ok(());
    for name in f.name.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        result = documents::add_tag(&state.core, &user.ctx, id, name).await;
        if result.is_err() {
            break;
        }
    }
    done(&state, &back(id), result, "Tag added.")
}

pub async fn remove_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, tag)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = documents::remove_tag(&state.core, &user.ctx, id, tag).await;
    done(&state, &back(id), r, "Tag removed.")
}

#[derive(Deserialize)]
pub struct ShareForm {
    subject: String,
    permission: String,
    #[serde(default)]
    expires_on: String,
    #[serde(default)]
    reason: String,
}

pub async fn share(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<ShareForm>,
) -> AppResult<Response> {
    let (kind, sid) = f
        .subject
        .split_once(':')
        .ok_or_else(|| AppError::BadRequest("invalid selection".into()))?;
    let sid: Uuid = sid
        .parse()
        .map_err(|_| AppError::BadRequest("invalid selection".into()))?;
    let subject = match kind {
        "user" => Subject::User(sid),
        "group" => Subject::Group(sid),
        "department" => Subject::Department(sid),
        "role" => Subject::Role(sid),
        _ => return Err(AppError::BadRequest("invalid selection".into())),
    };
    let permission: Permission = f
        .permission
        .parse()
        .map_err(|_| AppError::BadRequest("invalid permission".into()))?;
    let expires_at = match parse_date("Expiry", &f.expires_on) {
        Ok(Some(d)) => d
            .succ_opt()
            .and_then(|n| n.and_hms_opt(0, 0, 0))
            .and_then(|dt| dt.and_local_timezone(state.tz).earliest())
            .map(|t| t.with_timezone(&Utc)),
        Ok(None) => None,
        Err(e) => return done::<()>(&state, &back(id), Err(e), ""),
    };
    if f.reason.trim().is_empty() {
        return done::<()>(
            &state,
            &back(id),
            Err(CoreError::validation("enter a reason for sharing")),
            "",
        );
    }
    let r = documents::share(
        &state.core,
        &user.ctx,
        id,
        subject,
        permission,
        expires_at,
        &f.reason,
    )
    .await;
    done(&state, &back(id), r, "Access granted.")
}

pub async fn unshare(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, grant)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = documents::unshare(&state.core, &user.ctx, id, grant).await;
    done(&state, &back(id), r, "Access removed.")
}

#[derive(Deserialize)]
pub struct ReasonForm {
    #[serde(default)]
    reason: String,
}

pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<ReasonForm>,
) -> AppResult<Response> {
    match documents::delete(&state.core, &user.ctx, id, &f.reason).await {
        Ok(()) => Ok(crate::views::redirect_with(
            &state,
            "/documents",
            true,
            "Document deleted.",
        )),
        Err(e) => done::<()>(&state, &back(id), Err(e), ""),
    }
}

// ---------------------------------------------------------------------------
// Versions and files
// ---------------------------------------------------------------------------

pub async fn add_version(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    deferred: Option<Extension<DeferredCsrf>>,
    multipart: Multipart,
) -> AppResult<Response> {
    let up = upload::read(&state, deferred.map(|e| e.0), multipart).await?;
    let kind = VersionKind::parse_upload(up.get("kind")).unwrap_or(VersionKind::Revision);
    let note = up.get("note").to_owned();
    let milestone = match parse_date("Milestone date", up.get("milestone_date")) {
        Ok(d) => d,
        Err(e) => return done::<()>(&state, &back(id), Err(e), ""),
    };
    let Some(file) = up.file else {
        return done::<()>(
            &state,
            &back(id),
            Err(CoreError::validation("choose a file to upload")),
            "",
        );
    };
    let r = documents::add_version(&state.core, &user.ctx, id, file, kind, &note, milestone).await;
    done(&state, &back(id), r, "New version uploaded.")
}

#[derive(Deserialize)]
pub struct RestoreForm {
    part: String,
}

pub async fn restore_version(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, n)): Path<(Uuid, i32)>,
    Form(f): Form<RestoreForm>,
) -> AppResult<Response> {
    let part = if f.part == "file" {
        RestorePart::File
    } else {
        RestorePart::Metadata
    };
    let r = documents::restore_version(&state.core, &user.ctx, id, n, part).await;
    done(&state, &back(id), r, &format!("Restored from version {n}."))
}

/// `filename=` needs an ASCII fallback; `filename*=` carries the real name.
pub(crate) fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = url::form_urlencoded::byte_serialize(name.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

async fn download_inner(
    state: &AppState,
    user: &CurrentUser,
    id: Uuid,
    version: Option<i32>,
) -> AppResult<Response> {
    let f = documents::open_file(&state.core, &user.ctx, id, version).await?;
    let mut res = Response::new(Body::from_stream(f.stream));
    let h = res.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, k: header::HeaderName, v: String| {
        if let Ok(v) = header::HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    };
    set(h, header::CONTENT_TYPE, f.media_type);
    set(h, header::CONTENT_LENGTH, f.size.to_string());
    set(
        h,
        header::CONTENT_DISPOSITION,
        content_disposition(&f.file_name),
    );
    set(h, header::CACHE_CONTROL, "private, no-store".into());
    Ok(res)
}

pub async fn download_current(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    download_inner(&state, &user, id, None).await
}

pub async fn download_version(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, n)): Path<(Uuid, i32)>,
) -> AppResult<Response> {
    download_inner(&state, &user, id, Some(n)).await
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "documents/timeline.html")]
struct TimelinePage {
    l: Layout,
    id: Uuid,
    entries: Vec<crate::routes::audit::EntryView>,
    tz: String,
}

pub async fn timeline(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    let entries = documents::timeline(&state.core, &user.ctx, id).await?;
    let show_ip = user.can(Capability::ViewAudit);
    let title = entries
        .iter()
        .find_map(|e| e.event.object_label.clone())
        .unwrap_or_else(|| "Document".into());
    let page = TimelinePage {
        l: Layout::new(
            &state,
            &user,
            flash,
            &format!("History: {title}"),
            "documents",
        ),
        id,
        entries: entries
            .into_iter()
            .map(|e| crate::routes::audit::entry_view_with(state.tz, e, show_ip))
            .collect(),
        tz: state.tz.name().to_owned(),
    };
    Ok(Html(page.render()?))
}

/// Tag colors as CSS classes (inline styles are blocked by the CSP).
pub async fn tag_colors(State(state): State<AppState>) -> AppResult<Response> {
    let mut css = String::new();
    for t in tags::list(&state.core.db).await? {
        let hex = t.color.trim_start_matches('#');
        let channel = |i: usize| {
            u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0
        };
        let lin = |c: f64| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let lum = 0.2126 * lin(channel(0)) + 0.7152 * lin(channel(2)) + 0.0722 * lin(channel(4));
        let fg = if lum > 0.179 { "#1b1f24" } else { "#ffffff" };
        css.push_str(&format!(
            ".tag-{}{{background:#{hex};color:{fg};border-color:#{hex}}}\n",
            t.id.simple()
        ));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "private, max-age=60"),
        ],
        css,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_is_safe() {
        let h = content_disposition("Résumé \"final\".pdf");
        assert!(
            h.starts_with("attachment; filename=\"R_sum_ _final_.pdf\""),
            "{h}"
        );
        assert!(
            h.contains("filename*=UTF-8''R%C3%A9sum%C3%A9%20%22final%22.pdf"),
            "{h}"
        );
    }
}
