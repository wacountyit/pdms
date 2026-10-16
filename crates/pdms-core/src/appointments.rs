// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Positions (seats on boards, committees, and commissions, or appointed
//! roles) and the people appointed to them.
//!
//! Every appointment is kept: reappointing or replacing someone ends the
//! current appointment and starts a new one linked to it, so a position's
//! history is the full list of its appointees. Each position has a
//! requirements checklist ("Acceptance letter on file", "Appointed by the
//! board"); every appointment tracks each item with who satisfied it and when.
//!
//! Who may do what (decided in `pdms-authz`):
//! - appointment managers (admin, appointments clerk) manage everything;
//! - a position's manager and members of its department see it, and the
//!   manager records checklist items;
//! - auditors see everything.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::{Capability, PositionAttrs, Principal, can_track_position, can_view_position};
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::notifications::rules::DueEvent;
use crate::{Core, schedule};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "appointment_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AppointmentStatus {
    Active,
    Ended,
}

#[derive(Clone, Debug, Serialize)]
pub struct Position {
    pub id: Uuid,
    pub name: String,
    pub body_name: String,
    pub department_id: Option<Uuid>,
    pub department_name: Option<String>,
    pub appointing_authority: String,
    pub term_months: Option<i32>,
    pub seats: i32,
    pub manager_user_id: Option<Uuid>,
    pub manager_name: Option<String>,
    pub reappointment_lead_days: i32,
    pub description: String,
    pub is_active: bool,
    /// Active appointments.
    pub filled: i64,
}

impl Position {
    pub fn attrs(&self) -> PositionAttrs {
        PositionAttrs {
            department_id: self.department_id,
            manager_user_id: self.manager_user_id,
        }
    }

    /// "Seat 3, Advisory Committee".
    pub fn label(&self) -> String {
        format!("{}, {}", self.name, self.body_name)
    }

    pub fn vacancies(&self) -> i64 {
        (i64::from(self.seats) - self.filled).max(0)
    }

