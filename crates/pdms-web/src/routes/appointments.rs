// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Positions and appointments pages.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use axum_extra::extract::Form;
use chrono::{NaiveDate, Utc};
use pdms_authz::Capability;
use pdms_core::appointments::{
    self, Appointment, AppointmentEdit, AppointmentStatus, NewAppointment, Position, PositionSpec,
    Requirement, SeatState, Term,
};
use pdms_core::directory::{self, Department};
use pdms_core::notifications::rules::{self, DueEvent};
use pdms_core::{CoreError, CoreResult, schedule};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::routes::documents::parse_date;
use crate::seats;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_date, fmt_time, iso_date};

pub struct PositionRow {
    pub id: Uuid,
    pub name: String,
    pub department: String,
    pub term: String,
    pub seats: String,
    pub holders: String,
    pub next_date: String,
    pub status_class: &'static str,
    pub status_text: String,
}

/// The positions of one board or body.
pub struct BodyGroup {
    pub name: String,
    pub rows: Vec<PositionRow>,
}

#[derive(Template)]
#[template(path = "positions/list.html")]
struct ListPage {
    l: Layout,
    groups: Vec<BodyGroup>,
    /// The board counts, as on the county overview; none without positions.
    tiles: Vec<(&'static str, i64)>,
    can_manage: bool,
    departments: Vec<Department>,
    users: Vec<(Uuid, String)>,
}

/// A position's one status: its worst seat, using the same rule and words as
/// the dashboard and the county overview.
fn position_status(p: &Position, current: &[&Appointment], t: NaiveDate) -> (&'static str, String) {
    if !p.is_active {
        return ("neutral", "Inactive".to_owned());
    }
    let horizon = Some(t + chrono::Duration::days(seats::HORIZON_DAYS));
    let vacant = (appointments::vacant_seats(p) > 0).then_some(SeatState::Vacant);
    current
        .iter()
        .filter_map(|a| appointments::seat_state(a, t, horizon))
        .chain(vacant)
        .min()
        .map_or_else(
            || (seats::CURRENT.0, seats::CURRENT.1.to_owned()),
            |s| seats::status(&s),
        )
}

async fn pickers(state: &AppState) -> CoreResult<(Vec<Department>, Vec<(Uuid, String)>)> {
    let departments = directory::list_departments(&state.core.db)
        .await?
        .into_iter()
        .filter(|d| d.is_active)
        .collect();
    let users = directory::list_users(&state.core.db, "", 5000)
        .await?
        .into_iter()
        .filter(|u| u.status == pdms_core::model::UserStatus::Active)
        .map(|u| (u.id, u.display_name))
        .collect();
    Ok((departments, users))
}

fn today(state: &AppState) -> NaiveDate {
    schedule::local_today(Utc::now(), state.tz)
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let core = &state.core;
    let can_manage = user.can(Capability::ManageAppointments);
    let positions = appointments::list_positions(core, &user.ctx, can_manage).await?;
    let t = today(&state);
    let active: Vec<&Position> = positions.iter().filter(|p| p.is_active).collect();
    let tiles = if active.is_empty() {
        Vec::new()
    } else {
        let attention = appointments::attention(core, &user.ctx, t, seats::HORIZON_DAYS).await?;
        seats::Counts::of(&active, &attention.iter().collect::<Vec<_>>())
            .rows()
            .to_vec()
    };
    let mut groups: Vec<BodyGroup> = Vec::new();
    for p in positions {
        let detail = appointments::position(core, &user.ctx, p.id).await?;
        let current: Vec<&Appointment> = detail
            .appointments
            .iter()
            .filter(|a| a.status == AppointmentStatus::Active)
            .collect();
        let next = current.iter().filter_map(|a| a.end_date).min();
        let (status_class, status_text) = position_status(&p, &current, t);
        let row = PositionRow {
            id: p.id,
            department: p.department_name.clone().unwrap_or_default(),
            term: p.term_label(),
            seats: format!("{} of {}", p.filled, p.seats),
            holders: current
                .iter()
                .map(|a| a.person_name.clone())
                .collect::<Vec<_>>()
                .join(", "),
            next_date: fmt_date(next),
            status_class,
            status_text,
            name: p.name,
        };
        match groups.iter_mut().find(|g| g.name == p.body_name) {
            Some(g) => g.rows.push(row),
            None => groups.push(BodyGroup {
                name: p.body_name,
                rows: vec![row],
            }),
        }
    }
    groups.sort_by_key(|g| g.name.to_lowercase());
    for g in &mut groups {
        g.rows.sort_by_key(|r| r.name.to_lowercase());
    }
    let (departments, users) = if can_manage {
        pickers(&state).await?
    } else {
        (Vec::new(), Vec::new())
    };
    let page = ListPage {
        l: Layout::new(&state, &user, flash, "Boards and appointments", "positions"),
        groups,
        tiles,
        can_manage,
        departments,
        users,
    };
    Ok(Html(page.render()?))
}

pub struct CheckView {
    pub requirement_id: Uuid,
    pub label: String,
    pub description: String,
    pub satisfied: bool,
    pub by: String,
    pub note: String,
}

pub struct ApptView {
    pub id: Uuid,
    pub person: String,
    pub user_id: String,
    pub email: String,
    pub appointed_by: String,
    pub reference: String,
    pub start: String,
    pub end: String,
    pub reappointment: String,
    pub start_iso: String,
    pub end_iso: String,
    pub reappointment_iso: String,
    pub standing: String,
    pub standing_class: &'static str,
    pub ended: String,
    pub notes: String,
    pub checks: Vec<CheckView>,
    pub met: String,
    pub active: bool,
    pub reappointed: bool,
}

pub struct RuleLine {
    pub event: &'static str,
    pub source: &'static str,
    pub summary: String,
}

#[derive(Template)]
#[template(path = "positions/show.html")]
struct ShowPage {
    l: Layout,
    p: Position,
    status_class: &'static str,
    status_text: String,
    term: String,
    manager: String,
    department: String,
    can_manage: bool,
    can_track: bool,
    requirements: Vec<Requirement>,
    current: Vec<ApptView>,
    history: Vec<ApptView>,
    departments: Vec<Department>,
    users: Vec<(Uuid, String)>,
    today: String,
    rules: Vec<RuleLine>,
}

impl ShowPage {
    fn dept_selected(&self, id: &Uuid) -> bool {
        self.p.department_id == Some(*id)
    }

