// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Dashboard, calendar, ICS feeds, notification inbox, review tasks, and notification rules.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use pdms_authz::Capability;
use pdms_core::calendar::{self, CalEvent, Feed};
use pdms_core::directory::{self, Department, Group, Role, UserSummary};
use pdms_core::documents::search::DocumentFilter;
use pdms_core::documents::status::DocumentStatus;
use pdms_core::documents::tags::{self, Tag};
use pdms_core::documents::types::{self, DocumentType};
use pdms_core::model::UserStatus;
use pdms_core::notifications::inbox::{self, InboxItem};
use pdms_core::notifications::rules::{
    self, Channel, DueEvent, Recipient, Rule, RuleScope, RuleSpec,
};
use pdms_core::reviews::{self, Completion, Task};
use pdms_core::{CoreError, CoreResult, schedule};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::session::{Client, CurrentUser};
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_date, fmt_time, redirect_with};

pub struct EventView {
    pub href: String,
    pub label: &'static str,
    pub kind: &'static str,
    pub external_id: String,
    pub title: String,
    pub department: String,
    pub date: String,
    pub projected: bool,
    pub overdue: bool,
}

fn event_view(e: &CalEvent, today: NaiveDate) -> EventView {
    EventView {
        href: e.href(),
        label: e.label(),
        kind: e.kind.as_str(),
        external_id: e.external_id.clone(),
        title: e.title.clone(),
        department: e.department.clone(),
        date: fmt_date(Some(e.date)),
        projected: e.projected,
        overdue: !e.projected && e.date < today && e.kind != DueEvent::Expiration,
    }
}

// ---------------------------------------------------------------------------
// Dashboard (home page)
// ---------------------------------------------------------------------------

pub struct OverdueView {
    pub e: EventView,
    pub days: i64,
    pub missed: String,
    pub missed_count: usize,
}

pub struct TaskView {
    pub id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub kind: &'static str,
    pub due: String,
    pub status: &'static str,
    pub status_label: &'static str,
}

fn task_view(t: &Task) -> TaskView {
    TaskView {
        id: t.id,
        document_id: t.document_id,
        external_id: t.external_id.clone(),
        title: t.title.clone(),
        kind: t.kind.label(),
        due: fmt_date(Some(t.due_date)),
        status: t.status.as_str(),
        status_label: t.status.label(),
    }
}

pub struct ApprovalView {
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub kind: &'static str,
    pub submitted: String,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardPage {
    l: Layout,
    days: i64,
    upcoming: Vec<EventView>,
    overdue: Vec<OverdueView>,
    tasks: Vec<TaskView>,
    approvals: Vec<ApprovalView>,
    inbox: Vec<InboxItem>,
    departments: Vec<String>,
    roles: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct DashQuery {
    days: Option<i64>,
}

pub async fn dashboard(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<DashQuery>,
) -> AppResult<Html<String>> {
    let core = &state.core;
    let days = q
        .days
        .filter(|d| [30, 60, 90, 365].contains(d))
        .unwrap_or(30);
    let dash = calendar::dashboard(core, &user.ctx, &DocumentFilter::default(), days).await?;
    let me = directory::get_user(&core.db, user.session.user_id).await?;
    let page = DashboardPage {
        l: Layout::new(
            &state,
            &user,
            flash,
            &format!("Welcome, {}", user.session.display_name),
            "home",
        ),
        days,
        upcoming: dash
            .upcoming
            .iter()
            .map(|e| event_view(e, dash.today))
            .collect(),
        overdue: dash
            .overdue
            .iter()
            .map(|o| OverdueView {
                e: event_view(&o.event, dash.today),
                days: o.days_overdue,
                missed_count: o.missed_cycles.len(),
                missed: o
                    .missed_cycles
                    .iter()
                    .map(|d| fmt_date(Some(*d)))
                    .collect::<Vec<_>>()
                    .join(", "),
            })
            .collect(),
        tasks: reviews::my_open(core, &user.ctx)
            .await?
            .iter()
            .map(task_view)
            .collect(),
        approvals: pdms_core::workflow::my_pending(core, &user.ctx)
            .await?
            .into_iter()
            .map(|p| ApprovalView {
                document_id: p.document_id,
                external_id: p.external_id,
                title: p.title,
                kind: p.kind.label(),
                submitted: format!(
                    "{} · {}",
                    p.submitted_by.unwrap_or_default(),
                    fmt_date(Some(p.submitted_at.with_timezone(&state.tz).date_naive()))
                ),
            })
            .collect(),
        inbox: inbox::list(core, &user.ctx, false)
            .await?
            .into_iter()
            .take(5)
            .collect(),
        departments: me
            .memberships
            .iter()
            .map(|m| m.department_name.clone())
            .collect(),
        roles: me
            .roles
            .iter()
            .map(|r| match &r.department_name {
                Some(d) => format!("{} ({d})", r.role_name),
                None => r.role_name.clone(),
            })
            .collect(),
    };
    Ok(Html(page.render()?))
}

// ---------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct CalQuery {
    #[serde(default)]
    view: String,
    #[serde(default)]
    month: String,
    #[serde(default)]
    days: Option<i64>,
    #[serde(default)]
    department: String,
    #[serde(default, rename = "type")]
    doc_type: String,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    mine: String,
    #[serde(default)]
    status: String,
}

impl CalQuery {
    fn filter(&self, me: Uuid) -> DocumentFilter {
        DocumentFilter {
            department_id: self.department.parse().ok(),
            document_type_id: self.doc_type.parse().ok(),
            tags: self.tag.iter().filter_map(|t| t.parse().ok()).collect(),
            owner_id: (self.mine == "1").then_some(me),
            status: DocumentStatus::parse(&self.status),
            ..DocumentFilter::default()
        }
    }

