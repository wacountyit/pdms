// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Records request packages and reports.

use askama::Template;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use pdms_authz::Capability;
use pdms_core::documents::search::DocumentFilter;
use pdms_core::exports::{self, PackageInfo, PackageRequest, Versions};
use pdms_core::reports::{self, Report, ReportInfo, ReportParams};
use pdms_core::{CoreError, CoreResult};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::routes::documents::parse_date;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, human_size};

// ---------------------------------------------------------------------------
// Export packages
// ---------------------------------------------------------------------------

pub struct PackageRow {
    pub p: PackageInfo,
    pub when: String,
    pub size: String,
}

pub struct Candidate {
    pub id: Uuid,
    pub external_id: String,
    pub title: String,
    pub department: String,
    pub status: &'static str,
    pub confidential: bool,
}

#[derive(Template)]
#[template(path = "exports.html")]
struct ExportsPage {
    l: Layout,
    packages: Vec<PackageRow>,
    can_build: bool,
    q: String,
    searched: bool,
    candidates: Vec<Candidate>,
    fields: Vec<(&'static str, &'static str, bool)>,
}

#[derive(Deserialize, Default)]
pub struct ExportsQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    search: String,
}

pub async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<ExportsQuery>,
) -> AppResult<Html<String>> {
    if !user.can(Capability::BuildExportPackages) {
        user.require(&state, Capability::ViewAudit, "/exports")
            .await?;
    }
    let can_build = user.can(Capability::BuildExportPackages);
    let searched = can_build && (q.search == "1" || !q.q.trim().is_empty());
    let candidates = if searched {
        let f = DocumentFilter {
            q: Some(q.q.clone()).filter(|s| !s.trim().is_empty()),
            include_archived: true,
            ..DocumentFilter::default()
        };
        exports::candidates(&state.core, &user.ctx, &f)
            .await?
            .into_iter()
            .map(|d| Candidate {
                id: d.id,
                external_id: d.external_id,
                title: d.title,
                department: d.department_name,
                status: d.status.label(),
                confidential: d.visibility == pdms_authz::Visibility::Restricted,
            })
            .collect()
    } else {
        Vec::new()
    };
    let defaults = exports::default_fields();
    let page = ExportsPage {
        l: Layout::new(&state, &user, flash, "Records requests", "exports"),
        packages: exports::list(&state.core, &user.ctx)
            .await?
            .into_iter()
            .map(|p| PackageRow {
                when: fmt_time(state.tz, p.created_at),
                size: human_size(Some(p.size_bytes)),
                p,
            })
            .collect(),
        can_build,
        q: q.q,
        searched,
        candidates,
        fields: exports::FIELDS
            .iter()
            .map(|(k, l)| (*k, *l, defaults.get(k).copied().unwrap_or(true)))
            .collect(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct BuildForm {
    request_reference: String,
    #[serde(default)]
    document: Vec<String>,
    #[serde(default)]
    field: Vec<String>,
    #[serde(default)]
    versions: String,
    include_audit: Option<String>,
    #[serde(default)]
    note: String,
}

pub async fn build(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<BuildForm>,
) -> AppResult<Response> {
    let r: CoreResult<Uuid> = async {
        let ids = f
            .document
            .iter()
            .map(|s| {
                s.parse::<Uuid>()
                    .map_err(|_| CoreError::validation("invalid document"))
            })
            .collect::<CoreResult<Vec<_>>>()?;
        exports::build(
            &state.core,
            &user.ctx,
            PackageRequest {
                request_reference: f.request_reference,
                document_ids: ids,
                fields: f.field,
                versions: Versions::parse(&f.versions).unwrap_or(Versions::Current),
                include_audit: f.include_audit.as_deref() == Some("1"),
                note: f.note,
            },
        )
        .await
    }
    .await;
    done(
        &state,
        "/exports",
        r,
        "Package built. Download it below; the download is recorded.",
    )
}

pub async fn download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let (p, stream) = exports::open(&state.core, &user.ctx, id).await?;
    let mut res = Body::from_stream(stream).into_response();
    let h = res.headers_mut();
    for (k, v) in [
        (header::CONTENT_TYPE, "application/zip".to_owned()),
        (header::CONTENT_LENGTH, p.size_bytes.to_string()),
        (
            header::CONTENT_DISPOSITION,
            crate::routes::documents::content_disposition(&p.file_name()),
        ),
        (header::CACHE_CONTROL, "private, no-store".to_owned()),
    ] {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    }
    Ok(res)
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "reports/index.html")]
struct ReportsIndex {
    l: Layout,
    reports: Vec<&'static ReportInfo>,
}

pub async fn reports_index(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let page = ReportsIndex {
        l: Layout::new(&state, &user, flash, "Reports", "reports"),
        reports: reports::available(&user.ctx),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize, Default)]
pub struct ReportQuery {
    #[serde(default)]
    days: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    department: String,
}

impl ReportQuery {
    fn params(&self) -> CoreResult<ReportParams> {
        Ok(ReportParams {
            days: if self.days.trim().is_empty() {
                None
            } else {
                Some(
                    self.days
                        .trim()
                        .parse()
                        .map_err(|_| CoreError::validation("days must be a whole number"))?,
                )
            },
            from: parse_date("From", &self.from)?,
            to: parse_date("To", &self.to)?,
            department_id: if self.department.is_empty() {
                None
            } else {
                Some(
                    self.department
                        .parse()
                        .map_err(|_| CoreError::validation("invalid department"))?,
                )
            },
        })
    }

    fn query_string(&self) -> String {
        serde_urlencoded::to_string([
            ("days", self.days.as_str()),
            ("from", self.from.as_str()),
            ("to", self.to.as_str()),
            ("department", self.department.as_str()),
        ])
        .unwrap_or_default()
    }
}

#[derive(Template)]
#[template(path = "reports/show.html")]
struct ReportPage {
    l: Layout,
    info: &'static ReportInfo,
    r: Report,
    q: ReportQuery,
    csv_link: String,
    departments: Vec<pdms_core::directory::Department>,
}

pub async fn report(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(key): Path<String>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Response> {
    if key.ends_with(".csv") {
        return report_csv(State(state), user, Path(key), Query(q)).await;
    }
    let info = reports::info(&key).ok_or(CoreError::NotFound)?;
    if let Some(cap) = info.requires {
        user.require(&state, cap, &format!("/reports/{key}"))
            .await?;
    }
    let params = match q.params() {
        Ok(p) => p,
        Err(e) => return done::<()>(&state, &format!("/reports/{key}"), Err(e), ""),
    };
    let r = reports::run(&state.core, &user.ctx, &key, &params).await?;
    let page = ReportPage {
        l: Layout::new(&state, &user, flash, info.title, "reports"),
        csv_link: format!("/reports/{key}.csv?{}", q.query_string()),
        departments: pdms_core::directory::list_departments(&state.core.db).await?,
        info,
        r,
        q,
    };
    Ok(Html(page.render()?).into_response())
}

async fn report_csv(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(file): Path<String>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Response> {
    let key = file.strip_suffix(".csv").ok_or(CoreError::NotFound)?;
    let info = reports::info(key).ok_or(CoreError::NotFound)?;
    if let Some(cap) = info.requires {
        user.require(&state, cap, &format!("/reports/{key}"))
            .await?;
    }
    let csv = reports::export_csv(&state.core, &user.ctx, key, &q.params()?).await?;
    let mut res = csv.into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{key}-{}.csv\"",
        chrono::Utc::now().format("%Y%m%d")
    )) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}
