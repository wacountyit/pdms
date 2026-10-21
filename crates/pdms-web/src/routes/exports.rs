// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Records request packages and reports.

use askama::Template;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use chrono::{Datelike, Duration, Months, NaiveDate, Utc};
use pdms_authz::Capability;
use pdms_authz::Permission;
use pdms_core::appointments::{self, SeatState};
use pdms_core::calendar;
use pdms_core::documents::DocumentStatus;
use pdms_core::documents::search::{self, DocumentFilter};
use pdms_core::exports::{self, PackageInfo, PackageRequest, Versions};
use pdms_core::reports::{self, Report, ReportInfo, ReportParams};
use pdms_core::{CoreError, CoreResult};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::error::AppResult;
use crate::routes::documents::parse_date;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_date, fmt_time, human_size};

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
    pub visibility: &'static str,
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
                visibility: d.visibility.label(),
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
    o: Overview,
}

/// Documents scanned for the overview's type and department counts.
const OVERVIEW_DOC_CAP: i64 = 20_000;

pub struct Tile {
    pub value: usize,
    pub sub: String,
}

pub struct BoardRow {
    pub label: &'static str,
    pub count: i64,
}

pub struct Boards {
    pub rows: Vec<BoardRow>,
}

/// Everything the "County overview" shows, built only from visibility-filtered
/// sources.
pub struct Overview {
    pub months: u32,
    pub departments: Vec<(Uuid, String, bool)>,
    pub capped: bool,
    pub as_of: String,
    pub documents: Tile,
    pub overdue: Tile,
    pub due90: Tile,
    pub in_review: Tile,
    pub due_chart: crate::charts::VBars,
    pub due_total: usize,
    pub due_table: String,
    pub status_chart: crate::charts::Stacked,
    pub overdue_chart: Option<crate::charts::HBars>,
    pub boards: Option<Boards>,
}

#[derive(Deserialize, Default)]
pub struct OverviewQuery {
    months: Option<u32>,
    #[serde(default)]
    department: String,
}

fn plural_days(n: i64) -> String {
    if n == 1 {
        "1 day".to_owned()
    } else {
        format!("{n} days")
    }
}

fn add_months(d: NaiveDate, n: u32) -> NaiveDate {
    d.checked_add_months(Months::new(n)).unwrap_or(d)
}

