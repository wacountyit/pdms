// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Calendar events, dashboards, and subscribable ICS feeds.
//!
//! Events come from document dates, gathered through the authorized
//! document search, and from appointment dates (term ends and reappointment
//! decisions) of positions the viewer may see, so every view (calendar,
//! dashboard, ICS) shows only what the viewer may see.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use icalendar::{Calendar, Component, Event, EventLike};
use pdms_audit::{Actor, AuditEvent, AuthMethod, Category};
use pdms_authz::{Permission, Visibility};
use serde::Serialize;
use uuid::Uuid;

use crate::context::{ClientInfo, Ctx};
use crate::crypto::{new_token, sha256};
use crate::documents::DocumentStatus;
use crate::documents::search::{self, DocumentFilter, DocumentSummary};
use crate::error::{CoreError, CoreResult};
use crate::model::UserStatus;
use crate::notifications::rules::DueEvent;
use crate::{Core, appointments, schedule};

/// Documents considered at once for calendars and feeds.
const MAX_DOCUMENTS: i64 = 5000;

/// What a calendar event is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventSubject {
    Document(Uuid),
    Appointment {
        appointment_id: Uuid,
        position_id: Uuid,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct CalEvent {
    pub subject: EventSubject,
    /// A document's external ID, or the board's name.
    pub external_id: String,
    /// A document's title, or "person — position".
    pub title: String,
    pub department: String,
    pub type_name: String,
    pub kind: DueEvent,
    pub date: NaiveDate,
    /// A future review cycle computed from the interval, not a stored date.
    pub projected: bool,
    /// The document's status (appointments have none).
    pub status: Option<DocumentStatus>,
}

impl CalEvent {
    pub fn label(&self) -> &'static str {
        kind_label(self.kind)
    }

    pub fn document_id(&self) -> Option<Uuid> {
        match self.subject {
            EventSubject::Document(id) => Some(id),
            EventSubject::Appointment { .. } => None,
        }
    }

    pub fn href(&self) -> String {
        match self.subject {
            EventSubject::Document(id) => format!("/documents/{id}"),
            EventSubject::Appointment { position_id, .. } => format!("/positions/{position_id}"),
        }
    }
}

fn kind_label(kind: DueEvent) -> &'static str {
    match kind {
        DueEvent::Review => "Review",
        DueEvent::Renewal => "Renewal",
        DueEvent::Expiration => "Expires",
        DueEvent::TermEnd => "Term ends",
        DueEvent::Reappointment => "Reappointment due",
    }
}

impl From<appointments::AppointmentEvent> for CalEvent {
    fn from(e: appointments::AppointmentEvent) -> Self {
        CalEvent {
            subject: EventSubject::Appointment {
                appointment_id: e.appointment_id,
                position_id: e.position_id,
            },
            external_id: e.body_name,
            title: format!("{} — {}", e.person_name, e.position_name),
            department: e.department.unwrap_or_default(),
            type_name: "Appointment".into(),
            kind: e.kind,
            date: e.date,
            projected: false,
            status: None,
        }
    }
}

/// Whether a document filter leaves room for appointment events: only the
/// department and "mine" (positions the user manages) apply to them.
fn appointment_scope(filter: &DocumentFilter) -> Option<(Option<Uuid>, Option<Uuid>)> {
    let document_only = filter.q.as_deref().is_some_and(|q| !q.trim().is_empty())
        || filter.document_type_id.is_some()
        || filter.status.is_some()
        || !filter.tags.is_empty()
        || filter.date_field.is_some()
        || filter.custom.is_some();
    (!document_only).then_some((filter.department_id, filter.owner_id))
}

/// Appointment events in `[from, to]` that match the filter.
async fn appointment_events(
    core: &Core,
    ctx: &Ctx,
    filter: &DocumentFilter,
    from: NaiveDate,
    to: NaiveDate,
) -> CoreResult<Vec<CalEvent>> {
    let Some((dept, manager)) = appointment_scope(filter) else {
        return Ok(Vec::new());
    };
    Ok(appointments::events(core, ctx, dept, manager, from, to)
        .await?
        .into_iter()
        .map(CalEvent::from)
        .collect())
}

fn sort_events(out: &mut [CalEvent]) {
    out.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.external_id.cmp(&b.external_id))
            .then_with(|| a.title.cmp(&b.title))
    });
}

fn relevant(status: DocumentStatus) -> bool {
    !matches!(
        status,
        DocumentStatus::Superseded | DocumentStatus::Archived
    )
}