    fn manager_selected(&self, id: &Uuid) -> bool {
        self.p.manager_user_id == Some(*id)
    }

    fn term_value(&self) -> String {
        self.p
            .term_months
            .map(|m| m.to_string())
            .unwrap_or_default()
    }
}

fn appt_view(state: &AppState, a: &Appointment, t: NaiveDate) -> ApptView {
    let horizon = Some(t + chrono::Duration::days(seats::HORIZON_DAYS));
    let (standing_class, standing) = if a.status == AppointmentStatus::Ended {
        ("neutral", "Ended".to_owned())
    } else {
        appointments::seat_state(a, t, horizon).map_or_else(
            || (seats::CURRENT.0, seats::CURRENT.1.to_owned()),
            |s| seats::status(&s),
        )
    };
    ApptView {
        id: a.id,
        person: a.person_name.clone(),
        user_id: a.user_id.map(|u| u.to_string()).unwrap_or_default(),
        email: a.contact_email.clone(),
        appointed_by: a.appointed_by.clone(),
        reference: a.appointment_reference.clone(),
        start: fmt_date(Some(a.start_date)),
        end: if a.end_date.is_some() {
            fmt_date(a.end_date)
        } else {
            "No fixed end".into()
        },
        reappointment: fmt_date(a.reappointment_due_date),
        start_iso: iso_date(Some(a.start_date)),
        end_iso: iso_date(a.end_date),
        reappointment_iso: iso_date(a.reappointment_due_date),
        standing,
        standing_class,
        ended: match a.ended_on {
            Some(d) => format!("Ended {} ({})", fmt_date(Some(d)), a.end_reason),
            None => String::new(),
        },
        notes: a.notes.clone(),
        met: format!("{} of {}", a.requirements_met(), a.requirements.len()),
        checks: a
            .requirements
            .iter()
            .map(|r| CheckView {
                requirement_id: r.requirement_id,
                label: r.label.clone(),
                description: r.description.clone(),
                satisfied: r.satisfied,
                by: match (r.satisfied, &r.satisfied_by, r.satisfied_at) {
                    (true, by, Some(at)) => format!(
                        "by {} on {}",
                        by.clone().unwrap_or_else(|| "System".into()),
                        fmt_time(state.tz, at)
                    ),
                    _ => String::new(),
                },
                note: r.note.clone(),
            })
            .collect(),
        active: a.status == AppointmentStatus::Active,
        reappointed: a.predecessor_id.is_some(),
    }
}

pub async fn show(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    let core = &state.core;
    let detail = appointments::position(core, &user.ctx, id).await?;
    let t = today(&state);
    let (current, history): (Vec<&Appointment>, Vec<&Appointment>) = detail
        .appointments
        .iter()
        .partition(|a| a.status == AppointmentStatus::Active);
    let (departments, users) = if detail.can_manage {
        pickers(&state).await?
    } else {
        (Vec::new(), Vec::new())
    };
    let all_rules = rules::load_all(&core.db).await?;
    let rules = DueEvent::APPOINTMENT
        .into_iter()
        .map(|e| {
            let (rs, source) = rules::effective_for_position(&all_rules, id, e);
            RuleLine {
                event: e.label(),
                source: source.label(),
                summary: rs
                    .iter()
                    .map(|r| format!("{} → {}", r.summary(), r.recipient_labels()))
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        })
        .collect();
    let p = detail.position.clone();
    let (status_class, status_text) = position_status(&p, &current, t);
    let page = ShowPage {
        status_class,
        status_text,
        l: Layout::new(&state, &user, flash, &p.label(), "positions"),
        term: p.term_label(),
        manager: p.manager_name.clone().unwrap_or_else(|| "Not set".into()),
        department: p.department_name.clone().unwrap_or_else(|| "—".into()),
        can_manage: detail.can_manage,
        can_track: detail.can_track,
        requirements: detail.requirements.clone(),
        current: current.iter().map(|a| appt_view(&state, a, t)).collect(),
        history: history.iter().map(|a| appt_view(&state, a, t)).collect(),
        departments,
        users,
        today: t.to_string(),
        rules,
        p,
    };
    Ok(Html(page.render()?))
}

// ---------------------------------------------------------------------------
// Forms
// ---------------------------------------------------------------------------

fn opt_uuid(label: &str, s: &str) -> CoreResult<Option<Uuid>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse()
        .map(Some)
        .map_err(|_| CoreError::validation(format!("choose a {label}")))
}

fn opt_int(label: &str, s: &str) -> CoreResult<Option<i32>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse()
        .map(Some)
        .map_err(|_| CoreError::validation(format!("{label} must be a whole number")))
}

