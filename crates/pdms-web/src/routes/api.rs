// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The REST API (`/api/v1`), documented with OpenAPI at `/api/v1/openapi.json`
//! and readable at `/api/docs`.
//!
//! Requests authenticate with `Authorization: Bearer pdms_…` (an API token).
//! A token acts as its user, limited to its scopes; every check still goes
//! through the central authorization engine. Every request is logged, and
//! each token is rate limited.

use std::time::Instant;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Utc};
use pdms_core::api_tokens::{self, RequestRecord, TokenAuth};
use pdms_core::documents::search::{self, DocumentFilter};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::reports::{self, ReportParams};
use pdms_core::{ClientInfo, CoreError, appointments, audit_log, calendar, schedule};
use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{IntoParams, Modify, OpenApi, ToSchema};
use uuid::Uuid;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every error answer.
#[derive(Serialize, ToSchema)]
pub struct ErrorOut {
    /// Machine-readable: `unauthorized`, `insufficient_scope`, `forbidden`,
    /// `not_found`, `invalid`, `conflict`, `rate_limited`, `internal`.
    pub error: String,
    pub message: String,
}

pub struct ApiError(StatusCode, &'static str, String);

impl ApiError {
    fn scope(scope: &str) -> Self {
        ApiError(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            format!("this token lacks the {scope} scope"),
        )
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::NotFound => ApiError(StatusCode::NOT_FOUND, "not_found", "not found".into()),
            CoreError::Forbidden(_) => ApiError(
                StatusCode::FORBIDDEN,
                "forbidden",
                "you do not have permission for this".into(),
            ),
            CoreError::Unauthenticated => ApiError(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required".into(),
            ),
            CoreError::Validation(m) => ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid", m),
            CoreError::Conflict(m) => ApiError(StatusCode::CONFLICT, "conflict", m),
            other => {
                tracing::error!(error = %other, "API request failed");
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "something went wrong; the error has been logged".into(),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = (
            self.0,
            Json(ErrorOut {
                error: self.1.to_owned(),
                message: self.2,
            }),
        )
            .into_response();
        if self.0 == StatusCode::UNAUTHORIZED {
            res.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"pdms\""),
            );
        }
        res
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ---------------------------------------------------------------------------
// Authentication, rate limiting, and the request log
// ---------------------------------------------------------------------------

/// Authenticate every `/api/v1` request (except the OpenAPI document), rate
/// limit it, and log it.
pub async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    if !state.core.config.api.enabled {
        return ApiError(
            StatusCode::NOT_FOUND,
            "not_found",
            "the API is turned off".into(),
        )
        .into_response();
    }
    let started = Instant::now();
    let method = req.method().to_string();
    // Inside the nested router the URI has `/api/v1` stripped; log the original.
    let uri = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map_or_else(|| req.uri().clone(), |o| o.0.clone());
    let path = uri
        .path_and_query()
        .map_or_else(|| uri.path().to_owned(), |p| p.as_str().to_owned());
    let client = req
        .extensions()
        .get::<ClientInfo>()
        .cloned()
        .unwrap_or_default();
    let public = uri.path() == "/api/v1/openapi.json";
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_owned());
    let mut who = (None, None);
    let res = match presented {
        _ if public => next.run(req).await,
        None => ApiError(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "send an API token: Authorization: Bearer pdms_…".into(),
        )
        .into_response(),
        Some(token) => match api_tokens::authenticate(&state.core, &token, &client).await {
            Ok(Some(auth)) => {
                who = (Some(auth.token_id), Some(auth.user_id));
                if state.api_limiter.check_key(&auth.token_id).is_err() {
                    ApiError(
                        StatusCode::TOO_MANY_REQUESTS,
                        "rate_limited",
                        "too many requests for this token; slow down".into(),
                    )
                    .into_response()
                } else {
                    req.extensions_mut().insert(auth);
                    next.run(req).await
                }
            }
            Ok(None) => ApiError(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "the token is unknown, expired, or revoked, or its user is not active".into(),
            )
            .into_response(),
            Err(e) => ApiError::from(e).into_response(),
        },
    };
    let record = RequestRecord {
        token_id: who.0,
        user_id: who.1,
        method,
        path,
        status: res.status().as_u16(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        client,
    };
    if let Err(e) = api_tokens::log_request(&state.core.db, record).await {
        tracing::error!(error = %e, "could not log an API request");
    }
    res
}

/// The authenticated token (inserted by [`guard`]).
pub struct Api(pub TokenAuth);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Api {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<TokenAuth>()
            .cloned()
            .map(Api)
            .ok_or_else(|| ApiError::from(CoreError::Unauthenticated))
    }
}