/// Expand documents into dated events within `[from, to]`.
pub fn expand(
    docs: &[DocumentSummary],
    from: NaiveDate,
    to: NaiveDate,
    projected: bool,
) -> Vec<CalEvent> {
    let mut out = Vec::new();
    for d in docs.iter().filter(|d| relevant(d.status)) {
        let mut push = |kind, date: NaiveDate, projected| {
            if date >= from && date <= to {
                out.push(CalEvent {
                    subject: EventSubject::Document(d.id),
                    external_id: d.external_id.clone(),
                    title: d.title.clone(),
                    department: d.department_name.clone(),
                    type_name: d.type_name.clone(),
                    kind,
                    date,
                    projected,
                    status: Some(d.status),
                });
            }
        };
        if let Some(next) = d.next_review_date {
            push(DueEvent::Review, next, false);
            if projected && let Some(m) = d.review_interval_months.filter(|m| *m > 0) {
                let mut k = 1;
                loop {
                    let date = schedule::add_months(next, m * k);
                    if date > to || k > 500 {
                        break;
                    }
                    push(DueEvent::Review, date, true);
                    k += 1;
                }
            }
        }
        if let Some(r) = d.renewal_date {
            push(DueEvent::Renewal, r, false);
        }
        if let Some(e) = d.expiration_date {
            push(DueEvent::Expiration, e, false);
        }
    }
    sort_events(&mut out);
    out
}

/// Events the caller may see in `[from, to]`, narrowed by `filter`.
pub async fn events(
    core: &Core,
    ctx: &Ctx,
    filter: &DocumentFilter,
    from: NaiveDate,
    to: NaiveDate,
    projected: bool,
) -> CoreResult<Vec<CalEvent>> {
    let docs = search::all(core, ctx, filter, Permission::View, MAX_DOCUMENTS).await?;
    let mut out = expand(&docs, from, to, projected);
    out.extend(appointment_events(core, ctx, filter, from, to).await?);
    sort_events(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Dashboards
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct OverdueItem {
    pub event: CalEvent,
    pub days_overdue: i64,
    /// Review due dates that passed without a review (reviews only).
    pub missed_cycles: Vec<NaiveDate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dashboard {
    pub today: NaiveDate,
    pub days: i64,
    pub upcoming: Vec<CalEvent>,
    pub overdue: Vec<OverdueItem>,
}

/// Upcoming dates in the next `days` days, and everything overdue.
pub async fn dashboard(
    core: &Core,
    ctx: &Ctx,
    filter: &DocumentFilter,
    days: i64,
) -> CoreResult<Dashboard> {
    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let docs = search::all(core, ctx, filter, Permission::View, MAX_DOCUMENTS).await?;
    let end = today + Duration::days(days.clamp(1, 3660));
    let mut upcoming = expand(&docs, today, end, false);
    upcoming.extend(appointment_events(core, ctx, filter, today, end).await?);
    sort_events(&mut upcoming);
    let yesterday = today - Duration::days(1);
    let mut past = expand(&docs, NaiveDate::MIN, yesterday, false);
    // Active appointments whose term ended or whose decision date passed:
    // holdovers and replacements still to be selected.
    past.extend(appointment_events(core, ctx, filter, NaiveDate::MIN, yesterday).await?);
    let mut overdue: Vec<OverdueItem> = past
        .into_iter()
        // Expiration in the past means Expired, which has its own status; list
        // reviews and renewals that were not done.
        .filter(|e| e.kind != DueEvent::Expiration && e.status != Some(DocumentStatus::Expired))
        .map(|e| {
            let interval = docs
                .iter()
                .find(|d| Some(d.id) == e.document_id())
                .and_then(|d| d.review_interval_months);
            OverdueItem {
                days_overdue: (today - e.date).num_days(),
                missed_cycles: if e.kind == DueEvent::Review {
                    schedule::missed_cycles(e.date, interval, today)
                } else {
                    Vec::new()
                },
                event: e,
            }
        })
        .collect();
    overdue.sort_by_key(|o| std::cmp::Reverse(o.days_overdue));
    Ok(Dashboard {
        today,
        days,
        upcoming,
        overdue,
    })
}

// ---------------------------------------------------------------------------
// ICS feeds
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct Feed {
    pub id: Uuid,
    pub name: String,
    pub department_id: Option<Uuid>,
    pub department_name: Option<String>,
    pub only_owned: bool,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

pub async fn list_feeds(core: &Core, ctx: &Ctx) -> CoreResult<Vec<Feed>> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    Ok(sqlx::query_as!(
        Feed,
        r#"SELECT f.id, f.name, f.department_id, d.name AS "department_name?", f.only_owned, f.created_at,
                  f.revoked_at, f.last_used_at
             FROM calendar_feeds f LEFT JOIN departments d ON d.id = f.department_id
            WHERE f.user_id = $1 ORDER BY f.revoked_at NULLS FIRST, f.created_at DESC"#,
        user
    )
    .fetch_all(&core.db)
    .await?)
}

/// Create a feed; returns its ID and the secret token (shown once).
pub async fn create_feed(
    core: &Core,
    ctx: &Ctx,
    name: &str,
    department_id: Option<Uuid>,
    only_owned: bool,
) -> CoreResult<(Uuid, String)> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(CoreError::validation("name must be 1–200 characters"));
    }
    let token = new_token();
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO calendar_feeds (id, user_id, name, department_id, only_owned, token_hash)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        user,
        name,
        department_id,
        only_owned,
        &sha256(token.as_bytes())[..]
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
            CoreError::validation("unknown department")
        }
        _ => CoreError::Db(e),
    })?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Access,
                "calendar_feed.created",
                ctx.actor.clone(),
                "calendar_feed",
            )
            .object(id, Some(name))
            .department(department_id)
            .context("only_owned", only_owned),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok((id, token))
}