    /// Filters as a query string (without view/month), for navigation links.
    fn filter_query(&self) -> String {
        let mut pairs: Vec<(&str, String)> = Vec::new();
        for (k, v) in [
            ("department", &self.department),
            ("type", &self.doc_type),
            ("mine", &self.mine),
            ("status", &self.status),
        ] {
            if !v.is_empty() {
                pairs.push((k, v.clone()));
            }
        }
        for t in &self.tag {
            pairs.push(("tag", t.clone()));
        }
        serde_urlencoded::to_string(pairs).unwrap_or_default()
    }
}

pub struct DayCell {
    pub day: u32,
    pub iso: String,
    pub in_month: bool,
    pub today: bool,
    pub events: Vec<EventView>,
}

#[derive(Template)]
#[template(path = "calendar.html")]
struct CalendarPage {
    l: Layout,
    q: CalQuery,
    filters: String,
    agenda: bool,
    month_label: String,
    prev_month: String,
    next_month: String,
    weeks: Vec<Vec<DayCell>>,
    agenda_days: i64,
    agenda_groups: Vec<(String, Vec<EventView>)>,
    types: Vec<DocumentType>,
    departments: Vec<Department>,
    tags: Vec<Tag>,
    statuses: Vec<(&'static str, &'static str)>,
}

pub async fn calendar_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<CalQuery>,
) -> AppResult<Html<String>> {
    let core = &state.core;
    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let filter = q.filter(user.session.user_id);
    let agenda = q.view == "agenda";
    let first = NaiveDate::parse_from_str(&format!("{}-01", q.month), "%Y-%m-%d")
        .unwrap_or_else(|_| today.with_day(1).unwrap_or(today));
    let (mut weeks, mut groups) = (Vec::new(), Vec::new());
    let agenda_days = q.days.unwrap_or(90).clamp(7, 730);
    if agenda {
        let events = calendar::events(
            core,
            &user.ctx,
            &filter,
            today,
            today + Duration::days(agenda_days),
            true,
        )
        .await?;
        let mut current: Option<NaiveDate> = None;
        for e in &events {
            if current != Some(e.date) {
                groups.push((e.date.format("%A, %B %-d, %Y").to_string(), Vec::new()));
                current = Some(e.date);
            }
            if let Some(g) = groups.last_mut() {
                g.1.push(event_view(e, today));
            }
        }
    } else {
        // Weeks start on Sunday.
        let start = first - Duration::days(i64::from(first.weekday().num_days_from_sunday()));
        let next_first = schedule::add_months(first, 1);
        let end = next_first
            + Duration::days(i64::from(
                (7 - next_first.weekday().num_days_from_sunday()) % 7,
            ))
            - Duration::days(1);
        let events = calendar::events(core, &user.ctx, &filter, start, end, true).await?;
        // Overdue reviews and renewals have past dates; show them on today so
        // they cannot be missed.
        let mut overdue: Option<Vec<EventView>> = if today >= start && today <= end {
            Some(
                calendar::events(
                    core,
                    &user.ctx,
                    &filter,
                    NaiveDate::MIN,
                    today - Duration::days(1),
                    false,
                )
                .await?
                .iter()
                .filter(|e| {
                    e.kind != DueEvent::Expiration && e.status != Some(DocumentStatus::Expired)
                })
                .map(|e| event_view(e, today))
                .collect(),
            )
        } else {
            None
        };
        let mut d = start;
        while d <= end {
            let mut week = Vec::new();
            for _ in 0..7 {
                week.push(DayCell {
                    day: d.day(),
                    iso: d.to_string(),
                    in_month: d.month() == first.month(),
                    today: d == today,
                    events: {
                        let mut v = if d == today {
                            overdue.take().unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        v.extend(
                            events
                                .iter()
                                .filter(|e| e.date == d)
                                .map(|e| event_view(e, today)),
                        );
                        v
                    },
                });
                d += Duration::days(1);
            }
            weeks.push(week);
        }
    }
    let page = CalendarPage {
        l: Layout::new(&state, &user, flash, "Calendar", "calendar"),
        filters: q.filter_query(),
        agenda,
        month_label: first.format("%B %Y").to_string(),
        prev_month: schedule::add_months(first, -1).format("%Y-%m").to_string(),
        next_month: schedule::add_months(first, 1).format("%Y-%m").to_string(),
        weeks,
        agenda_days,
        agenda_groups: groups,
        types: types::list_types(&core.db).await?,
        departments: directory::list_departments(&core.db).await?,
        tags: tags::list(&core.db).await?,
        statuses: DocumentStatus::ALL
            .iter()
            .map(|s| (s.as_str(), s.label()))
            .collect(),
        q,
    };
    Ok(Html(page.render()?))
}

// ---------------------------------------------------------------------------
// Calendar feeds
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "feeds.html")]
struct FeedsPage {
    l: Layout,
    feeds: Vec<(Feed, String, String)>,
    departments: Vec<Department>,
}