fn required_date(label: &str, s: &str) -> CoreResult<NaiveDate> {
    parse_date(label, s)?.ok_or_else(|| CoreError::validation(format!("enter the {label}")))
}

#[derive(Deserialize)]
pub struct PositionForm {
    name: String,
    body_name: String,
    #[serde(default)]
    department_id: String,
    #[serde(default)]
    appointing_authority: String,
    #[serde(default)]
    term_months: String,
    #[serde(default)]
    seats: String,
    #[serde(default)]
    manager_user_id: String,
    #[serde(default)]
    reappointment_lead_days: String,
    #[serde(default)]
    description: String,
    is_active: Option<String>,
}

impl PositionForm {
    fn spec(self, default_active: bool) -> CoreResult<PositionSpec> {
        Ok(PositionSpec {
            department_id: opt_uuid("department", &self.department_id)?,
            term_months: opt_int("The term", &self.term_months)?,
            seats: opt_int("Seats", &self.seats)?.unwrap_or(1),
            manager_user_id: opt_uuid("manager", &self.manager_user_id)?,
            reappointment_lead_days: opt_int("The lead time", &self.reappointment_lead_days)?
                .unwrap_or(90),
            is_active: self.is_active.map_or(default_active, |v| v == "true"),
            name: self.name,
            body_name: self.body_name,
            appointing_authority: self.appointing_authority,
            description: self.description,
        })
    }
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<PositionForm>,
) -> AppResult<Response> {
    let r =
        async { appointments::create_position(&state.core, &user.ctx, f.spec(true)?).await }.await;
    match r {
        Ok(id) => done(
            &state,
            &format!("/positions/{id}"),
            Ok(()),
            "Position created. Add its requirements checklist below.",
        ),
        Err(e) => done::<()>(&state, "/positions", Err(e), ""),
    }
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<PositionForm>,
) -> AppResult<Response> {
    let r =
        async { appointments::update_position(&state.core, &user.ctx, id, f.spec(false)?).await }
            .await;
    done(&state, &format!("/positions/{id}"), r, "Position saved.")
}

#[derive(Deserialize)]
pub struct RequirementForm {
    label: String,
    #[serde(default)]
    description: String,
    is_active: Option<String>,
}

pub async fn add_requirement(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<RequirementForm>,
) -> AppResult<Response> {
    let r =
        appointments::add_requirement(&state.core, &user.ctx, id, &f.label, &f.description).await;
    done(
        &state,
        &format!("/positions/{id}#requirements"),
        r,
        "Requirement added.",
    )
}

pub async fn update_requirement(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, req)): Path<(Uuid, Uuid)>,
    Form(f): Form<RequirementForm>,
) -> AppResult<Response> {
    let r = appointments::update_requirement(
        &state.core,
        &user.ctx,
        req,
        &f.label,
        &f.description,
        f.is_active.as_deref() != Some("false"),
    )
    .await;
    done(
        &state,
        &format!("/positions/{id}#requirements"),
        r,
        "Requirement saved.",
    )
}

