// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Reminders: rules, the planner, email and in-app delivery.

pub mod actions;
pub mod delivery;
pub(crate) mod dispatch;
pub mod inbox;
pub mod mail_setup;
pub mod mailer;
pub mod planner;
pub mod rules;

pub use planner::{EMAIL_JOB, PlanReport, plan, refresh_appointment, refresh_document};

/// A date-driven reminder, or an "action needed" notice from a workflow.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, sqlx::Type)]
#[sqlx(type_name = "notification_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Reminder,
    Action,
}

impl NotificationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::Reminder => "reminder",
            NotificationKind::Action => "action",
        }
    }
}
