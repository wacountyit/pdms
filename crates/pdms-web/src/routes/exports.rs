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
use pdms_core::appointments;
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
    /// The first 12 hex digits of the package's SHA-256, for the table.
    pub sha_short: String,
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
                sha_short: p.package_sha256.chars().take(12).collect(),
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

/// The N in "Based on the first N documents you can see", when a cap was
/// reached. `scanned` is what the overview's own scan returned (at most
/// [`OVERVIEW_DOC_CAP`]). `in_scope` is the exact number of visible,
/// non-archived documents matching the department filter: what
/// `calendar::dashboard` and `calendar::events` read, at most
/// [`calendar::MAX_DOCUMENTS`] of them, for the Overdue and Due-in-90 tiles
/// and the month and department charts. The smaller cap that binds wins.
fn cap_reached(scanned: i64, in_scope: i64) -> Option<i64> {
    [
        (in_scope >= calendar::MAX_DOCUMENTS, calendar::MAX_DOCUMENTS),
        (scanned >= OVERVIEW_DOC_CAP, OVERVIEW_DOC_CAP),
    ]
    .into_iter()
    .filter(|(hit, _)| *hit)
    .map(|(_, cap)| cap)
    .min()
}

/// The last day of the due-by-month chart: the end of the last month it
/// shows. The chart starts with the current month, so `months` bars run to
/// the day before the first of month `months` from now.
fn period_end(today: NaiveDate, months: u32) -> NaiveDate {
    let first = today.with_day(1).unwrap_or(today);
    add_months(first, months)
        .pred_opt()
        .unwrap_or(today)
        .max(today)
}

pub struct Tile {
    pub value: usize,
    pub sub: String,
    pub href: String,
}

pub struct BoardRow {
    pub label: &'static str,
    pub count: i64,
}

pub struct Boards {
    /// "Seats you can see", or "… in this department" when filtered.
    pub scope: &'static str,
    /// No visible position belongs to the selected department.
    pub empty: bool,
    pub rows: Vec<BoardRow>,
}

/// Everything the "County overview" shows, built only from visibility-filtered
/// sources.
pub struct Overview {
    pub months: u32,
    pub departments: Vec<(Uuid, String, bool)>,
    /// "5,000" when a document cap was reached (see [`cap_reached`]).
    pub cap: Option<String>,
    pub as_of: String,
    pub documents: Tile,
    pub overdue: Tile,
    pub due90: Tile,
    pub in_review: Tile,
    pub due_chart: crate::charts::VBars,
    /// The sum of the month bars.
    pub due_total: i64,
    pub due_table: String,
    pub status_chart: crate::charts::Stacked,
    pub status_table: String,
    pub overdue_chart: Option<crate::charts::HBars>,
    pub overdue_table: String,
    pub boards: Option<Boards>,
}

#[derive(Deserialize, Default)]
pub struct OverviewQuery {
    months: Option<u32>,
    #[serde(default)]
    department: String,
}