async fn build_overview(
    state: &AppState,
    user: &CurrentUser,
    q: &OverviewQuery,
) -> CoreResult<Overview> {
    let core = &state.core;
    let ctx = &user.ctx;
    let months = match q.months {
        Some(12) => 12,
        _ => 6,
    };
    let today = pdms_core::schedule::local_today(Utc::now(), core.config.timezone());

    // Everything the viewer can see: the source of the department options and
    // of the type and department counts.
    let visible = search::all(
        core,
        ctx,
        &DocumentFilter::default(),
        Permission::View,
        OVERVIEW_DOC_CAP,
    )
    .await?;
    let capped = visible.len() as i64 >= OVERVIEW_DOC_CAP;
    let mut departments: Vec<(Uuid, String)> = visible
        .iter()
        .map(|d| (d.department_id, d.department_name.clone()))
        .collect();
    departments.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then(a.0.cmp(&b.0))
    });
    departments.dedup_by_key(|d| d.0);
    // A department the viewer cannot see is ignored.
    let department = Uuid::parse_str(q.department.trim())
        .ok()
        .filter(|id| departments.iter().any(|d| d.0 == *id));
    let filter = DocumentFilter {
        department_id: department,
        ..Default::default()
    };

    let scoped: Vec<&search::DocumentSummary> = visible
        .iter()
        .filter(|d| department.is_none_or(|id| d.department_id == id))
        .collect();
    let types: BTreeSet<&str> = scoped.iter().map(|d| d.type_name.as_str()).collect();
    let depts: BTreeSet<Uuid> = scoped.iter().map(|d| d.department_id).collect();

    let counts = search::status_counts(core, ctx, &filter).await?;
    let count_of = |s: DocumentStatus| {
        counts
            .iter()
            .filter(|(st, _)| *st == s)
            .map(|(_, n)| *n)
            .sum::<i64>()
    };
    let documents_total: i64 = counts
        .iter()
        .filter(|(s, _)| !matches!(s, DocumentStatus::Archived | DocumentStatus::Superseded))
        .map(|(_, n)| *n)
        .sum();

    let dash = calendar::dashboard(core, ctx, &filter, 1).await?;
    let oldest = dash.overdue.iter().map(|o| o.days_overdue).max();
    let due90_events =
        calendar::events(core, ctx, &filter, today, today + Duration::days(90), false).await?;
    let next_due = due90_events.iter().map(|e| e.date).min();

    // Due by month: every month in the period, zero months included.
    let end = add_months(today, months);
    let period_events = calendar::events(core, ctx, &filter, today, end, false).await?;
    let first = today.with_day(1).unwrap_or(today);
    let mut data = Vec::new();
    for i in 0..months {
        let m = add_months(first, i);
        let n = period_events
            .iter()
            .filter(|e| e.date.year() == m.year() && e.date.month() == m.month())
            .count() as i64;
        data.push((
            m.format("%b").to_string(),
            n,
            format!("{}: {n} due", m.format("%B %Y")),
        ));
    }
    let due_chart = crate::charts::vbars(&data, "Items due per month");

    let other = count_of(DocumentStatus::Draft)
        + count_of(DocumentStatus::Expired)
        + count_of(DocumentStatus::Superseded);
    let status_chart = crate::charts::stacked(
        &[
            ("chart-seg-good", "Active", count_of(DocumentStatus::Active)),
            (
                "chart-seg-info",
                "In review",
                count_of(DocumentStatus::InReview),
            ),
            (
                "chart-seg-warning",
                "Renewal due",
                count_of(DocumentStatus::RenewalDue),
            ),
            (
                "chart-seg-critical",
                "Overdue",
                count_of(DocumentStatus::Overdue),
            ),
            ("chart-seg-neutral", "Other", other),
        ],
        "Documents by status",
    );

    let mut by_dept: BTreeMap<&str, i64> = BTreeMap::new();
    for o in &dash.overdue {
        *by_dept.entry(o.event.department.as_str()).or_default() += 1;
    }
    let mut rows: Vec<(String, i64)> = by_dept
        .into_iter()
        .map(|(k, v)| {
            let k = if k.is_empty() { "No department" } else { k };
            (k.to_owned(), v)
        })
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let overdue_chart =
        (!rows.is_empty()).then(|| crate::charts::hbars(&rows, "Overdue items by department"));

    let boards = if appointments::any_visible(core, ctx).await? {
        let attention = appointments::attention(core, ctx, today, 90).await?;
        let n = |f: fn(&SeatState) -> bool| attention.iter().filter(|a| f(&a.state)).count() as i64;
        let decision = n(|s| matches!(s, SeatState::DecisionOverdue | SeatState::DecisionDueToday));
        let holdover = n(|s| matches!(s, SeatState::Holdover));
        let vacant = n(|s| matches!(s, SeatState::Vacant));
        let ending = n(|s| matches!(s, SeatState::TermEnding(_)));
        let filled: i64 = appointments::list_positions(core, ctx, false)
            .await?
            .iter()
            .map(|p| p.filled)
            .sum();
        let current = (filled - holdover - decision - ending).max(0);
        Some(Boards {
            rows: vec![
                BoardRow {
                    label: "Filled and current",
                    count: current,
                },
                BoardRow {
                    label: "Decision due or overdue",
                    count: decision,
                },
                BoardRow {
                    label: "Holdover",
                    count: holdover,
                },
                BoardRow {
                    label: "Term ending soon",
                    count: ending,
                },
                BoardRow {
                    label: "Vacant",
                    count: vacant,
                },
            ],
        })
    } else {
        None
    };

    let departments = departments
        .into_iter()
        .map(|(id, name)| (id, name, department == Some(id)))
        .collect();
    Ok(Overview {
        months,
        departments,
        capped,
        as_of: fmt_date(Some(today)),
        documents: Tile {
            value: documents_total.max(0) as usize,
            sub: format!(
                "{} · {}",
                plural_n(types.len(), "type"),
                plural_n(depts.len(), "department")
            ),
        },
        overdue: Tile {
            value: dash.overdue.len(),
            sub: oldest.map_or_else(
                || "Nothing overdue".to_owned(),
                |d| format!("Oldest: {}", plural_days(d)),
            ),
        },
        due90: Tile {
            value: due90_events.len(),
            sub: next_due.map_or_else(
                || "Nothing due".to_owned(),
                |d| format!("Next: {}", fmt_date(Some(d))),
            ),
        },
        in_review: Tile {
            value: count_of(DocumentStatus::InReview).max(0) as usize,
            sub: "Awaiting sign-off".to_owned(),
        },
        due_chart,
        due_total: period_events.len(),
        due_table: format!("/reports/upcoming?days={}", months * 31),
        status_chart,
        overdue_chart,
        boards,
    })
}

fn plural_n(n: usize, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

pub async fn reports_index(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<OverviewQuery>,
) -> AppResult<Html<String>> {
    let o = build_overview(&state, &user, &q).await?;
    let page = ReportsIndex {
        l: Layout::new(&state, &user, flash, "Reports", "reports"),
        reports: reports::available(&user.ctx),
        o,
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