pub async fn feeds(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let page = FeedsPage {
        l: Layout::new(&state, &user, flash, "Calendar subscriptions", "calendar"),
        feeds: calendar::list_feeds(&state.core, &user.ctx)
            .await?
            .into_iter()
            .map(|f| {
                let created = fmt_time(state.tz, f.created_at);
                let used = f
                    .last_used_at
                    .map(|t| fmt_time(state.tz, t))
                    .unwrap_or_else(|| "Never".into());
                (f, created, used)
            })
            .collect(),
        departments: directory::list_departments(&state.core.db).await?,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct FeedForm {
    name: String,
    #[serde(default)]
    department_id: String,
    only_owned: Option<String>,
}

pub async fn create_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<FeedForm>,
) -> AppResult<Response> {
    let dept = if f.department_id.is_empty() {
        None
    } else {
        Some(
            f.department_id
                .parse()
                .map_err(|_| AppError::BadRequest("invalid department".into()))?,
        )
    };
    match calendar::create_feed(
        &state.core,
        &user.ctx,
        &f.name,
        dept,
        f.only_owned.as_deref() == Some("true"),
    )
    .await
    {
        Ok((_, token)) => {
            let url = state
                .core
                .config
                .server
                .public_url
                .join(&format!("ics/{token}.ics"))
                .map(|u| u.to_string())
                .unwrap_or_default();
            Ok(redirect_with(
                &state,
                "/calendar/feeds",
                true,
                &format!(
                    "Subscription created. Add this address to Outlook (Add calendar → From internet). It is shown only once: {url}"
                ),
            ))
        }
        Err(e) => done::<()>(&state, "/calendar/feeds", Err(e), ""),
    }
}

pub async fn revoke_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = calendar::revoke_feed(&state.core, &user.ctx, id).await;
    done(
        &state,
        "/calendar/feeds",
        r,
        "Subscription revoked. Calendars using it will stop updating.",
    )
}