/// `base` narrowed to the overview's department, so a tile or "View as
/// table" link opens the same slice the overview shows.
fn in_department(base: &str, department: Option<Uuid>) -> String {
    match department {
        Some(id) if base.contains('?') => format!("{base}&department={id}"),
        Some(id) => format!("{base}?department={id}"),
        None => base.to_owned(),
    }
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
    let today = pdms_core::schedule::local_today(Utc::now(), core.config().timezone());

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
    // What the calendar functions below read: visible, matching, not archived.
    let in_scope: i64 = counts
        .iter()
        .filter(|(s, _)| *s != DocumentStatus::Archived)
        .map(|(_, n)| *n)
        .sum();
    let cap = cap_reached(visible.len() as i64, in_scope).map(crate::views::group_digits);

    let dash = calendar::dashboard(core, ctx, &filter, 1).await?;
    let oldest = dash.overdue.iter().map(|o| o.days_overdue).max();
    let due90_events =
        calendar::events(core, ctx, &filter, today, today + Duration::days(90), false).await?;
    let next_due = due90_events.iter().map(|e| e.date).min();

    // Due by month: every month in the period, zero months included. The
    // period ends with the last month shown, so every event has a bar and the
    // total is the sum of the bars.
    let end = period_end(today, months);
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
    let due_total: i64 = data.iter().map(|d| d.1).sum();
    let due_chart = crate::charts::vbars(&data, "Items due per month");

    // Superseded documents are left out, as the Documents tile leaves them
    // out, so the chart's total equals the tile.
    let other = count_of(DocumentStatus::Draft) + count_of(DocumentStatus::Expired);
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
        // The department filter applies here too: only positions of the
        // selected department (a position with no department then drops out).
        let positions: Vec<appointments::Position> = appointments::list_positions(core, ctx, false)
            .await?
            .into_iter()
            .filter(|p| department.is_none_or(|id| p.department_id == Some(id)))
            .collect();
        let position_ids: BTreeSet<Uuid> = positions.iter().map(|p| p.id).collect();
        let attention: Vec<_> =
            appointments::attention(core, ctx, today, crate::seats::HORIZON_DAYS)
                .await?
                .into_iter()
                .filter(|a| position_ids.contains(&a.position_id))
                .collect();
        let counts = crate::seats::Counts::of(
            &positions.iter().collect::<Vec<_>>(),
            &attention.iter().collect::<Vec<_>>(),
        );
        Some(Boards {
            scope: if department.is_some() {
                "Seats you can see in this department"
            } else {
                "Seats you can see"
            },
            empty: department.is_some() && positions.is_empty(),
            rows: counts
                .rows()
                .into_iter()
                .map(|(label, count)| BoardRow { label, count })
                .collect(),
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
        cap,
        as_of: fmt_date(Some(today)),
        documents: Tile {
            value: documents_total.max(0) as usize,
            sub: format!(
                "{} · {}",
                plural_n(types.len(), "type"),
                plural_n(depts.len(), "department")
            ),
            href: in_department("/documents", department),
        },
        overdue: Tile {
            value: dash.overdue.len(),
            // The tile counts overdue dates of documents and board terms; the
            // status chart counts documents in Overdue status.
            sub: oldest.map_or_else(
                || "Documents and board terms · none".to_owned(),
                |d| format!("Documents and board terms · oldest {}", plural_days(d)),
            ),
            href: in_department("/reports/overdue", department),
        },
        due90: Tile {
            value: due90_events.len(),
            sub: next_due.map_or_else(
                || "Nothing due".to_owned(),
                |d| format!("Next: {}", fmt_date(Some(d))),
            ),
            href: in_department("/reports/upcoming?days=90", department),
        },
        in_review: Tile {
            value: count_of(DocumentStatus::InReview).max(0) as usize,
            sub: "Awaiting sign-off".to_owned(),
            href: in_department("/documents?status=in_review", department),
        },
        due_chart,
        due_total,
        // The Upcoming report covers [today, today + days]: the same period.
        due_table: in_department(
            &format!("/reports/upcoming?days={}", (end - today).num_days().max(1)),
            department,
        ),
        status_chart,
        status_table: in_department("/documents", department),
        overdue_chart,
        overdue_table: in_department("/reports/overdue", department),
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
    /// The page of rows shown (the CSV always has every row).
    #[serde(default)]
    page: Option<usize>,
}

/// Rows per page of a report.
const REPORT_PAGE: usize = 100;

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
    /// Only this page's rows.
    r: Report,
    q: ReportQuery,
    total: usize,
    page: usize,
    pages: usize,
    /// The query string without the page, for pager links.
    query: String,
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
    let mut r = reports::run(&state.core, &user.ctx, &key, &params).await?;
    let total = r.rows.len();
    let pages = total.div_ceil(REPORT_PAGE).max(1);
    let page = q.page.unwrap_or(1).clamp(1, pages);
    r.rows = r
        .rows
        .into_iter()
        .skip((page - 1) * REPORT_PAGE)
        .take(REPORT_PAGE)
        .collect();
    let page = ReportPage {
        l: Layout::new(&state, &user, flash, info.title, "reports"),
        total,
        page,
        pages,
        query: q.query_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or(NaiveDate::MIN)
    }

    #[test]
    fn cap_notice_names_the_cap_that_binds() {
        // Nothing reached.
        assert_eq!(cap_reached(4_999, 4_999), None);
        // The calendar reads at most 5,000: the tiles and charts that use it
        // are partial even though the overview's own scan is not.
        assert_eq!(
            cap_reached(4_999, calendar::MAX_DOCUMENTS),
            Some(calendar::MAX_DOCUMENTS)
        );
        // Both reached: the smaller cap is the one to state.
        assert_eq!(
            cap_reached(OVERVIEW_DOC_CAP, 25_000),
            Some(calendar::MAX_DOCUMENTS)
        );
        // Only the overview's scan reached its cap (a department filter
        // leaves fewer than 5,000 in scope).
        assert_eq!(cap_reached(OVERVIEW_DOC_CAP, 120), Some(OVERVIEW_DOC_CAP));
        assert_eq!(crate::views::group_digits(calendar::MAX_DOCUMENTS), "5,000");
    }

    #[test]
    fn period_ends_with_the_last_month_shown() {
        // Six bars: October through March.
        assert_eq!(period_end(day("2026-10-21"), 6), day("2027-03-31"));
        assert_eq!(period_end(day("2026-10-01"), 6), day("2027-03-31"));
        assert_eq!(period_end(day("2026-10-31"), 12), day("2027-09-30"));
        // Leap February.
        assert_eq!(period_end(day("2027-09-15"), 6), day("2028-02-29"));
    }
}