impl Api {
    fn need(&self, scope: &str) -> ApiResult<()> {
        if self.0.has_scope(scope) {
            Ok(())
        } else {
            Err(ApiError::scope(scope))
        }
    }

    fn ctx(&self) -> &pdms_core::Ctx {
        &self.0.ctx
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
pub struct MeOut {
    pub user_id: Uuid,
    pub display_name: String,
    pub token_id: Uuid,
    pub scopes: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TypeOut {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub id_prefix: String,
    pub review_interval_months: Option<i32>,
    pub requires_legal_review: bool,
    pub requires_signatures: bool,
}

#[derive(Serialize, ToSchema)]
pub struct DocumentSummaryOut {
    pub id: Uuid,
    /// Stable external ID, e.g. `PAG-2026-000001`.
    pub external_id: String,
    pub title: String,
    pub document_type: String,
    pub department: String,
    pub owner: String,
    /// `draft`, `in_review`, `active`, `renewal_due`, `overdue`, `expired`, `superseded`, `archived`.
    pub status: String,
    pub execution_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub last_review_date: Option<NaiveDate>,
    pub next_review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub review_interval_months: Option<i32>,
    pub tags: Vec<String>,
    #[schema(value_type = Object)]
    pub custom_fields: serde_json::Value,
    pub updated_at: DateTime<Utc>,
    pub url: String,
}

#[derive(Serialize, ToSchema)]
pub struct DocumentListOut {
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub items: Vec<DocumentSummaryOut>,
}

#[derive(Serialize, ToSchema)]
pub struct DocumentOut {
    #[serde(flatten)]
    pub summary: DocumentSummaryOut,
    pub description: String,
    pub department_id: Uuid,
    pub owner_id: Uuid,
    pub document_type_id: Uuid,
    pub is_confidential: bool,
    pub legal_hold: bool,
    pub current_version: i32,
    pub created_at: DateTime<Utc>,
    /// Your permissions on this document.
    pub permissions: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct VersionOut {
    pub version_no: i32,
    /// `initial`, `revision`, `amendment`, `signed_copy`, `metadata`, `restore`.
    pub kind: String,
    pub file_changed: bool,
    pub file_name: Option<String>,
    pub media_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub note: String,
    pub milestone_date: Option<NaiveDate>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct EventOut {
    pub date: NaiveDate,
    /// `review`, `renewal`, `expiration`, `term_end`, `reappointment`.
    pub kind: String,
    pub label: String,
    pub title: String,
    /// Document external ID, or the board's name.
    pub reference: String,
    pub department: String,
    pub document_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    pub position_id: Option<Uuid>,
    pub projected: bool,
}

#[derive(Serialize, ToSchema)]
pub struct PositionOut {
    pub id: Uuid,
    pub name: String,
    pub body_name: String,
    pub department: Option<String>,
    pub appointing_authority: String,
    pub term_months: Option<i32>,
    pub seats: i32,
    pub filled: i64,
    pub is_active: bool,
}

#[derive(Serialize, ToSchema)]
pub struct AppointmentOut {
    pub id: Uuid,
    pub person_name: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub reappointment_due_date: Option<NaiveDate>,
    /// `active` or `ended`.
    pub status: String,
    pub ended_on: Option<NaiveDate>,
    pub end_reason: String,
    pub appointed_by: String,
    pub appointment_reference: String,
    pub requirements_met: usize,
    pub requirements_total: usize,
}

#[derive(Serialize, ToSchema)]
pub struct PositionDetailOut {
    #[serde(flatten)]
    pub position: PositionOut,
    pub appointments: Vec<AppointmentOut>,
}

#[derive(Serialize, ToSchema)]
pub struct ReportInfoOut {
    pub key: String,
    pub title: String,
    pub description: String,
}

#[derive(Serialize, ToSchema)]
pub struct ReportOut {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Serialize, ToSchema)]
pub struct AuditEntryOut {
    pub seq: i64,
    pub occurred_at: DateTime<Utc>,
    pub category: String,
    pub action: String,
    pub outcome: String,
    pub actor: Option<String>,
    pub actor_user_id: Option<Uuid>,
    pub object_type: String,
    pub object_id: Option<String>,
    pub object_label: Option<String>,
    pub document_id: Option<Uuid>,
    pub entry_hash: String,
}

#[derive(Serialize, ToSchema)]
pub struct CommentOut {
    pub id: Uuid,
    /// The comment this replies to.
    pub parent_id: Option<Uuid>,
    pub author: String,
    /// Empty when deleted.
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// Mentioned users' names.
    pub mentions: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TextOut {
    pub version: i32,
    /// `done`, `empty`, `unsupported`, `failed`.
    pub status: String,
    /// `pdf_text`, `docx`, `plain`, `ocr`, `none`.
    pub method: String,
    pub characters: i32,
    pub extracted_at: DateTime<Utc>,
    /// The first 2,000 characters.
    pub preview: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateDocumentIn {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// The type's key (e.g. `contract`) or ID.
    pub document_type: String,
    pub department_id: Uuid,
    /// Defaults to you.
    pub owner_id: Option<Uuid>,
    /// `draft` (default) or `active`.
    #[serde(default)]
    pub status: Option<String>,
    pub execution_date: Option<NaiveDate>,
    pub effective_date: Option<NaiveDate>,
    pub last_review_date: Option<NaiveDate>,
    pub next_review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub review_interval_months: Option<i32>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedOut {
    pub id: Uuid,
    pub external_id: String,
    pub url: String,
}

#[derive(Deserialize, ToSchema)]
pub struct StatusIn {
    pub status: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize, IntoParams)]
pub struct DocumentQuery {
    /// Full-text search.
    q: Option<String>,
    /// Document type ID.
    document_type_id: Option<Uuid>,
    department_id: Option<Uuid>,
    status: Option<String>,
    owner_id: Option<Uuid>,
    /// Tag ID (repeat for several).
    #[serde(default)]
    tag: Vec<Uuid>,
    page: Option<i64>,
    /// 1–200, default 50.
    per_page: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
pub struct RangeQuery {
    /// Default: today.
    from: Option<NaiveDate>,
    /// Default: 90 days after `from`.
    to: Option<NaiveDate>,
    department_id: Option<Uuid>,
}

#[derive(Deserialize, IntoParams)]
pub struct ReportQuery {
    days: Option<i64>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    department_id: Option<Uuid>,
}

#[derive(Deserialize, IntoParams)]
pub struct AuditQuery {
    /// Action or action prefix, e.g. `document.` or `auth.login_failed`.
    action: Option<String>,
    document_id: Option<Uuid>,
    actor_user_id: Option<Uuid>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    /// Return entries with `seq` below this (for paging backwards).
    before_seq: Option<i64>,
    /// 1–500, default 100.
    limit: Option<i64>,
}

fn doc_url(state: &AppState, id: Uuid) -> String {
    state
        .core
        .config
        .server
        .public_url
        .join(&format!("documents/{id}"))
        .map(|u| u.to_string())
        .unwrap_or_default()
}

fn summary_out(state: &AppState, d: search::DocumentSummary) -> DocumentSummaryOut {
    DocumentSummaryOut {
        url: doc_url(state, d.id),
        id: d.id,
        external_id: d.external_id,
        title: d.title,
        document_type: d.type_name,
        department: d.department_name,
        owner: d.owner_name,
        status: d.status.as_str().to_owned(),
        execution_date: d.execution_date,
        effective_date: d.effective_date,
        last_review_date: d.last_review_date,
        next_review_date: d.next_review_date,
        renewal_date: d.renewal_date,
        expiration_date: d.expiration_date,
        review_interval_months: d.review_interval_months,
        tags: d.tags.0.into_iter().map(|t| t.name).collect(),
        custom_fields: d.custom_fields,
        updated_at: d.updated_at,
    }
}

/// Accept a UUID or an external ID.
async fn resolve(state: &AppState, id: &str) -> ApiResult<Uuid> {
    if let Ok(u) = id.parse::<Uuid>() {
        return Ok(u);
    }
    documents::id_by_external_id(&state.core, id)
        .await?
        .ok_or_else(|| CoreError::NotFound.into())
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// Who this token acts as.
#[utoipa::path(get, path = "/api/v1/me", tag = "account",
    responses((status = 200, body = MeOut), (status = 401, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn me(api: Api) -> ApiResult<Json<MeOut>> {
    Ok(Json(MeOut {
        user_id: api.0.user_id,
        display_name: api.0.ctx.actor.display.clone().unwrap_or_default(),
        token_id: api.0.token_id,
        scopes: api.0.scopes.clone(),
    }))
}

/// Document types. Scope `documents:read`.
#[utoipa::path(get, path = "/api/v1/document-types", tag = "documents",
    responses((status = 200, body = [TypeOut]), (status = 403, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn document_types(
    State(state): State<AppState>,
    api: Api,
) -> ApiResult<Json<Vec<TypeOut>>> {
    api.need("documents:read")?;
    Ok(Json(
        types::list_types(&state.core.db)
            .await?
            .into_iter()
            .filter(|t| t.is_active)
            .map(|t| TypeOut {
                id: t.id,
                key: t.key,
                name: t.name,
                id_prefix: t.id_prefix,
                review_interval_months: t.review_interval_months,
                requires_legal_review: t.requires_legal_review,
                requires_signatures: t.requires_signatures,
            })
            .collect(),
    ))
}

/// Search documents you can view. Scope `documents:read`.
#[utoipa::path(get, path = "/api/v1/documents", tag = "documents", params(DocumentQuery),
    responses((status = 200, body = DocumentListOut), (status = 403, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn list_documents(
    State(state): State<AppState>,
    api: Api,
    axum_extra::extract::Query(q): axum_extra::extract::Query<DocumentQuery>,
) -> ApiResult<Json<DocumentListOut>> {
    api.need("documents:read")?;
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            DocumentStatus::parse(s)
                .ok_or_else(|| CoreError::validation(format!("unknown status {s:?}")))?,
        ),
        None => None,
    };
    let f = DocumentFilter {
        q: q.q,
        document_type_id: q.document_type_id,
        department_id: q.department_id,
        status,
        owner_id: q.owner_id,
        tags: q.tag,
        include_archived: status == Some(DocumentStatus::Archived),
        page: q.page.unwrap_or(1),
        per_page: q.per_page.unwrap_or(50),
        ..DocumentFilter::default()
    };
    let r = search::search(&state.core, api.ctx(), &f).await?;
    Ok(Json(DocumentListOut {
        total: r.total,
        page: r.page,
        per_page: r.per_page,
        items: r
            .items
            .into_iter()
            .map(|d| summary_out(&state, d))
            .collect(),
    }))
}

/// One document, by ID or external ID. Scope `documents:read`. Audited as a view.
#[utoipa::path(get, path = "/api/v1/documents/{id}", tag = "documents",
    params(("id" = String, Path, description = "UUID or external ID, e.g. PAG-2026-000001")),
    responses((status = 200, body = DocumentOut), (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn get_document(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<String>,
) -> ApiResult<Json<DocumentOut>> {
    api.need("documents:read")?;
    let id = resolve(&state, &id).await?;
    let v = documents::view(&state.core, api.ctx(), id).await?;
    let d = v.document;
    Ok(Json(DocumentOut {
        summary: DocumentSummaryOut {
            url: doc_url(&state, d.id),
            id: d.id,
            external_id: d.external_id.clone(),
            title: d.title.clone(),
            document_type: d.type_name.clone(),
            department: d.department_name.clone(),
            owner: d.owner_name.clone(),
            status: d.status.as_str().to_owned(),
            execution_date: d.execution_date,
            effective_date: d.effective_date,
            last_review_date: d.last_review_date,
            next_review_date: d.next_review_date,
            renewal_date: d.renewal_date,
            expiration_date: d.expiration_date,
            review_interval_months: d.review_interval_months.or(d.type_review_interval_months),
            tags: d.tags.iter().map(|t| t.name.clone()).collect(),
            custom_fields: d.custom_fields.clone(),
            updated_at: d.updated_at,
        },
        description: d.description,
        department_id: d.department_id,
        owner_id: d.owner_id,
        document_type_id: d.document_type_id,
        is_confidential: d.visibility == pdms_authz::Visibility::Restricted,
        legal_hold: d.legal_hold,
        current_version: d.current_version,
        created_at: d.created_at,
        permissions: v
            .permissions
            .iter()
            .map(|p| p.as_str().to_owned())
            .collect(),
    }))
}

/// Version history. Scope `documents:read`.
#[utoipa::path(get, path = "/api/v1/documents/{id}/versions", tag = "documents",
    params(("id" = String, Path, description = "UUID or external ID")),
    responses((status = 200, body = [VersionOut]), (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn versions(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<VersionOut>>> {
    api.need("documents:read")?;
    let id = resolve(&state, &id).await?;
    Ok(Json(
        documents::versions(&state.core, api.ctx(), id)
            .await?
            .into_iter()
            .map(|v| VersionOut {
                version_no: v.version_no,
                kind: v.kind.as_str().to_owned(),
                file_changed: v.file_changed,
                file_name: v.file_name,
                media_type: v.media_type,
                size_bytes: v.size_bytes,
                sha256: v.blob_sha256,
                note: v.note,
                milestone_date: v.milestone_date,
                created_by: v.created_by_name,
                created_at: v.created_at,
            })
            .collect(),
    ))
}

async fn file_response(
    state: &AppState,
    api: &Api,
    id: &str,
    version: Option<i32>,
) -> ApiResult<Response> {
    api.need("documents:read")?;
    let id = resolve(state, id).await?;
    let f = documents::open_file(&state.core, api.ctx(), id, version).await?;
    let mut res = Response::new(Body::from_stream(f.stream));
    let h = res.headers_mut();
    for (k, v) in [
        (header::CONTENT_TYPE, f.media_type),
        (header::CONTENT_LENGTH, f.size.to_string()),
        (
            header::CONTENT_DISPOSITION,
            crate::routes::documents::content_disposition(&f.file_name),
        ),
        (header::CACHE_CONTROL, "private, no-store".into()),
        (
            header::HeaderName::from_static("x-pdms-version"),
            f.version_no.to_string(),
        ),
    ] {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    }
    Ok(res)
}

/// The current file. Scope `documents:read`. Audited as a download.
#[utoipa::path(get, path = "/api/v1/documents/{id}/file", tag = "documents",
    params(("id" = String, Path, description = "UUID or external ID")),
    responses((status = 200, description = "The file", content_type = "application/octet-stream"),
              (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn current_file(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    file_response(&state, &api, &id, None).await
}

/// One version's file. Scope `documents:read`. Audited as a download.
#[utoipa::path(get, path = "/api/v1/documents/{id}/versions/{version}/file", tag = "documents",
    params(("id" = String, Path, description = "UUID or external ID"), ("version" = i32, Path)),
    responses((status = 200, description = "The file", content_type = "application/octet-stream"),
              (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn version_file(
    State(state): State<AppState>,
    api: Api,
    Path((id, version)): Path<(String, i32)>,
) -> ApiResult<Response> {
    file_response(&state, &api, &id, Some(version)).await
}

/// Comments on a document, oldest first (replies follow their comment).
/// Scope `documents:read`.
#[utoipa::path(get, path = "/api/v1/documents/{id}/comments", tag = "documents",
    params(("id" = String, Path, description = "UUID or external ID")),
    responses((status = 200, body = [CommentOut]), (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn comments(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<CommentOut>>> {
    api.need("documents:read")?;
    let id = resolve(&state, &id).await?;
    let out = |c: &pdms_core::comments::Comment| CommentOut {
        id: c.id,
        parent_id: c.parent_id,
        author: c.author.clone(),
        body: c.body.clone(),
        created_at: c.created_at,
        edited_at: c.edited_at,
        deleted_at: c.deleted_at,
        mentions: c.mentions.iter().map(|m| m.1.clone()).collect(),
    };
    Ok(Json(
        pdms_core::comments::list(&state.core, api.ctx(), id)
            .await?
            .iter()
            .flat_map(|t| std::iter::once(out(&t.comment)).chain(t.replies.iter().map(out)))
            .collect(),
    ))
}

/// Text read from the document's latest file (embedded text or OCR), if any.
/// Scope `documents:read`.
#[utoipa::path(get, path = "/api/v1/documents/{id}/text", tag = "documents",
    params(("id" = String, Path, description = "UUID or external ID")),
    responses((status = 200, body = TextOut), (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn text(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<String>,
) -> ApiResult<Json<TextOut>> {
    api.need("documents:read")?;
    let id = resolve(&state, &id).await?;
    let t = pdms_core::ocr::latest_text(&state.core, api.ctx(), id)
        .await?
        .ok_or(CoreError::NotFound)?;
    Ok(Json(TextOut {
        version: t.version_no,
        status: t.status,
        method: t.method,
        characters: t.char_count,
        extracted_at: t.extracted_at,
        preview: t.preview,
    }))
}

/// Create a document record (no file). Scope `documents:write`; needs
/// `create` in the department.
#[utoipa::path(post, path = "/api/v1/documents", tag = "documents", request_body = CreateDocumentIn,
    responses((status = 201, body = CreatedOut), (status = 403, body = ErrorOut), (status = 422, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn create_document(
    State(state): State<AppState>,
    api: Api,
    Json(b): Json<CreateDocumentIn>,
) -> ApiResult<(StatusCode, Json<CreatedOut>)> {
    api.need("documents:write")?;
    let type_id = match b.document_type.parse::<Uuid>() {
        Ok(id) => id,
        Err(_) => {
            types::type_by_key(&state.core.db, &b.document_type)
                .await
                .map_err(|_| CoreError::validation("unknown document type"))?
                .id
        }
    };
    let status = match b.status.as_deref().unwrap_or("draft") {
        "draft" => DocumentStatus::Draft,
        "active" => DocumentStatus::Active,
        _ => return Err(CoreError::validation("status must be draft or active").into()),
    };
    let id = documents::create(
        &state.core,
        api.ctx(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: b.title,
                description: b.description,
                document_type_id: type_id,
                department_id: b.department_id,
                execution_date: b.execution_date,
                effective_date: b.effective_date,
                last_review_date: b.last_review_date,
                next_review_date: b.next_review_date,
                renewal_date: b.renewal_date,
                expiration_date: b.expiration_date,
                review_interval_months: b.review_interval_months,
                ..Default::default()
            },
            owner_id: b.owner_id.unwrap_or(api.0.user_id),
            status,
            tags: b.tags,
            file: None,
            note: "created through the API".into(),
        },
    )
    .await?;
    let d = documents::view(&state.core, api.ctx(), id).await?.document;
    Ok((
        StatusCode::CREATED,
        Json(CreatedOut {
            id,
            external_id: d.external_id,
            url: doc_url(&state, id),
        }),
    ))
}

/// Change a document's status (validated transitions). Scope `documents:write`.
#[utoipa::path(post, path = "/api/v1/documents/{id}/status", tag = "documents", request_body = StatusIn,
    params(("id" = String, Path, description = "UUID or external ID")),
    responses((status = 204), (status = 403, body = ErrorOut), (status = 409, body = ErrorOut), (status = 422, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn change_status(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<String>,
    Json(b): Json<StatusIn>,
) -> ApiResult<StatusCode> {
    api.need("documents:write")?;
    let id = resolve(&state, &id).await?;
    let to = DocumentStatus::parse(&b.status)
        .ok_or_else(|| CoreError::validation(format!("unknown status {:?}", b.status)))?;
    documents::change_status(&state.core, api.ctx(), id, to, &b.note).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Dates in a range: reviews, renewals, expirations, appointment term ends,
/// and reappointment decisions you can see. Scope `calendar:read`.
#[utoipa::path(get, path = "/api/v1/calendar", tag = "calendar", params(RangeQuery),
    responses((status = 200, body = [EventOut]), (status = 403, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn calendar_events(
    State(state): State<AppState>,
    api: Api,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Json<Vec<EventOut>>> {
    api.need("calendar:read")?;
    let today = schedule::local_today(Utc::now(), state.tz);
    let from = q.from.unwrap_or(today);
    let to = q.to.unwrap_or(from + chrono::Duration::days(90));
    if to < from || (to - from).num_days() > 3660 {
        return Err(CoreError::validation("choose a range of up to ten years").into());
    }
    let f = DocumentFilter {
        department_id: q.department_id,
        ..DocumentFilter::default()
    };
    let ev = calendar::events(&state.core, api.ctx(), &f, from, to, true).await?;
    Ok(Json(
        ev.into_iter()
            .map(|e| {
                let (appointment_id, position_id) = match e.subject {
                    calendar::EventSubject::Appointment {
                        appointment_id,
                        position_id,
                    } => (Some(appointment_id), Some(position_id)),
                    calendar::EventSubject::Document(_) => (None, None),
                };
                EventOut {
                    date: e.date,
                    kind: e.kind.as_str().to_owned(),
                    label: e.label().to_owned(),
                    document_id: e.document_id(),
                    title: e.title,
                    reference: e.external_id,
                    department: e.department,
                    appointment_id,
                    position_id,
                    projected: e.projected,
                }
            })
            .collect(),
    ))
}

fn position_out(p: appointments::Position) -> PositionOut {
    PositionOut {
        id: p.id,
        name: p.name,
        body_name: p.body_name,
        department: p.department_name,
        appointing_authority: p.appointing_authority,
        term_months: p.term_months,
        seats: p.seats,
        filled: p.filled,
        is_active: p.is_active,
    }
}

/// Positions you can see. Scope `appointments:read`.
#[utoipa::path(get, path = "/api/v1/positions", tag = "appointments",
    responses((status = 200, body = [PositionOut]), (status = 403, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn positions(
    State(state): State<AppState>,
    api: Api,
) -> ApiResult<Json<Vec<PositionOut>>> {
    api.need("appointments:read")?;
    Ok(Json(
        appointments::list_positions(&state.core, api.ctx(), true)
            .await?
            .into_iter()
            .map(position_out)
            .collect(),
    ))
}

/// A position with every appointment (current and past). Scope `appointments:read`.
#[utoipa::path(get, path = "/api/v1/positions/{id}", tag = "appointments",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = PositionDetailOut), (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn position(
    State(state): State<AppState>,
    api: Api,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PositionDetailOut>> {
    api.need("appointments:read")?;
    let d = appointments::position(&state.core, api.ctx(), id).await?;
    Ok(Json(PositionDetailOut {
        appointments: d
            .appointments
            .iter()
            .map(|a| AppointmentOut {
                id: a.id,
                person_name: a.person_name.clone(),
                start_date: a.start_date,
                end_date: a.end_date,
                reappointment_due_date: a.reappointment_due_date,
                status: match a.status {
                    appointments::AppointmentStatus::Active => "active".into(),
                    appointments::AppointmentStatus::Ended => "ended".into(),
                },
                ended_on: a.ended_on,
                end_reason: a.end_reason.clone(),
                appointed_by: a.appointed_by.clone(),
                appointment_reference: a.appointment_reference.clone(),
                requirements_met: a.requirements_met(),
                requirements_total: a.requirements.len(),
            })
            .collect(),
        position: position_out(d.position),
    }))
}

/// Reports you may run. Scope `reports:read`.
#[utoipa::path(get, path = "/api/v1/reports", tag = "reports",
    responses((status = 200, body = [ReportInfoOut]), (status = 403, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn report_list(api: Api) -> ApiResult<Json<Vec<ReportInfoOut>>> {
    api.need("reports:read")?;
    Ok(Json(
        reports::available(api.ctx())
            .into_iter()
            .map(|r| ReportInfoOut {
                key: r.key.into(),
                title: r.title.into(),
                description: r.description.into(),
            })
            .collect(),
    ))
}

/// Run a report. Scope `reports:read` (plus the report's own requirement).
#[utoipa::path(get, path = "/api/v1/reports/{key}", tag = "reports",
    params(("key" = String, Path), ReportQuery),
    responses((status = 200, body = ReportOut), (status = 403, body = ErrorOut), (status = 404, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn report(
    State(state): State<AppState>,
    api: Api,
    Path(key): Path<String>,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<ReportOut>> {
    api.need("reports:read")?;
    let r = reports::run(
        &state.core,
        api.ctx(),
        &key,
        &ReportParams {
            days: q.days,
            from: q.from,
            to: q.to,
            department_id: q.department_id,
        },
    )
    .await?;
    Ok(Json(ReportOut {
        key: r.key.into(),
        title: r.title.into(),
        subtitle: r.subtitle,
        columns: r.columns,
        rows: r.rows,
    }))
}

/// Search the audit log (newest first). Scope `audit:read`, and the user must
/// be an auditor or administrator. The search itself is audited.
#[utoipa::path(get, path = "/api/v1/audit", tag = "audit", params(AuditQuery),
    responses((status = 200, body = [AuditEntryOut]), (status = 403, body = ErrorOut)),
    security(("bearer" = [])))]
pub async fn audit(
    State(state): State<AppState>,
    api: Api,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Vec<AuditEntryOut>>> {
    api.need("audit:read")?;
    let f = audit_log::AuditFilter {
        action_prefix: q.action,
        document_id: q.document_id,
        actor_user_id: q.actor_user_id,
        from: q.from,
        to: q.to,
        before_seq: q.before_seq,
        ..Default::default()
    };
    let rows = audit_log::search(&state.core.db, api.ctx(), &f, q.limit.unwrap_or(100)).await?;
    Ok(Json(
        rows.into_iter()
            .map(|s| AuditEntryOut {
                seq: s.seq,
                occurred_at: s.event.occurred_at,
                category: s.event.category.as_str().to_owned(),
                action: s.event.action,
                outcome: s.event.outcome.as_str().to_owned(),
                actor: s.event.actor.display,
                actor_user_id: s.event.actor.user_id,
                object_type: s.event.object_type,
                object_id: s.event.object_id,
                object_label: s.event.object_label,
                document_id: s.event.document_id,
                entry_hash: hex_encode(&s.entry_hash),
            })
            .collect(),
    ))
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------------

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("pdms_ API token")
                    .description(Some(
                        "Create a token under Account → API tokens. It acts as you, limited to its scopes.",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "PDMS API",
        version = "1",
        description = "Documents, dates, appointments, reports, and the audit log. Every request is authorized as the token's user, limited to the token's scopes, and logged."
    ),
    paths(
        me, document_types, list_documents, get_document, versions, current_file, version_file, comments, text,
        create_document, change_status, calendar_events, positions, position, report_list, report, audit
    ),
    components(schemas(
        ErrorOut, MeOut, TypeOut, DocumentSummaryOut, DocumentListOut, DocumentOut, VersionOut, EventOut,
        PositionOut, AppointmentOut, PositionDetailOut, ReportInfoOut, ReportOut, AuditEntryOut,
        CreateDocumentIn, CreatedOut, StatusIn, CommentOut, TextOut
    )),
    modifiers(&BearerAuth),
    tags(
        (name = "account", description = "The token and its user"),
        (name = "documents", description = "Documents, versions, and files"),
        (name = "calendar", description = "Due dates"),
        (name = "appointments", description = "Boards, positions, and appointments"),
        (name = "reports", description = "Reports (the same as in the web app)"),
        (name = "audit", description = "The audit log"),
    )
)]
pub struct ApiDoc;

/// The OpenAPI document (public: describes the API, contains no data).
pub async fn openapi_json(State(state): State<AppState>) -> Response {
    let mut doc = ApiDoc::openapi();
    doc.servers = Some(vec![utoipa::openapi::Server::new(
        state
            .core
            .config
            .server
            .public_url
            .as_str()
            .trim_end_matches('/'),
    )]);
    Json(doc).into_response()
}

/// A readable page listing every endpoint, generated from the OpenAPI document.
pub async fn docs_page(State(state): State<AppState>) -> Html<String> {
    let doc = ApiDoc::openapi();
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut rows = String::new();
    for (path, item) in &doc.paths.paths {
        for (method, op) in [
            ("GET", &item.get),
            ("POST", &item.post),
            ("PUT", &item.put),
            ("PATCH", &item.patch),
            ("DELETE", &item.delete),
        ] {
            let Some(op) = op else { continue };
            let summary = op
                .summary
                .clone()
                .or_else(|| op.description.clone())
                .unwrap_or_default();
            rows.push_str(&format!(
                "<tr><td><code>{method}</code></td><td><code>{}</code></td><td>{}</td></tr>",
                esc(path),
                esc(&summary)
            ));
        }
    }
    let scopes: String = api_tokens::SCOPES
        .iter()
        .map(|(k, v)| format!("<li><code>{}</code>: {}</li>", esc(k), esc(v)))
        .collect();
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>API – {org}</title><link rel=\"stylesheet\" href=\"/static/app.css\"></head><body><main id=\"main\">\
         <h1>{org} API</h1>\
         <p>Base URL: <code>{base}/api/v1</code>. Authenticate with <code>Authorization: Bearer pdms_…</code>. \
         Create tokens in the web app under <a href=\"/account/tokens\">API tokens</a>. The machine-readable \
         description is at <a href=\"/api/v1/openapi.json\"><code>/api/v1/openapi.json</code></a> (OpenAPI 3.1).</p>\
         <h2>Scopes</h2><ul>{scopes}</ul>\
         <p>A token can never do more than its user: every request is checked against the user's current access. \
         Requests are logged; each token is limited to {rpm} requests a minute. Errors are JSON: \
         <code>{{\"error\": \"forbidden\", \"message\": \"…\"}}</code>.</p>\
         <h2>Endpoints</h2><table><thead><tr><th scope=\"col\">Method</th><th scope=\"col\">Path</th><th scope=\"col\">What</th></tr></thead><tbody>{rows}</tbody></table>\
         <h2>Webhooks</h2><p>Administrators can send events (for example <code>document.created</code>, \
         <code>workflow.approved</code>, <code>appointment.reappointed</code>) to an HTTPS endpoint. Each POST carries \
         <code>X-PDMS-Event</code>, <code>X-PDMS-Delivery</code>, and <code>X-PDMS-Signature: t=&lt;unix time&gt;,v1=&lt;hex HMAC-SHA256 of \"t.body\"&gt;</code>. \
         Answer with any 2xx; failures are retried with backoff.</p>\
         </main></body></html>",
        org = esc(state.org_name()),
        base = esc(state
            .core
            .config
            .server
            .public_url
            .as_str()
            .trim_end_matches('/')),
        rpm = state.core.config.api.requests_per_minute,
    ))
}