/// Public ICS endpoint: the secret token is the credential.
pub async fn ics(
    State(state): State<AppState>,
    Client(client): Client,
    Path(file): Path<String>,
) -> AppResult<Response> {
    let token = file.strip_suffix(".ics").unwrap_or(&file);
    match calendar::render_feed(&state.core, token, &client).await? {
        Some(body) => Ok((
            [
                (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
                (header::CACHE_CONTROL, "private, max-age=900"),
            ],
            body,
        )
            .into_response()),
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

// ---------------------------------------------------------------------------
// Notification inbox
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "notifications.html")]
struct InboxPage {
    l: Layout,
    items: Vec<(InboxItem, String, String)>,
    show_all: bool,
}

#[derive(Deserialize, Default)]
pub struct InboxQuery {
    #[serde(default)]
    all: String,
}

pub async fn inbox_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<InboxQuery>,
) -> AppResult<Html<String>> {
    let show_all = q.all == "1";
    let items = inbox::list(&state.core, &user.ctx, show_all).await?;
    let page = InboxPage {
        l: Layout::new(&state, &user, flash, "Notifications", "notifications"),
        items: items
            .into_iter()
            .map(|i| {
                let when = fmt_time(state.tz, i.created_at);
                let due = fmt_date(i.due_date);
                (i, when, due)
            })
            .collect(),
        show_all,
    };
    Ok(Html(page.render()?))
}

pub async fn acknowledge(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = inbox::acknowledge(&state.core, &user.ctx, id).await;
    done(&state, "/notifications", r, "Acknowledged.")
}

pub async fn acknowledge_all(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Response> {
    let mut result = Ok(());
    for item in inbox::list(&state.core, &user.ctx, false).await? {
        result = inbox::acknowledge(&state.core, &user.ctx, item.delivery_id).await;
        if result.is_err() {
            break;
        }
    }
    done(
        &state,
        "/notifications",
        result,
        "All notifications acknowledged.",
    )
}

// ---------------------------------------------------------------------------
// Review tasks
// ---------------------------------------------------------------------------

fn back(id: Uuid) -> String {
    format!("/documents/{id}#reviews")
}

pub async fn start_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((doc, task)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = reviews::start(&state.core, &user.ctx, task).await;
    done(&state, &back(doc), r, "Marked as started.")
}

#[derive(Deserialize)]
pub struct CompleteForm {
    #[serde(default)]
    completed_on: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    new_renewal_date: String,
    #[serde(default)]
    new_expiration_date: String,
}

fn opt_date(s: &str) -> CoreResult<Option<NaiveDate>> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map(Some)
        .map_err(|_| CoreError::validation("enter dates as YYYY-MM-DD"))
}

pub async fn complete_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((doc, task)): Path<(Uuid, Uuid)>,
    Form(f): Form<CompleteForm>,
) -> AppResult<Response> {
    let r = async {
        let c = Completion {
            completed_on: opt_date(&f.completed_on)?,
            note: f.note.clone(),
            new_renewal_date: opt_date(&f.new_renewal_date)?,
            new_expiration_date: opt_date(&f.new_expiration_date)?,
        };
        reviews::complete(&state.core, &user.ctx, task, c).await
    }
    .await;
    done(
        &state,
        &back(doc),
        r,
        "Completed. The schedule has been updated.",
    )
}

pub async fn approve_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((doc, task)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = reviews::approve(&state.core, &user.ctx, task).await;
    match r {
        Err(CoreError::Forbidden(Some(pdms_authz::DenyReason::SelfApproval))) => Ok(redirect_with(
            &state,
            &back(doc),
            false,
            "You completed this review, so someone else must approve it.",
        )),
        other => done(&state, &back(doc), other, "Approved."),
    }
}

// ---------------------------------------------------------------------------
// Notification rules (document overrides and administration)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RuleForm {
    event: String,
    offsets: String,
    #[serde(default)]
    overdue_repeat_days: String,
    #[serde(default)]
    channel: Vec<String>,
    #[serde(default)]
    recipient: Vec<String>,
    is_active: Option<String>,
    #[serde(default)]
    back: String,
}

