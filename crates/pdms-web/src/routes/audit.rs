// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use askama::Template;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use chrono::NaiveDate;
use pdms_audit::{Category, Outcome, SealedEvent, format_timestamp};
use pdms_authz::Capability;
use pdms_core::audit_log::{self, AuditFilter, ExportFormat};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, fmt_time};

const PAGE: i64 = 100;

/// Raw filter form values, echoed back into the form.
#[derive(Deserialize, Serialize, Default)]
pub struct FilterForm {
    #[serde(default)]
    action: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    object_id: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default, skip_serializing)]
    before: Option<i64>,
}

impl FilterForm {
    fn to_filter(&self, tz: chrono_tz::Tz) -> AppResult<AuditFilter> {
        let bad = |what: &str| AppError::BadRequest(format!("invalid {what}"));
        let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
        let day = |s: &str, offset: u64| -> AppResult<Option<chrono::DateTime<chrono::Utc>>> {
            let Some(s) = non_empty(s) else {
                return Ok(None);
            };
            let d = NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(|_| bad("date"))?;
            let d = d
                .checked_add_days(chrono::Days::new(offset))
                .ok_or_else(|| bad("date"))?;
            let local = d
                .and_hms_opt(0, 0, 0)
                .and_then(|dt| dt.and_local_timezone(tz).earliest())
                .ok_or_else(|| bad("date"))?;
            Ok(Some(local.with_timezone(&chrono::Utc)))
        };
        Ok(AuditFilter {
            actor_user_id: non_empty(&self.actor)
                .map(|s| s.parse())
                .transpose()
                .map_err(|_| bad("actor ID"))?,
            action_prefix: non_empty(&self.action),
            category: non_empty(&self.category)
                .map(|s| s.parse::<Category>())
                .transpose()
                .map_err(|_| bad("category"))?,
            outcome: non_empty(&self.outcome)
                .map(|s| s.parse::<Outcome>())
                .transpose()
                .map_err(|_| bad("outcome"))?,
            object_id: non_empty(&self.object_id),
            from: day(&self.from, 0)?,
            // "To" is inclusive of the whole day.
            to: day(&self.to, 1)?,
            before_seq: self.before,
            ..AuditFilter::default()
        })
    }

    fn query_string(&self) -> String {
        serde_urlencoded::to_string(self).unwrap_or_default()
    }
}

pub struct EntryView {
    pub seq: i64,
    pub iso: String,
    pub when: String,
    pub actor: String,
    pub via: String,
    pub category: &'static str,
    pub action: String,
    pub outcome: &'static str,
    pub object: String,
    pub changes: Vec<(String, String, String)>,
    pub context: String,
}

fn entry_view(tz: chrono_tz::Tz, s: SealedEvent) -> EntryView {
    entry_view_with(tz, s, true)
}

/// `show_ip`: client IP addresses are shown only to audit viewers.
pub fn entry_view_with(tz: chrono_tz::Tz, s: SealedEvent, show_ip: bool) -> EntryView {
    let e = s.event;
    let actor = e
        .actor
        .display
        .clone()
        .unwrap_or_else(|| "(unauthenticated)".into());
    let mut via = e.actor.auth_method.as_str().to_owned();
    if let Some(ip) = e.actor.ip.filter(|_| show_ip) {
        via.push_str(&format!(" from {ip}"));
    }
    let object = match (&e.object_label, &e.object_id) {
        (Some(l), _) => format!("{} “{l}”", e.object_type),
        (None, Some(id)) => format!("{} {id}", e.object_type),
        (None, None) => e.object_type.clone(),
    };
    EntryView {
        seq: s.seq,
        iso: format_timestamp(e.occurred_at),
        when: fmt_time(tz, e.occurred_at),
        actor,
        via,
        category: e.category.as_str(),
        action: e.action,
        outcome: e.outcome.as_str(),
        object,
        changes: e
            .changes
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.field, c.before.to_string(), c.after.to_string()))
            .collect(),
        context: if e.context.is_empty() {
            String::new()
        } else {
            serde_json::to_string_pretty(&pdms_audit::AuditValue::Map(e.context).to_json())
                .unwrap_or_default()
        },
    }
}

#[derive(Template)]
#[template(path = "audit.html")]
struct AuditPage {
    l: Layout,
    f: FilterForm,
    query: String,
    categories: Vec<&'static str>,
    entries: Vec<EntryView>,
    older: Option<i64>,
    can_export: bool,
    can_verify: bool,
    tz: String,
}

pub async fn audit(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(form): Query<FilterForm>,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ViewAudit, "/audit")
        .await?;
    let filter = form.to_filter(state.tz)?;
    let rows = audit_log::search(&state.core.db, &user.ctx, &filter, PAGE).await?;
    let older = (rows.len() as i64 == PAGE)
        .then(|| rows.last().map(|r| r.seq))
        .flatten();
    let page = AuditPage {
        l: Layout::new(&state, &user, flash, "Audit log", "audit"),
        query: form.query_string(),
        categories: Category::ALL.iter().map(|c| c.as_str()).collect(),
        entries: rows.into_iter().map(|r| entry_view(state.tz, r)).collect(),
        older,
        can_export: user.can(Capability::ExportAudit),
        can_verify: user.can(Capability::VerifyAudit),
        tz: state.tz.name().to_owned(),
        f: form,
    };
    Ok(Html(page.render()?))
}

async fn export(
    state: AppState,
    user: CurrentUser,
    form: FilterForm,
    format: ExportFormat,
) -> AppResult<Response> {
    user.require(&state, Capability::ExportAudit, "/audit/export")
        .await?;
    let filter = form.to_filter(state.tz)?;
    let body = audit_log::export(&state.core.db, &user.ctx, &filter, format).await?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let (ctype, ext) = match format {
        ExportFormat::Csv => ("text/csv; charset=utf-8", "csv"),
        ExportFormat::Json => ("application/json", "json"),
    };
    Ok((
        [
            (header::CONTENT_TYPE, ctype.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"audit-{stamp}.{ext}\""),
            ),
        ],
        body,
    )
        .into_response())
}

pub async fn export_csv(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(form): Query<FilterForm>,
) -> AppResult<Response> {
    export(state, user, form, ExportFormat::Csv).await
}

pub async fn export_json(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(form): Query<FilterForm>,
) -> AppResult<Response> {
    export(state, user, form, ExportFormat::Json).await
}

#[derive(Template)]
#[template(path = "audit_verify.html")]
struct VerifyPage {
    l: Layout,
    intact: bool,
    checked: u64,
    last_seq: i64,
    last_hash: String,
    breaks: Vec<String>,
}

pub async fn verify(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::VerifyAudit, "/audit/verify")
        .await?;
    let report = audit_log::verify(&state.core.db, &user.ctx).await?;
    let page = VerifyPage {
        l: Layout::new(&state, &user, flash, "Audit integrity check", "audit"),
        intact: report.is_intact(),
        checked: report.entries_checked,
        last_seq: report.last.seq,
        last_hash: hex_string(&report.last.entry_hash),
        breaks: report.breaks.iter().map(ToString::to_string).collect(),
    };
    Ok(Html(page.render()?))
}

fn hex_string(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