/// Revoke one of the caller's feeds. Effective on the next fetch.
pub async fn revoke_feed(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let mut tx = core.db.begin().await?;
    let name = sqlx::query_scalar!(
        "UPDATE calendar_feeds SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL RETURNING name",
        id,
        user
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Access,
                "calendar_feed.revoked",
                ctx.actor.clone(),
                "calendar_feed",
            )
            .object(id, Some(&name)),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Render a feed by its token, with the owner's permissions as of now.
/// `None` for unknown or revoked tokens and inactive owners. Restricted
/// documents are never included (feed URLs end up on phones and in shared
/// calendars). Fetches are audited at most once an hour per feed.
pub async fn render_feed(
    core: &Core,
    token: &str,
    client: &ClientInfo,
) -> CoreResult<Option<String>> {
    let f = sqlx::query!(
        r#"SELECT f.id, f.name, f.user_id, f.department_id, f.only_owned, f.last_audited_at,
                  u.display_name, u.status AS "status: UserStatus"
             FROM calendar_feeds f JOIN users u ON u.id = f.user_id
            WHERE f.token_hash = $1 AND f.revoked_at IS NULL"#,
        &sha256(token.as_bytes())[..]
    )
    .fetch_optional(&core.db)
    .await?;
    let Some(f) = f else { return Ok(None) };
    if f.status != UserStatus::Active {
        return Ok(None);
    }
    let principal = crate::auth::principal::load(&core.db, f.user_id).await?;
    let ctx = Ctx::user(principal, &f.display_name, AuthMethod::IcsToken, client);
    let now = Utc::now();
    let today = schedule::local_today(now, core.config.timezone());
    let filter = DocumentFilter {
        department_id: f.department_id,
        owner_id: f.only_owned.then_some(f.user_id),
        ..DocumentFilter::default()
    };
    let docs: Vec<DocumentSummary> =
        search::all(core, &ctx, &filter, Permission::View, MAX_DOCUMENTS)
            .await?
            .into_iter()
            .filter(|d| d.visibility != Visibility::Restricted)
            .collect();

    let mut cal = Calendar::new();
    cal.name(&format!(
        "{}: {}",
        crate::branding::org_name(core).await,
        f.name
    ));
    cal.append_property(icalendar::Property::new(
        "PRODID",
        "-//PDMS//Document dates//EN",
    ));
    let host = core
        .config
        .server
        .public_url
        .host_str()
        .unwrap_or("pdms.local")
        .to_owned();
    let window_start = today - Duration::days(365);
    for d in &docs {
        let url = core
            .config
            .server
            .public_url
            .join(&format!("documents/{}", d.id))
            .map(|u| u.to_string())
            .unwrap_or_default();
        let mut add = |kind: DueEvent, date: NaiveDate, rrule: Option<String>| {
            if date < window_start && rrule.is_none() {
                return;
            }
            let label = match kind {
                DueEvent::Review => "Review due",
                DueEvent::Renewal => "Renewal due",
                other => kind_label(other),
            };
            let mut ev = Event::new();
            ev.uid(&format!("{}-{}@{host}", d.id, kind.as_str()))
                .summary(&format!("{label}: {} {}", d.external_id, d.title))
                .description(&format!(
                    "{} · {} · {}\n{url}",
                    d.type_name,
                    d.department_name,
                    d.status.label()
                ))
                .url(&url)
                .starts(date)
                .ends(date + Duration::days(1))
                .timestamp(now);
            if let Some(r) = rrule {
                ev.add_property("RRULE", r);
            }
            cal.push(ev.done());
        };
        if let Some(next) = d.next_review_date {
            let rrule = d
                .review_interval_months
                .filter(|m| *m > 0)
                .map(|m| format!("FREQ=MONTHLY;INTERVAL={m}"));
            add(DueEvent::Review, next, rrule);
        }
        if let Some(r) = d.renewal_date {
            add(DueEvent::Renewal, r, None);
        }
        if let Some(e) = d.expiration_date {
            add(DueEvent::Expiration, e, None);
        }
    }
    let appts = appointments::events(
        core,
        &ctx,
        f.department_id,
        f.only_owned.then_some(f.user_id),
        window_start,
        today + Duration::days(3660),
    )
    .await?;
    for e in &appts {
        let url = core
            .config
            .server
            .public_url
            .join(&format!("positions/{}", e.position_id))
            .map(|u| u.to_string())
            .unwrap_or_default();
        let mut ev = Event::new();
        ev.uid(&format!("{}-{}@{host}", e.appointment_id, e.kind.as_str()))
            .summary(&format!(
                "{}: {} ({}, {})",
                kind_label(e.kind),
                e.person_name,
                e.position_name,
                e.body_name
            ))
            .description(&format!(
                "Appointment · {}\n{url}",
                e.department.clone().unwrap_or_default()
            ))
            .url(&url)
            .starts(e.date)
            .ends(e.date + Duration::days(1))
            .timestamp(now);
        cal.push(ev.done());
    }
    let body = cal.done().to_string();

    let audit_due = f
        .last_audited_at
        .is_none_or(|t| now - t > Duration::hours(1));
    sqlx::query!(
        "UPDATE calendar_feeds SET last_used_at = now(),
                last_audited_at = CASE WHEN $2 THEN now() ELSE last_audited_at END WHERE id = $1",
        f.id,
        audit_due
    )
    .execute(&core.db)
    .await?;
    if audit_due {
        let actor = Actor {
            user_id: Some(f.user_id),
            display: Some(f.display_name.clone()),
            auth_method: AuthMethod::IcsToken,
            ip: client.ip,
            user_agent: client.user_agent.clone(),
            api_token_id: None,
            request_id: client.request_id.clone(),
        };
        pdms_audit::record(
            &core.db,
            vec![
                AuditEvent::new(
                    Category::Access,
                    "calendar_feed.accessed",
                    actor,
                    "calendar_feed",
                )
                .object(f.id, Some(&f.name))
                .context("documents", i64::try_from(docs.len()).unwrap_or(i64::MAX))
                .context(
                    "appointment_events",
                    i64::try_from(appts.len()).unwrap_or(i64::MAX),
                ),
            ],
        )
        .await?;
    }
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::types::Json;

    fn doc(
        next: Option<&str>,
        interval: Option<i32>,
        renewal: Option<&str>,
        expiration: Option<&str>,
        status: DocumentStatus,
    ) -> DocumentSummary {
        let d = |s: Option<&str>| s.map(|s| s.parse().unwrap());
        DocumentSummary {
            id: Uuid::nil(),
            external_id: "DOC-1".into(),
            title: "T".into(),
            type_name: "Policy".into(),
            department_id: Uuid::nil(),
            department_name: "IT".into(),
            owner_name: "O".into(),
            status,
            execution_date: None,
            effective_date: None,
            last_review_date: None,
            next_review_date: d(next),
            renewal_date: d(renewal),
            expiration_date: d(expiration),
            review_interval_months: interval,
            visibility: Visibility::Department,
            custom_fields: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            tags: Json(vec![]),
            total: 1,
        }
    }

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn reviews_project_forward_and_dates_are_windowed() {
        let docs = vec![doc(
            Some("2026-11-01"),
            Some(12),
            Some("2027-03-01"),
            Some("2030-01-01"),
            DocumentStatus::Active,
        )];
        let ev = expand(&docs, day("2026-10-01"), day("2028-12-31"), true);
        let summary: Vec<(DueEvent, String, bool)> = ev
            .iter()
            .map(|e| (e.kind, e.date.to_string(), e.projected))
            .collect();
        assert_eq!(
            summary,
            vec![
                (DueEvent::Review, "2026-11-01".into(), false),
                (DueEvent::Renewal, "2027-03-01".into(), false),
                (DueEvent::Review, "2027-11-01".into(), true),
                (DueEvent::Review, "2028-11-01".into(), true),
            ]
        );
        assert_eq!(
            expand(&docs, day("2026-10-01"), day("2028-12-31"), false).len(),
            2
        );
    }

    #[test]
    fn no_expiration_date_means_no_expiration_event() {
        // Acceptance test 6 (calendar side).
        let docs = vec![doc(None, None, None, None, DocumentStatus::Active)];
        assert!(expand(&docs, NaiveDate::MIN, NaiveDate::MAX, true).is_empty());
    }

    #[test]
    fn archived_and_superseded_are_left_out() {
        let docs = vec![
            doc(
                Some("2026-11-01"),
                None,
                None,
                None,
                DocumentStatus::Archived,
            ),
            doc(
                Some("2026-11-01"),
                None,
                None,
                None,
                DocumentStatus::Superseded,
            ),
        ];
        assert!(expand(&docs, NaiveDate::MIN, NaiveDate::MAX, true).is_empty());
    }
}