impl RuleForm {
    fn spec(&self) -> CoreResult<RuleSpec> {
        let event =
            DueEvent::parse(&self.event).ok_or_else(|| CoreError::validation("choose an event"))?;
        let offsets = self
            .offsets
            .split([',', ' '])
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse::<i32>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                CoreError::validation("reminder days must be whole numbers, e.g. 90, 30, 7, 0")
            })?;
        let repeat = if self.overdue_repeat_days.trim().is_empty() {
            None
        } else {
            Some(
                self.overdue_repeat_days
                    .trim()
                    .parse()
                    .map_err(|_| CoreError::validation("repeat must be a whole number of days"))?,
            )
        };
        let channels = self
            .channel
            .iter()
            .filter_map(|c| match c.as_str() {
                "email" => Some(Channel::Email),
                "in_app" => Some(Channel::InApp),
                _ => None,
            })
            .collect();
        let recipients = self
            .recipient
            .iter()
            .filter(|r| !r.is_empty())
            .filter_map(|r| Recipient::parse(r))
            .collect();
        Ok(RuleSpec {
            event,
            offsets_days: offsets,
            overdue_repeat_days: repeat,
            channels,
            recipients,
            is_active: self.is_active.as_deref() != Some("false"),
        })
    }
}

fn rule_back(s: &str, default: &str) -> String {
    if (s.starts_with("/documents/") || s.starts_with("/admin/"))
        && !s.contains("//")
        && !s.contains('\\')
    {
        s.to_owned()
    } else {
        default.to_owned()
    }
}

pub async fn create_document_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(doc): Path<Uuid>,
    Form(f): Form<RuleForm>,
) -> AppResult<Response> {
    let r =
        async { rules::create(&state.core, &user.ctx, RuleScope::Document(doc), f.spec()?).await }
            .await;
    done(
        &state,
        &format!("/documents/{doc}#notifications"),
        r,
        "Reminder rule saved for this document.",
    )
}

pub async fn create_admin_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<AdminRuleForm>,
) -> AppResult<Response> {
    let scope = if f.document_type_id.is_empty() {
        RuleScope::Organization
    } else {
        RuleScope::DocumentType(
            f.document_type_id
                .parse()
                .map_err(|_| AppError::BadRequest("invalid type".into()))?,
        )
    };
    let r = async { rules::create(&state.core, &user.ctx, scope, f.rule.spec()?).await }.await;
    done(&state, "/admin/notification-rules", r, "Rule added.")
}

#[derive(Deserialize)]
pub struct AdminRuleForm {
    #[serde(default)]
    document_type_id: String,
    #[serde(flatten)]
    rule: RuleForm,
}

pub async fn update_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<RuleForm>,
) -> AppResult<Response> {
    let to = rule_back(&f.back, "/admin/notification-rules");
    let r = async { rules::update(&state.core, &user.ctx, id, f.spec()?).await }.await;
    done(&state, &to, r, "Rule saved.")
}

#[derive(Deserialize)]
pub struct BackForm {
    #[serde(default)]
    back: String,
}

pub async fn delete_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<BackForm>,
) -> AppResult<Response> {
    let to = rule_back(&f.back, "/admin/notification-rules");
    let r = rules::delete(&state.core, &user.ctx, id).await;
    done(&state, &to, r, "Rule removed.")
}

/// Choices for recipient pickers as (recipient key, label).
pub struct RecipientOptions {
    pub users: Vec<(String, String)>,
    pub groups: Vec<(String, String)>,
    pub departments: Vec<(String, String)>,
    pub roles: Vec<(String, String)>,
}