#[derive(Deserialize)]
pub struct AppointForm {
    #[serde(default)]
    person_name: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    contact_email: String,
    #[serde(default)]
    appointed_by: String,
    #[serde(default)]
    appointment_reference: String,
    #[serde(default)]
    start_date: String,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    reappointment_due_date: String,
    #[serde(default)]
    notes: String,
}

impl AppointForm {
    async fn new_appointment(self, state: &AppState) -> CoreResult<NewAppointment> {
        let user_id = opt_uuid("user", &self.user_id)?;
        let person_name = match (self.person_name.trim().is_empty(), user_id) {
            (true, Some(u)) => {
                directory::get_user(&state.core.db, u)
                    .await?
                    .user
                    .display_name
            }
            _ => self.person_name,
        };
        Ok(NewAppointment {
            person_name,
            user_id,
            contact_email: self.contact_email,
            appointed_by: self.appointed_by,
            appointment_reference: self.appointment_reference,
            start_date: required_date("start date", &self.start_date)?,
            end_date: parse_date("End date", &self.end_date)?,
            reappointment_due_date: parse_date(
                "Reappointment decision date",
                &self.reappointment_due_date,
            )?,
            notes: self.notes,
        })
    }
}

fn back(id: Uuid) -> String {
    format!("/positions/{id}#appointees")
}

pub async fn appoint(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<AppointForm>,
) -> AppResult<Response> {
    let r = async {
        let a = f.new_appointment(&state).await?;
        appointments::appoint(&state.core, &user.ctx, id, a).await
    }
    .await;
    done(&state, &back(id), r, "Appointment recorded.")
}

pub async fn replace(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, appt)): Path<(Uuid, Uuid)>,
    Form(f): Form<AppointForm>,
) -> AppResult<Response> {
    let r = async {
        let a = f.new_appointment(&state).await?;
        appointments::replace(&state.core, &user.ctx, appt, a).await
    }
    .await;
    done(
        &state,
        &back(id),
        r,
        "Successor appointed; the previous appointment has ended.",
    )
}

#[derive(Deserialize)]
pub struct TermForm {
    #[serde(default)]
    start_date: String,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    reappointment_due_date: String,
    #[serde(default)]
    appointed_by: String,
    #[serde(default)]
    appointment_reference: String,
    #[serde(default)]
    notes: String,
}

pub async fn reappoint(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, appt)): Path<(Uuid, Uuid)>,
    Form(f): Form<TermForm>,
) -> AppResult<Response> {
    let r = async {
        let t = Term {
            start_date: parse_date("Start date", &f.start_date)?,
            end_date: parse_date("End date", &f.end_date)?,
            reappointment_due_date: parse_date(
                "Reappointment decision date",
                &f.reappointment_due_date,
            )?,
            appointed_by: f.appointed_by,
            appointment_reference: f.appointment_reference,
            notes: f.notes,
        };
        appointments::reappoint(&state.core, &user.ctx, appt, t).await
    }
    .await;
    done(&state, &back(id), r, "Reappointed for a new term.")
}

#[derive(Deserialize)]
pub struct EndForm {
    #[serde(default)]
    ended_on: String,
    #[serde(default)]
    reason: String,
}

pub async fn end(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, appt)): Path<(Uuid, Uuid)>,
    Form(f): Form<EndForm>,
) -> AppResult<Response> {
    let r = async {
        let on = required_date("end date", &f.ended_on)?;
        appointments::end(&state.core, &user.ctx, appt, on, &f.reason).await
    }
    .await;
    done(
        &state,
        &back(id),
        r,
        "Appointment ended. The seat is now vacant.",
    )
}

pub async fn edit(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, appt)): Path<(Uuid, Uuid)>,
    Form(f): Form<AppointForm>,
) -> AppResult<Response> {
    let r = async {
        let a = f.new_appointment(&state).await?;
        appointments::update_appointment(
            &state.core,
            &user.ctx,
            appt,
            AppointmentEdit {
                person_name: a.person_name,
                user_id: a.user_id,
                contact_email: a.contact_email,
                appointed_by: a.appointed_by,
                appointment_reference: a.appointment_reference,
                start_date: a.start_date,
                end_date: a.end_date,
                reappointment_due_date: a.reappointment_due_date,
                notes: a.notes,
            },
        )
        .await
    }
    .await;
    done(&state, &back(id), r, "Appointment saved.")
}

#[derive(Deserialize)]
pub struct CheckForm {
    #[serde(default)]
    satisfied: String,
    #[serde(default)]
    note: String,
}

pub async fn check(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, appt, req)): Path<(Uuid, Uuid, Uuid)>,
    Form(f): Form<CheckForm>,
) -> AppResult<Response> {
    let r = appointments::set_requirement(
        &state.core,
        &user.ctx,
        appt,
        req,
        f.satisfied == "true",
        &f.note,
    )
    .await;
    done(&state, &back(id), r, "Checklist updated.")
}