    pub fn term_label(&self) -> String {
        match self.term_months {
            Some(m) if m % 12 == 0 && m >= 12 => {
                let y = m / 12;
                if y == 1 {
                    "1 year".into()
                } else {
                    format!("{y} years")
                }
            }
            Some(1) => "1 month".into(),
            Some(m) => format!("{m} months"),
            None => "No fixed term".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Requirement {
    pub id: Uuid,
    pub position_id: Uuid,
    pub label: String,
    pub description: String,
    pub sort_order: i32,
    pub is_active: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequirementCheck {
    pub requirement_id: Uuid,
    pub label: String,
    pub description: String,
    pub satisfied: bool,
    pub satisfied_by: Option<String>,
    pub satisfied_at: Option<DateTime<Utc>>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Appointment {
    pub id: Uuid,
    pub position_id: Uuid,
    pub person_name: String,
    pub user_id: Option<Uuid>,
    pub contact_email: String,
    pub appointed_by: String,
    pub appointment_reference: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub reappointment_due_date: Option<NaiveDate>,
    pub status: AppointmentStatus,
    pub ended_on: Option<NaiveDate>,
    pub end_reason: String,
    pub predecessor_id: Option<Uuid>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub requirements: Vec<RequirementCheck>,
}

impl Appointment {
    /// Where the appointment stands as of `today`.
    pub fn standing(&self, today: NaiveDate) -> &'static str {
        match (self.status, self.end_date) {
            (AppointmentStatus::Ended, _) => "Ended",
            (AppointmentStatus::Active, Some(end)) if end < today => "Holdover (term ended)",
            (AppointmentStatus::Active, _)
                if self.reappointment_due_date.is_some_and(|d| d <= today) =>
            {
                "Reappointment decision due"
            }
            (AppointmentStatus::Active, _) => "Serving",
        }
    }

    pub fn requirements_met(&self) -> usize {
        self.requirements.iter().filter(|r| r.satisfied).count()
    }
}

/// The fields of a position an appointment manager sets.
#[derive(Clone, Debug, Default)]
pub struct PositionSpec {
    pub name: String,
    pub body_name: String,
    pub department_id: Option<Uuid>,
    pub appointing_authority: String,
    pub term_months: Option<i32>,
    pub seats: i32,
    pub manager_user_id: Option<Uuid>,
    pub reappointment_lead_days: i32,
    pub description: String,
    pub is_active: bool,
}

/// A new appointment (also used when replacing someone).
#[derive(Clone, Debug, Default)]
pub struct NewAppointment {
    pub person_name: String,
    /// When the appointee has an account here.
    pub user_id: Option<Uuid>,
    pub contact_email: String,
    pub appointed_by: String,
    /// E.g. the resolution or meeting reference.
    pub appointment_reference: String,
    pub start_date: NaiveDate,
    /// Defaults to the start date plus the position's term.
    pub end_date: Option<NaiveDate>,
    /// Defaults to the end date minus the position's lead time.
    pub reappointment_due_date: Option<NaiveDate>,
    pub notes: String,
}

/// A new term for the same person.
#[derive(Clone, Debug, Default)]
pub struct Term {
    /// Defaults to the day after the current term ends (or today).
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub reappointment_due_date: Option<NaiveDate>,
    pub appointed_by: String,
    pub appointment_reference: String,
    pub notes: String,
}

/// Corrections to an appointment's details.
#[derive(Clone, Debug, Default)]
pub struct AppointmentEdit {
    pub person_name: String,
    pub user_id: Option<Uuid>,
    pub contact_email: String,
    pub appointed_by: String,
    pub appointment_reference: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub reappointment_due_date: Option<NaiveDate>,
    pub notes: String,
}

// ---------------------------------------------------------------------------
// Loading and authorization
// ---------------------------------------------------------------------------

async fn load_positions(conn: &mut PgConnection, only: Option<Uuid>) -> CoreResult<Vec<Position>> {
    Ok(sqlx::query_as!(
        Position,
        r#"SELECT p.id, p.name, p.body_name, p.department_id, d.name AS "department_name?",
                  p.appointing_authority, p.term_months, p.seats, p.manager_user_id,
                  u.display_name AS "manager_name?", p.reappointment_lead_days, p.description, p.is_active,
                  (SELECT count(*) FROM appointments a WHERE a.position_id = p.id AND a.status = 'active') AS "filled!"
             FROM positions p
             LEFT JOIN departments d ON d.id = p.department_id
             LEFT JOIN users u ON u.id = p.manager_user_id
            WHERE ($1::uuid IS NULL OR p.id = $1)
            ORDER BY p.body_name, p.name"#,
        only
    )
    .fetch_all(conn)
    .await?)
}

async fn load_position(core: &Core, id: Uuid) -> CoreResult<Position> {
    let mut conn = core.db.acquire().await?;
    load_positions(&mut conn, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(CoreError::NotFound)
}

fn position_event(ctx: &Ctx, category: Category, action: &str, p: &Position) -> AuditEvent {
    AuditEvent::new(category, action, ctx.actor.clone(), "position")
        .object(p.id, Some(&p.label()))
        .department(p.department_id)
}

fn appointment_event(ctx: &Ctx, action: &str, p: &Position, id: Uuid, person: &str) -> AuditEvent {
    AuditEvent::new(Category::Workflow, action, ctx.actor.clone(), "appointment")
        .object(id, Some(&format!("{person} — {}", p.label())))
        .department(p.department_id)
        .context("position_id", p.id)
}

fn visible(ctx: &Ctx, p: &Position) -> bool {
    ctx.principal()
        .is_none_or(|pr| can_view_position(pr, &p.attrs()))
}

fn may_track(ctx: &Ctx, p: &Position) -> bool {
    ctx.principal()
        .is_none_or(|pr| can_track_position(pr, &p.attrs()))
}

async fn deny(core: &Core, ctx: &Ctx, p: &Position, needed: &str) -> CoreError {
    let e = position_event(ctx, Category::Access, "access.denied", p)
        .outcome(Outcome::Denied)
        .context("required", needed);
    match pdms_audit::record(&core.db, vec![e]).await {
        Ok(()) => CoreError::Forbidden(None),
        Err(err) => err.into(),
    }
}

async fn require_view(core: &Core, ctx: &Ctx, p: &Position) -> CoreResult<()> {
    if visible(ctx, p) {
        Ok(())
    } else {
        Err(deny(core, ctx, p, "view_position").await)
    }
}

async fn require_manage(core: &Core, ctx: &Ctx, p: &Position) -> CoreResult<()> {
    if ctx.can(Capability::ManageAppointments) {
        Ok(())
    } else {
        Err(deny(core, ctx, p, "manage_appointments").await)
    }
}

/// Whether the caller may see the appointment's position (not audited; for filtering).
pub async fn can_view_appointment(
    core: &Core,
    ctx: &Ctx,
    appointment_id: Uuid,
) -> CoreResult<bool> {
    match ctx.principal() {
        None => Ok(true),
        Some(p) => principal_can_view_appointment(core, p, appointment_id).await,
    }
}

/// Whether the caller may see a position (not audited; for filtering).
pub async fn can_view_position_id(core: &Core, ctx: &Ctx, position_id: Uuid) -> CoreResult<bool> {
    let Some(p) = ctx.principal() else {
        return Ok(true);
    };
    let row = sqlx::query!(
        "SELECT department_id, manager_user_id FROM positions WHERE id = $1",
        position_id
    )
    .fetch_optional(&core.db)
    .await?;
    Ok(row.is_some_and(|r| {
        can_view_position(
            p,
            &PositionAttrs {
                department_id: r.department_id,
                manager_user_id: r.manager_user_id,
            },
        )
    }))
}

pub(crate) async fn principal_can_view_appointment(
    core: &Core,
    principal: &Principal,
    appointment_id: Uuid,
) -> CoreResult<bool> {
    let row = sqlx::query!(
        "SELECT p.department_id, p.manager_user_id FROM appointments a JOIN positions p ON p.id = a.position_id
          WHERE a.id = $1",
        appointment_id
    )
    .fetch_optional(&core.db)
    .await?;
    Ok(row.is_some_and(|r| {
        can_view_position(
            principal,
            &PositionAttrs {
                department_id: r.department_id,
                manager_user_id: r.manager_user_id,
            },
        )
    }))
}

async fn load_requirements(core: &Core, position_id: Uuid) -> CoreResult<Vec<Requirement>> {
    Ok(sqlx::query_as!(
        Requirement,
        "SELECT id, position_id, label, description, sort_order, is_active
           FROM position_requirements WHERE position_id = $1 ORDER BY sort_order, created_at",
        position_id
    )
    .fetch_all(&core.db)
    .await?)
}

async fn load_appointments(
    core: &Core,
    position_id: Option<Uuid>,
    only: Option<Uuid>,
) -> CoreResult<Vec<Appointment>> {
    let rows = sqlx::query!(
        r#"SELECT id, position_id, person_name, user_id, contact_email, appointed_by, appointment_reference,
                  start_date, end_date, reappointment_due_date, status AS "status: AppointmentStatus", ended_on,
                  end_reason, predecessor_id, notes, created_at
             FROM appointments
            WHERE ($1::uuid IS NULL OR position_id = $1) AND ($2::uuid IS NULL OR id = $2)
            ORDER BY (status = 'active') DESC, start_date DESC, created_at DESC"#,
        position_id,
        only
    )
    .fetch_all(&core.db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    // Active requirements, plus inactive ones an appointment already has a record for.
    let checks = sqlx::query!(
        r#"SELECT a.id AS appointment_id, r.id AS requirement_id, r.label, r.description,
                  coalesce(s.satisfied, false) AS "satisfied!", u.display_name AS "satisfied_by?",
                  s.satisfied_at, coalesce(s.note, '') AS "note!"
             FROM appointments a
             JOIN position_requirements r ON r.position_id = a.position_id
             LEFT JOIN appointment_requirement_status s ON s.appointment_id = a.id AND s.requirement_id = r.id
             LEFT JOIN users u ON u.id = s.satisfied_by
            WHERE a.id = ANY($1) AND (r.is_active OR s.satisfied)
            ORDER BY r.sort_order, r.created_at"#,
        &ids
    )
    .fetch_all(&core.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Appointment {
            requirements: checks
                .iter()
                .filter(|c| c.appointment_id == r.id)
                .map(|c| RequirementCheck {
                    requirement_id: c.requirement_id,
                    label: c.label.clone(),
                    description: c.description.clone(),
                    satisfied: c.satisfied,
                    satisfied_by: c.satisfied_by.clone(),
                    satisfied_at: c.satisfied_at,
                    note: c.note.clone(),
                })
                .collect(),
            id: r.id,
            position_id: r.position_id,
            person_name: r.person_name,
            user_id: r.user_id,
            contact_email: r.contact_email,
            appointed_by: r.appointed_by,
            appointment_reference: r.appointment_reference,
            start_date: r.start_date,
            end_date: r.end_date,
            reappointment_due_date: r.reappointment_due_date,
            status: r.status,
            ended_on: r.ended_on,
            end_reason: r.end_reason,
            predecessor_id: r.predecessor_id,
            notes: r.notes,
            created_at: r.created_at,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Positions the caller may see.
pub async fn list_positions(
    core: &Core,
    ctx: &Ctx,
    include_inactive: bool,
) -> CoreResult<Vec<Position>> {
    let mut conn = core.db.acquire().await?;
    Ok(load_positions(&mut conn, None)
        .await?
        .into_iter()
        .filter(|p| (include_inactive || p.is_active) && visible(ctx, p))
        .collect())
}

/// Whether the caller can see any position (for showing the menu item).
pub async fn any_visible(core: &Core, ctx: &Ctx) -> CoreResult<bool> {
    if ctx.can(Capability::ViewAllAppointments) {
        return Ok(true);
    }
    Ok(!list_positions(core, ctx, false).await?.is_empty())
}

/// What needs attention on a seat, worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeatState {
    /// The reappointment decision date has passed and the term has not ended.
    DecisionOverdue,
    /// The reappointment decision date is today.
    DecisionDueToday,
    /// The term ended and the person still serves.
    Holdover,
    Vacant,
    /// The term ends on this date, soon.
    TermEnding(NaiveDate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatAttention {
    pub position_id: Uuid,
    pub label: String,
    pub state: SeatState,
}

/// The one place that decides what an active appointment needs, worst state
/// first when several apply. `horizon` is the last day a term end still counts
/// as "ending"; `None` ignores term ends.
fn classify(a: &Appointment, today: NaiveDate, horizon: Option<NaiveDate>) -> Option<SeatState> {
    if a.reappointment_due_date.is_some_and(|d| d < today) {
        Some(SeatState::DecisionOverdue)
    } else if a.reappointment_due_date == Some(today) {
        Some(SeatState::DecisionDueToday)
    } else if a.end_date.is_some_and(|e| e < today) {
        Some(SeatState::Holdover)
    } else {
        a.end_date
            .filter(|e| horizon.is_some_and(|h| *e <= h))
            .map(SeatState::TermEnding)
    }
}

/// Unfilled seats of an active position; the one vacancy rule.
fn vacant_seats(p: &Position) -> i64 {
    if p.is_active { p.vacancies() } else { 0 }
}

/// The single word the positions list shows for a position: a decision due,
/// a holdover, a vacancy, or nothing. `current` are its active appointments.
pub fn list_flag(p: &Position, current: &[&Appointment], today: NaiveDate) -> &'static str {
    let worst = current
        .iter()
        .filter_map(|a| classify(a, today, None))
        .min();
    match worst {
        Some(SeatState::DecisionOverdue | SeatState::DecisionDueToday) => "Decision due",
        Some(SeatState::Holdover) => "Holdover",
        _ if vacant_seats(p) > 0 => "Vacant",
        _ => "",
    }
}

/// Seats the caller can see that need attention as of `today`: reappointment
/// decisions due, holdovers, vacancies, and terms ending within `within_days`.
/// Worst first, then by label.
pub async fn attention(
    core: &Core,
    ctx: &Ctx,
    today: NaiveDate,
    within_days: i64,
) -> CoreResult<Vec<SeatAttention>> {
    let horizon = Some(today + Duration::days(within_days));
    let mut rows = Vec::new();
    for p in list_positions(core, ctx, false).await? {
        let label = p.label();
        let appts = load_appointments(core, Some(p.id), None).await?;
        for a in appts
            .iter()
            .filter(|a| a.status == AppointmentStatus::Active)
        {
            if let Some(state) = classify(a, today, horizon) {
                rows.push(SeatAttention {
                    position_id: p.id,
                    label: label.clone(),
                    state,
                });
            }
        }
        let vacant = vacant_seats(&p);
        if vacant > 0 {
            rows.push(SeatAttention {
                position_id: p.id,
                label: if vacant > 1 {
                    format!("{label} ({vacant} vacant)")
                } else {
                    label.clone()
                },
                state: SeatState::Vacant,
            });
        }
    }
    rows.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.label.cmp(&b.label)));
    Ok(rows)
}

#[derive(Clone, Debug, Serialize)]
pub struct PositionDetail {
    pub position: Position,
    pub requirements: Vec<Requirement>,
    /// Active appointments first, then history, newest first.
    pub appointments: Vec<Appointment>,
    pub can_manage: bool,
    pub can_track: bool,
}

pub async fn position(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<PositionDetail> {
    let p = load_position(core, id).await?;
    require_view(core, ctx, &p).await?;
    Ok(PositionDetail {
        requirements: load_requirements(core, id).await?,
        appointments: load_appointments(core, Some(id), None).await?,
        can_manage: ctx.can(Capability::ManageAppointments),
        can_track: may_track(ctx, &p),
        position: p,
    })
}

/// One appointment and its position.
pub async fn appointment(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<(Position, Appointment)> {
    let a = load_appointments(core, None, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(CoreError::NotFound)?;
    let p = load_position(core, a.position_id).await?;
    require_view(core, ctx, &p).await?;
    Ok((p, a))
}

/// A dated appointment event for calendars and dashboards.
#[derive(Clone, Debug, Serialize)]
pub struct AppointmentEvent {
    pub appointment_id: Uuid,
    pub position_id: Uuid,
    pub person_name: String,
    pub position_name: String,
    pub body_name: String,
    pub department: Option<String>,
    pub kind: DueEvent,
    pub date: NaiveDate,
}

/// Term-end and reappointment dates of active appointments the caller may
/// see, within `[from, to]`, optionally limited to one department or to
/// positions managed by one user.
pub async fn events(
    core: &Core,
    ctx: &Ctx,
    department_id: Option<Uuid>,
    managed_by: Option<Uuid>,
    from: NaiveDate,
    to: NaiveDate,
) -> CoreResult<Vec<AppointmentEvent>> {
    // Callers may pass open-ended bounds; keep them within what Postgres stores.
    let floor = NaiveDate::from_ymd_opt(1000, 1, 1).unwrap_or(from);
    let ceiling = NaiveDate::from_ymd_opt(9999, 12, 31).unwrap_or(to);
    let (from, to) = (from.clamp(floor, ceiling), to.clamp(floor, ceiling));
    let rows = sqlx::query!(
        r#"SELECT a.id, a.position_id, a.person_name, a.end_date, a.reappointment_due_date,
                  p.name, p.body_name, p.department_id, p.manager_user_id, d.name AS "department?"
             FROM appointments a
             JOIN positions p ON p.id = a.position_id
             LEFT JOIN departments d ON d.id = p.department_id
            WHERE a.status = 'active' AND p.is_active
              AND ($1::uuid IS NULL OR p.department_id = $1)
              AND ($2::uuid IS NULL OR p.manager_user_id = $2)
              AND (a.end_date BETWEEN $3 AND $4 OR a.reappointment_due_date BETWEEN $3 AND $4)"#,
        department_id,
        managed_by,
        from,
        to
    )
    .fetch_all(&core.db)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let attrs = PositionAttrs {
            department_id: r.department_id,
            manager_user_id: r.manager_user_id,
        };
        if ctx
            .principal()
            .is_some_and(|p| !can_view_position(p, &attrs))
        {
            continue;
        }
        for (kind, date) in [
            (DueEvent::TermEnd, r.end_date),
            (DueEvent::Reappointment, r.reappointment_due_date),
        ] {
            if let Some(date) = date.filter(|d| *d >= from && *d <= to) {
                out.push(AppointmentEvent {
                    appointment_id: r.id,
                    position_id: r.position_id,
                    person_name: r.person_name.clone(),
                    position_name: r.name.clone(),
                    body_name: r.body_name.clone(),
                    department: r.department.clone(),
                    kind,
                    date,
                });
            }
        }
    }
    out.sort_by_key(|e| e.date);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Positions and requirements
// ---------------------------------------------------------------------------

fn trimmed(s: &str, max: usize, what: &str, required: bool) -> CoreResult<String> {
    let t = s.trim();
    if required && t.is_empty() {
        return Err(CoreError::validation(format!("{what} is required")));
    }
    if t.chars().count() > max {
        return Err(CoreError::validation(format!(
            "{what} must be at most {max} characters"
        )));
    }
    Ok(t.to_owned())
}

fn check_spec(s: &mut PositionSpec) -> CoreResult<()> {
    s.name = trimmed(&s.name, 200, "the position name", true)?;
    s.body_name = trimmed(&s.body_name, 200, "the board or body", true)?;
    s.appointing_authority = trimmed(
        &s.appointing_authority,
        200,
        "the appointing authority",
        false,
    )?;
    s.description = trimmed(&s.description, 4000, "the description", false)?;
    if s.term_months.is_some_and(|m| !(1..=600).contains(&m)) {
        return Err(CoreError::validation("the term must be 1–600 months"));
    }
    if !(1..=500).contains(&s.seats) {
        return Err(CoreError::validation("seats must be 1–500"));
    }
    if !(0..=3650).contains(&s.reappointment_lead_days) {
        return Err(CoreError::validation(
            "the reappointment lead time must be 0–3650 days",
        ));
    }
    Ok(())
}

fn fk_error(e: sqlx::Error) -> CoreError {
    match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
            CoreError::validation("a chosen department or user no longer exists")
        }
        _ => CoreError::Db(e),
    }
}

fn spec_changes(e: AuditEvent, before: Option<&Position>, s: &PositionSpec) -> AuditEvent {
    e.change("name", before.map(|b| b.name.clone()), s.name.clone())
        .change(
            "body_name",
            before.map(|b| b.body_name.clone()),
            s.body_name.clone(),
        )
        .change(
            "department_id",
            before.and_then(|b| b.department_id),
            s.department_id,
        )
        .change(
            "appointing_authority",
            before.map(|b| b.appointing_authority.clone()),
            s.appointing_authority.clone(),
        )
        .change(
            "term_months",
            before.and_then(|b| b.term_months),
            s.term_months,
        )
        .change("seats", before.map(|b| b.seats), s.seats)
        .change(
            "manager_user_id",
            before.and_then(|b| b.manager_user_id),
            s.manager_user_id,
        )
        .change(
            "reappointment_lead_days",
            before.map(|b| b.reappointment_lead_days),
            s.reappointment_lead_days,
        )
        .change(
            "description",
            before.map(|b| b.description.clone()),
            s.description.clone(),
        )
        .change("is_active", before.map(|b| b.is_active), s.is_active)
}

pub async fn create_position(core: &Core, ctx: &Ctx, mut spec: PositionSpec) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageAppointments)?;
    check_spec(&mut spec)?;
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO positions (id, name, body_name, department_id, appointing_authority, term_months, seats,
                                manager_user_id, reappointment_lead_days, description, is_active)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        id,
        spec.name,
        spec.body_name,
        spec.department_id,
        spec.appointing_authority,
        spec.term_months,
        spec.seats,
        spec.manager_user_id,
        spec.reappointment_lead_days,
        spec.description,
        spec.is_active
    )
    .execute(&mut *tx)
    .await
    .map_err(fk_error)?;
    let e = AuditEvent::new(
        Category::Admin,
        "position.created",
        ctx.actor.clone(),
        "position",
    )
    .object(id, Some(&format!("{}, {}", spec.name, spec.body_name)))
    .department(spec.department_id);
    pdms_audit::append(&mut tx, vec![spec_changes(e, None, &spec)]).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update_position(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    mut spec: PositionSpec,
) -> CoreResult<()> {
    let before = load_position(core, id).await?;
    require_manage(core, ctx, &before).await?;
    check_spec(&mut spec)?;
    if i64::from(spec.seats) < before.filled {
        return Err(CoreError::validation(format!(
            "{} people currently hold this position; end appointments before reducing the seats",
            before.filled
        )));
    }
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE positions SET name = $2, body_name = $3, department_id = $4, appointing_authority = $5,
                term_months = $6, seats = $7, manager_user_id = $8, reappointment_lead_days = $9,
                description = $10, is_active = $11, updated_at = now()
          WHERE id = $1",
        id,
        spec.name,
        spec.body_name,
        spec.department_id,
        spec.appointing_authority,
        spec.term_months,
        spec.seats,
        spec.manager_user_id,
        spec.reappointment_lead_days,
        spec.description,
        spec.is_active
    )
    .execute(&mut *tx)
    .await
    .map_err(fk_error)?;
    let e = spec_changes(
        position_event(ctx, Category::Admin, "position.updated", &before),
        Some(&before),
        &spec,
    );
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Add a checklist item to a position. Current appointments start with it unsatisfied.
pub async fn add_requirement(
    core: &Core,
    ctx: &Ctx,
    position_id: Uuid,
    label: &str,
    description: &str,
) -> CoreResult<Uuid> {
    let p = load_position(core, position_id).await?;
    require_manage(core, ctx, &p).await?;
    let label = trimmed(label, 200, "the requirement", true)?;
    let description = trimmed(description, 2000, "the description", false)?;
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO position_requirements (id, position_id, label, description, sort_order)
         VALUES ($1, $2, $3, $4,
                 (SELECT coalesce(max(sort_order), 0) + 10 FROM position_requirements WHERE position_id = $2))",
        id,
        position_id,
        label,
        description
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            position_event(ctx, Category::Admin, "position.requirement_added", &p)
                .context("requirement_id", id)
                .change("label", None::<String>, label),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Rename, describe, or retire a checklist item. Retired items keep the
/// records already made against them.
pub async fn update_requirement(
    core: &Core,
    ctx: &Ctx,
    requirement_id: Uuid,
    label: &str,
    description: &str,
    is_active: bool,
) -> CoreResult<()> {
    let before = sqlx::query!(
        "SELECT position_id, label, description, is_active FROM position_requirements WHERE id = $1",
        requirement_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let p = load_position(core, before.position_id).await?;
    require_manage(core, ctx, &p).await?;
    let label = trimmed(label, 200, "the requirement", true)?;
    let description = trimmed(description, 2000, "the description", false)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE position_requirements SET label = $2, description = $3, is_active = $4 WHERE id = $1",
        requirement_id,
        label,
        description,
        is_active
    )
    .execute(&mut *tx)
    .await?;
    let e = position_event(ctx, Category::Admin, "position.requirement_updated", &p)
        .context("requirement_id", requirement_id)
        .change("label", before.label, label)
        .change("description", before.description, description)
        .change("is_active", before.is_active, is_active);
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Appointments
// ---------------------------------------------------------------------------

struct Dates {
    start: NaiveDate,
    end: Option<NaiveDate>,
    reappointment: Option<NaiveDate>,
}

/// Fill in the end and reappointment dates from the position's defaults.
fn dates(
    p: &Position,
    start: NaiveDate,
    end: Option<NaiveDate>,
    reappointment: Option<NaiveDate>,
) -> CoreResult<Dates> {
    let end = end.or_else(|| {
        p.term_months
            .map(|m| schedule::add_months(start, m) - Duration::days(1))
    });
    if end.is_some_and(|e| e < start) {
        return Err(CoreError::validation(
            "the term cannot end before it starts",
        ));
    }
    let reappointment = reappointment.or_else(|| {
        end.map(|e| (e - Duration::days(i64::from(p.reappointment_lead_days))).max(start))
    });
    if reappointment.is_some_and(|r| r < start) {
        return Err(CoreError::validation(
            "the reappointment decision date cannot be before the term starts",
        ));
    }
    Ok(Dates {
        start,
        end,
        reappointment,
    })
}

fn check_person(a: &mut NewAppointment) -> CoreResult<()> {
    a.person_name = trimmed(&a.person_name, 200, "the person's name", true)?;
    a.contact_email = trimmed(&a.contact_email, 320, "the email address", false)?;
    if !a.contact_email.is_empty() && !a.contact_email.contains('@') {
        return Err(CoreError::validation("enter a valid email address"));
    }
    a.appointed_by = trimmed(&a.appointed_by, 200, "the appointing authority", false)?;
    a.appointment_reference = trimmed(&a.appointment_reference, 200, "the reference", false)?;
    a.notes = trimmed(&a.notes, 4000, "the notes", false)?;
    Ok(())
}

/// Insert an appointment and its checklist; returns the audit entry.
async fn insert(
    tx: &mut PgConnection,
    ctx: &Ctx,
    p: &Position,
    a: &NewAppointment,
    d: &Dates,
    predecessor: Option<Uuid>,
    action: &str,
) -> CoreResult<(Uuid, AuditEvent)> {
    let id = Uuid::now_v7();
    let appointed_by = if a.appointed_by.is_empty() {
        p.appointing_authority.clone()
    } else {
        a.appointed_by.clone()
    };
    sqlx::query!(
        "INSERT INTO appointments (id, position_id, person_name, user_id, contact_email, appointed_by,
                                   appointment_reference, start_date, end_date, reappointment_due_date,
                                   predecessor_id, notes, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        id,
        p.id,
        a.person_name,
        a.user_id,
        a.contact_email,
        appointed_by,
        a.appointment_reference,
        d.start,
        d.end,
        d.reappointment,
        predecessor,
        a.notes,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await
    .map_err(fk_error)?;
    sqlx::query!(
        "INSERT INTO appointment_requirement_status (appointment_id, requirement_id)
         SELECT $1, id FROM position_requirements WHERE position_id = $2 AND is_active",
        id,
        p.id
    )
    .execute(&mut *tx)
    .await?;
    let mut e = appointment_event(ctx, action, p, id, &a.person_name)
        .change("person_name", None::<String>, a.person_name.clone())
        .change("user_id", None::<Uuid>, a.user_id)
        .change("appointed_by", None::<String>, appointed_by)
        .change(
            "appointment_reference",
            None::<String>,
            a.appointment_reference.clone(),
        )
        .change("start_date", None::<NaiveDate>, d.start)
        .change("end_date", None::<NaiveDate>, d.end)
        .change("reappointment_due_date", None::<NaiveDate>, d.reappointment);
    if let Some(prev) = predecessor {
        e = e.context("predecessor_id", prev);
    }
    Ok((id, e))
}

/// End an active appointment inside a transaction; returns the audit entry.
async fn close(
    tx: &mut PgConnection,
    ctx: &Ctx,
    p: &Position,
    a: &Appointment,
    ended_on: NaiveDate,
    reason: &str,
) -> CoreResult<AuditEvent> {
    let r = sqlx::query!(
        "UPDATE appointments SET status = 'ended', ended_on = $2, end_reason = $3, updated_at = now()
          WHERE id = $1 AND status = 'active'",
        a.id,
        ended_on,
        reason
    )
    .execute(&mut *tx)
    .await?;
    if r.rows_affected() != 1 {
        return Err(CoreError::Conflict(
            "This appointment changed while you were working. Reload and try again.".into(),
        ));
    }
    Ok(
        appointment_event(ctx, "appointment.ended", p, a.id, &a.person_name)
            .change("status", "active", "ended")
            .change("ended_on", None::<NaiveDate>, ended_on)
            .context("reason", reason.to_owned()),
    )
}

async fn refresh(core: &Core, id: Uuid) -> CoreResult<()> {
    crate::notifications::refresh_appointment(core, id, Utc::now()).await?;
    Ok(())
}

/// Appoint someone to a vacant seat.
pub async fn appoint(
    core: &Core,
    ctx: &Ctx,
    position_id: Uuid,
    mut a: NewAppointment,
) -> CoreResult<Uuid> {
    let p = load_position(core, position_id).await?;
    require_manage(core, ctx, &p).await?;
    if !p.is_active {
        return Err(CoreError::validation("this position is inactive"));
    }
    check_person(&mut a)?;
    let d = dates(&p, a.start_date, a.end_date, a.reappointment_due_date)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!("SELECT id FROM positions WHERE id = $1 FOR UPDATE", p.id)
        .fetch_one(&mut *tx)
        .await?;
    let filled = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM appointments WHERE position_id = $1 AND status = 'active'"#,
        p.id
    )
    .fetch_one(&mut *tx)
    .await?;
    if filled >= i64::from(p.seats) {
        return Err(CoreError::validation(
            "every seat is filled; reappoint or replace a current appointee, or end an appointment first",
        ));
    }
    let (id, e) = insert(&mut tx, ctx, &p, &a, &d, None, "appointment.created").await?;
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    refresh(core, id).await?;
    Ok(id)
}

async fn active_appointment(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
) -> CoreResult<(Position, Appointment)> {
    let (p, a) = appointment(core, ctx, id).await?;
    require_manage(core, ctx, &p).await?;
    if a.status != AppointmentStatus::Active {
        return Err(CoreError::validation("this appointment has already ended"));
    }
    Ok((p, a))
}

/// Give the same person a new term. The current appointment ends the day
/// before the new term starts; the checklist starts over.
pub async fn reappoint(core: &Core, ctx: &Ctx, id: Uuid, t: Term) -> CoreResult<Uuid> {
    let (p, old) = active_appointment(core, ctx, id).await?;
    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let start = t
        .start_date
        .or_else(|| old.end_date.map(|e| e + Duration::days(1)))
        .unwrap_or(today);
    if start <= old.start_date {
        return Err(CoreError::validation(
            "the new term must start after the current one started",
        ));
    }
    let mut a = NewAppointment {
        person_name: old.person_name.clone(),
        user_id: old.user_id,
        contact_email: old.contact_email.clone(),
        appointed_by: t.appointed_by,
        appointment_reference: t.appointment_reference,
        start_date: start,
        end_date: t.end_date,
        reappointment_due_date: t.reappointment_due_date,
        notes: t.notes,
    };
    check_person(&mut a)?;
    let d = dates(&p, start, a.end_date, a.reappointment_due_date)?;
    let mut tx = core.db.begin().await?;
    let ended = close(
        &mut tx,
        ctx,
        &p,
        &old,
        start - Duration::days(1),
        "reappointed",
    )
    .await?;
    let (new_id, e) = insert(
        &mut tx,
        ctx,
        &p,
        &a,
        &d,
        Some(old.id),
        "appointment.reappointed",
    )
    .await?;
    pdms_audit::append(&mut tx, vec![ended, e]).await?;
    tx.commit().await?;
    refresh(core, new_id).await?;
    Ok(new_id)
}

/// Replace the current appointee with someone new. The current appointment
/// ends the day before the successor starts.
pub async fn replace(core: &Core, ctx: &Ctx, id: Uuid, mut a: NewAppointment) -> CoreResult<Uuid> {
    let (p, old) = active_appointment(core, ctx, id).await?;
    check_person(&mut a)?;
    if a.start_date <= old.start_date {
        return Err(CoreError::validation(
            "the successor's term must start after the current appointee's started",
        ));
    }
    let d = dates(&p, a.start_date, a.end_date, a.reappointment_due_date)?;
    let mut tx = core.db.begin().await?;
    let ended = close(
        &mut tx,
        ctx,
        &p,
        &old,
        a.start_date - Duration::days(1),
        "replaced",
    )
    .await?;
    let (new_id, e) = insert(
        &mut tx,
        ctx,
        &p,
        &a,
        &d,
        Some(old.id),
        "appointment.replaced",
    )
    .await?;
    pdms_audit::append(&mut tx, vec![ended, e]).await?;
    tx.commit().await?;
    refresh(core, new_id).await?;
    Ok(new_id)
}

/// End an appointment without a successor (resignation, removal, term ended).
/// The seat becomes vacant.
pub async fn end(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    ended_on: NaiveDate,
    reason: &str,
) -> CoreResult<()> {
    let (p, a) = active_appointment(core, ctx, id).await?;
    let reason = trimmed(reason, 200, "the reason", true)?;
    if ended_on < a.start_date {
        return Err(CoreError::validation(
            "an appointment cannot end before it starts",
        ));
    }
    let mut tx = core.db.begin().await?;
    let e = close(&mut tx, ctx, &p, &a, ended_on, &reason).await?;
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

/// Correct an appointment's details and dates.
pub async fn update_appointment(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    edit: AppointmentEdit,
) -> CoreResult<()> {
    let (p, before) = appointment(core, ctx, id).await?;
    require_manage(core, ctx, &p).await?;
    let mut a = NewAppointment {
        person_name: edit.person_name,
        user_id: edit.user_id,
        contact_email: edit.contact_email,
        appointed_by: edit.appointed_by,
        appointment_reference: edit.appointment_reference,
        start_date: edit.start_date,
        end_date: edit.end_date,
        reappointment_due_date: edit.reappointment_due_date,
        notes: edit.notes,
    };
    check_person(&mut a)?;
    if a.end_date.is_some_and(|e| e < a.start_date) {
        return Err(CoreError::validation(
            "the term cannot end before it starts",
        ));
    }
    if a.reappointment_due_date.is_some_and(|r| r < a.start_date) {
        return Err(CoreError::validation(
            "the reappointment decision date cannot be before the term starts",
        ));
    }
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE appointments SET person_name = $2, user_id = $3, contact_email = $4, appointed_by = $5,
                appointment_reference = $6, start_date = $7, end_date = $8, reappointment_due_date = $9,
                notes = $10, updated_at = now()
          WHERE id = $1",
        id,
        a.person_name,
        a.user_id,
        a.contact_email,
        a.appointed_by,
        a.appointment_reference,
        a.start_date,
        a.end_date,
        a.reappointment_due_date,
        a.notes
    )
    .execute(&mut *tx)
    .await
    .map_err(fk_error)?;
    let e = appointment_event(ctx, "appointment.updated", &p, id, &a.person_name)
        .change("person_name", before.person_name, a.person_name.clone())
        .change("user_id", before.user_id, a.user_id)
        .change(
            "contact_email",
            before.contact_email,
            a.contact_email.clone(),
        )
        .change("appointed_by", before.appointed_by, a.appointed_by.clone())
        .change(
            "appointment_reference",
            before.appointment_reference,
            a.appointment_reference.clone(),
        )
        .change("start_date", before.start_date, a.start_date)
        .change("end_date", before.end_date, a.end_date)
        .change(
            "reappointment_due_date",
            before.reappointment_due_date,
            a.reappointment_due_date,
        )
        .change("notes", before.notes, a.notes.clone());
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    if before.status == AppointmentStatus::Active {
        refresh(core, id).await?;
    }
    Ok(())
}

/// Record whether a checklist item is satisfied for an appointment (who and
/// when are recorded automatically).
pub async fn set_requirement(
    core: &Core,
    ctx: &Ctx,
    appointment_id: Uuid,
    requirement_id: Uuid,
    satisfied: bool,
    note: &str,
) -> CoreResult<()> {
    let (p, a) = appointment(core, ctx, appointment_id).await?;
    if !may_track(ctx, &p) {
        return Err(deny(core, ctx, &p, "track_requirements").await);
    }
    let label = sqlx::query_scalar!(
        "SELECT label FROM position_requirements WHERE id = $1 AND position_id = $2",
        requirement_id,
        p.id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let note = trimmed(note, 1000, "the note", false)?;
    let before = a
        .requirements
        .iter()
        .find(|r| r.requirement_id == requirement_id)
        .is_some_and(|r| r.satisfied);
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO appointment_requirement_status (appointment_id, requirement_id, satisfied, satisfied_by,
                                                     satisfied_at, note)
         VALUES ($1, $2, $3, CASE WHEN $3 THEN $4::uuid END, CASE WHEN $3 THEN now() END, $5)
         ON CONFLICT (appointment_id, requirement_id) DO UPDATE
            SET satisfied = EXCLUDED.satisfied,
                satisfied_by = CASE WHEN EXCLUDED.satisfied = appointment_requirement_status.satisfied
                                    THEN appointment_requirement_status.satisfied_by ELSE EXCLUDED.satisfied_by END,
                satisfied_at = CASE WHEN EXCLUDED.satisfied = appointment_requirement_status.satisfied
                                    THEN appointment_requirement_status.satisfied_at ELSE EXCLUDED.satisfied_at END,
                note = EXCLUDED.note",
        appointment_id,
        requirement_id,
        satisfied,
        ctx.user_id(),
        note
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            appointment_event(
                ctx,
                "appointment.requirement_recorded",
                &p,
                appointment_id,
                &a.person_name,
            )
            .context("requirement_id", requirement_id)
            .context("requirement", label)
            .context("note", note)
            .change("satisfied", before, satisfied),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(term: Option<i32>, lead: i32) -> Position {
        Position {
            id: Uuid::nil(),
            name: "Seat 1".into(),
            body_name: "Board".into(),
            department_id: None,
            department_name: None,
            appointing_authority: "Board".into(),
            term_months: term,
            seats: 1,
            manager_user_id: None,
            manager_name: None,
            reappointment_lead_days: lead,
            description: String::new(),
            is_active: true,
            filled: 0,
        }
    }

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn terms_default_from_the_position() {
        let x = dates(&pos(Some(24), 90), d("2025-01-01"), None, None).unwrap();
        assert_eq!(
            x.end,
            Some(d("2026-12-31")),
            "two-year term ends the day before"
        );
        assert_eq!(x.reappointment, Some(d("2026-10-02")));
        let open = dates(&pos(None, 90), d("2025-01-01"), None, None).unwrap();
        assert_eq!((open.end, open.reappointment), (None, None));
        // A short term never puts the decision before the start.
        let short = dates(&pos(Some(1), 90), d("2025-01-01"), None, None).unwrap();
        assert_eq!(short.reappointment, Some(d("2025-01-01")));
        assert!(dates(&pos(None, 0), d("2025-01-01"), Some(d("2024-12-31")), None).is_err());
    }

    #[test]
    fn standing_reflects_holdover() {
        let a = Appointment {
            id: Uuid::nil(),
            position_id: Uuid::nil(),
            person_name: "Jane".into(),
            user_id: None,
            contact_email: String::new(),
            appointed_by: String::new(),
            appointment_reference: String::new(),
            start_date: d("2023-01-01"),
            end_date: Some(d("2024-12-31")),
            reappointment_due_date: Some(d("2024-10-01")),
            status: AppointmentStatus::Active,
            ended_on: None,
            end_reason: String::new(),
            predecessor_id: None,
            notes: String::new(),
            created_at: Utc::now(),
            requirements: vec![],
        };
        assert_eq!(a.standing(d("2024-06-01")), "Serving");
        assert_eq!(a.standing(d("2024-10-01")), "Reappointment decision due");
        assert_eq!(a.standing(d("2025-01-01")), "Holdover (term ended)");
    }

    #[test]
    fn term_labels() {
        assert_eq!(pos(Some(24), 0).term_label(), "2 years");
        assert_eq!(pos(Some(12), 0).term_label(), "1 year");
        assert_eq!(pos(Some(18), 0).term_label(), "18 months");
        assert_eq!(pos(None, 0).term_label(), "No fixed term");
    }
}