impl RecipientOptions {
    pub async fn load(state: &AppState) -> AppResult<Self> {
        let db = &state.core.db;
        let users: Vec<UserSummary> = directory::list_users(db, "", 5000)
            .await?
            .into_iter()
            .filter(|u| u.status == UserStatus::Active)
            .collect();
        let groups: Vec<Group> = directory::list_groups(db).await?;
        let departments: Vec<Department> = directory::list_departments(db).await?;
        let roles: Vec<Role> = directory::list_roles(db).await?;
        Ok(RecipientOptions {
            users: users
                .into_iter()
                .map(|u| (Recipient::User(u.id).key(), u.display_name))
                .collect(),
            groups: groups
                .into_iter()
                .map(|g| (Recipient::Group(g.id).key(), g.name))
                .collect(),
            departments: departments
                .into_iter()
                .map(|d| (Recipient::Department(d.id).key(), d.name))
                .collect(),
            roles: roles
                .into_iter()
                .map(|r| (Recipient::Role(r.id).key(), r.name))
                .collect(),
        })
    }
}

/// Values for one rule form.
pub struct RuleFormView {
    pub form_id: String,
    pub event: String,
    pub offsets: String,
    pub repeat: String,
    pub email: bool,
    pub in_app: bool,
    pub keys: Vec<String>,
}

impl RuleFormView {
    pub fn has(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }

    pub fn blank(form_id: &str, event: DueEvent) -> Self {
        RuleFormView {
            form_id: form_id.to_owned(),
            event: event.as_str().to_owned(),
            offsets: "90, 30, 7, 0".into(),
            repeat: "7".into(),
            email: true,
            in_app: true,
            keys: vec![Recipient::Owner.key()],
        }
    }
}

pub struct RuleView {
    pub rule: Rule,
    pub scope_label: String,
    pub form: RuleFormView,
}

impl RuleView {
    pub fn new(rule: Rule, scope_label: String) -> Self {
        let form = RuleFormView {
            form_id: format!("r{}", rule.id.simple()),
            event: rule.event.as_str().to_owned(),
            offsets: rule
                .offsets_days
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            repeat: rule
                .overdue_repeat_days
                .map(|r| r.to_string())
                .unwrap_or_default(),
            email: rule.channels.contains(&Channel::Email),
            in_app: rule.channels.contains(&Channel::InApp),
            keys: rule.recipients.iter().map(|r| r.0.key()).collect(),
        };
        RuleView {
            rule,
            scope_label,
            form,
        }
    }
}

#[derive(Template)]
#[template(path = "admin/notification_rules.html")]
struct AdminRulesPage {
    l: Layout,
    rules: Vec<RuleView>,
    types: Vec<DocumentType>,
    opts: RecipientOptions,
    events: Vec<(&'static str, &'static str)>,
    new_form: RuleFormView,
}

pub async fn admin_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(
        &state,
        Capability::ManageDocumentSchema,
        "/admin/notification-rules",
    )
    .await?;
    let types = types::list_types(&state.core.db).await?;
    let rules = rules::load_all(&state.core.db)
        .await?
        .into_iter()
        .filter_map(|r| {
            let label = match r.scope {
                RuleScope::Organization => "Organization default".to_owned(),
                RuleScope::DocumentType(t) => format!(
                    "{} documents",
                    types
                        .iter()
                        .find(|x| x.id == t)
                        .map_or("?", |x| x.name.as_str())
                ),
                RuleScope::Document(_) | RuleScope::Position(_) => return None,
            };
            Some(RuleView::new(r, label))
        })
        .collect();
    let page = AdminRulesPage {
        l: Layout::new(&state, &user, flash, "Notification rules", "admin"),
        rules,
        types,
        opts: RecipientOptions::load(&state).await?,
        // Appointment events apply to organization defaults only (not to types).
        events: DueEvent::ALL
            .iter()
            .chain(DueEvent::APPOINTMENT.iter())
            .map(|e| (e.as_str(), e.label()))
            .collect(),
        new_form: RuleFormView::blank("new", DueEvent::Review),
    };
    Ok(Html(page.render()?))
}

// ---------------------------------------------------------------------------
// Delivery log (administrators)
// ---------------------------------------------------------------------------

pub struct DeliveryRow {
    pub when: String,
    pub href: String,
    pub external_id: String,
    pub event: String,
    pub trigger: String,
    pub recipient: String,
    pub channel: String,
    pub status: String,
    pub attempts: i32,
    pub detail: String,
}

#[derive(Template)]
#[template(path = "admin/deliveries.html")]
struct DeliveriesPage {
    l: Layout,
    rows: Vec<DeliveryRow>,
    status: String,
}

#[derive(Deserialize, Default)]
pub struct DeliveryQuery {
    #[serde(default)]
    status: String,
}

pub async fn deliveries(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<DeliveryQuery>,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageSettings, "/admin/notifications")
        .await?;
    let rows = sqlx::query!(
        r#"SELECT dl.created_at, n.document_id, n.appointment_id, a.position_id AS "position_id?",
                  coalesce(d.external_id, a.person_name, '') AS "external_id!",
                  coalesce(n.event::text, 'action') AS "event!", n.trigger::text AS "trigger?",
                  n.offset_days, n.subject, u.display_name, u.email, dl.channel::text AS "channel!", dl.status::text AS "status!",
                  dl.attempts, coalesce(dl.last_error, dl.skip_reason, '') AS "detail!"
             FROM notification_deliveries dl
             JOIN notifications n ON n.id = dl.notification_id
             LEFT JOIN documents d ON d.id = n.document_id
             LEFT JOIN appointments a ON a.id = n.appointment_id
             JOIN users u ON u.id = dl.user_id
            WHERE ($1 = '' OR dl.status::text = $1)
            ORDER BY dl.created_at DESC LIMIT 500"#,
        q.status
    )
    .fetch_all(&state.core.db)
    .await
    .map_err(CoreError::from)?;
    let page = DeliveriesPage {
        l: Layout::new(&state, &user, flash, "Notification delivery log", "admin"),
        rows: rows
            .into_iter()
            .map(|r| DeliveryRow {
                when: fmt_time(state.tz, r.created_at),
                href: match (r.document_id, r.position_id) {
                    (Some(d), _) => format!("/documents/{d}"),
                    (None, Some(p)) => format!("/positions/{p}"),
                    _ => String::new(),
                },
                external_id: r.external_id,
                event: r.event,
                trigger: match (r.trigger, r.offset_days, r.subject) {
                    (Some(t), Some(o), _) => format!("{t} ({o} days)"),
                    (_, _, Some(subject)) => subject,
                    _ => String::new(),
                },
                recipient: format!("{} <{}>", r.display_name, r.email),
                channel: r.channel,
                status: r.status,
                attempts: r.attempts,
                detail: r.detail,
            })
            .collect(),
        status: q.status,
    };
    Ok(Html(page.render()?))
}

/// Data for the document page's review and notification sections.
pub struct DocSchedule {
    pub open_tasks: Vec<Task>,
    pub history: Vec<Task>,
    pub rules: Vec<(DueEvent, rules::RuleSource, Vec<RuleView>)>,
    pub opts: Option<RecipientOptions>,
    pub missed: Vec<NaiveDate>,
}

pub async fn doc_schedule(
    state: &AppState,
    user: &CurrentUser,
    doc: &pdms_core::documents::Document,
    can_edit: bool,
) -> AppResult<DocSchedule> {
    let core = &state.core;
    let tasks = reviews::for_document(core, &user.ctx, doc.id).await?;
    let (open_tasks, history): (Vec<Task>, Vec<Task>) =
        tasks.into_iter().partition(|t| t.status.is_open());
    let all = rules::load_all(&core.db).await?;
    let rules = DueEvent::ALL
        .iter()
        .map(|e| {
            let (rs, source) = rules::effective(&all, doc.id, doc.document_type_id, *e);
            (
                *e,
                source,
                rs.into_iter()
                    .cloned()
                    .map(|r| RuleView::new(r, String::new()))
                    .collect(),
            )
        })
        .collect();
    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let interval = doc
        .review_interval_months
        .or(doc.type_review_interval_months);
    Ok(DocSchedule {
        open_tasks,
        history: history
            .into_iter()
            .filter(|t| t.status != reviews::TaskStatus::Cancelled)
            .collect(),
        rules,
        opts: if can_edit {
            Some(RecipientOptions::load(state).await?)
        } else {
            None
        },
        missed: doc
            .next_review_date
            .map(|n| schedule::missed_cycles(n, interval, today))
            .unwrap_or_default(),
    })
}
