// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! One wording for appointment seats, shared by the dashboard, the county
//! overview and the appointments pages, so a seat reads the same everywhere
//! (R2 spec §6.1).

use pdms_core::appointments::{Position, SeatAttention, SeatState};

use crate::views::fmt_date;

/// How far ahead a term end counts as "ending soon".
pub const HORIZON_DAYS: i64 = 90;

/// The status class (`.status-*`) and the words for a seat state.
pub fn status(state: &SeatState) -> (&'static str, String) {
    match state {
        SeatState::DecisionOverdue => ("critical", "Decision overdue".to_owned()),
        SeatState::DecisionDueToday => ("critical", "Decision due today".to_owned()),
        SeatState::Holdover => ("warning", "Holdover".to_owned()),
        SeatState::Vacant => ("neutral", "Vacant".to_owned()),
        SeatState::TermEnding(d) => ("info", format!("Term ends {}", fmt_date(Some(*d)))),
    }
}

/// A seat with nothing to do.
pub const CURRENT: (&str, &str) = ("good", "Current");

/// The five board counts, in the order they are shown.
pub struct Counts {
    pub current: i64,
    pub decision: i64,
    pub holdover: i64,
    pub ending: i64,
    pub vacant: i64,
}

impl Counts {
    /// `positions` are the active positions in scope; `attention` is what
    /// [`pdms_core::appointments::attention`] returned for them.
    pub fn of(positions: &[&Position], attention: &[&SeatAttention]) -> Counts {
        let n = |f: fn(&SeatState) -> bool| attention.iter().filter(|a| f(&a.state)).count() as i64;
        let decision = n(|s| matches!(s, SeatState::DecisionOverdue | SeatState::DecisionDueToday));
        let holdover = n(|s| matches!(s, SeatState::Holdover));
        let vacant = n(|s| matches!(s, SeatState::Vacant));
        let ending = n(|s| matches!(s, SeatState::TermEnding(_)));
        let filled: i64 = positions.iter().map(|p| p.filled).sum();
        Counts {
            current: (filled - holdover - decision - ending).max(0),
            decision,
            holdover,
            ending,
            vacant,
        }
    }

    /// (label, count) pairs for display.
    pub fn rows(&self) -> [(&'static str, i64); 5] {
        [
            ("Filled and current", self.current),
            ("Decision due or overdue", self.decision),
            ("Holdover", self.holdover),
            ("Term ending soon", self.ending),
            ("Vacant", self.vacant),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn every_state_has_words_and_a_class() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert_eq!(
            status(&SeatState::DecisionOverdue),
            ("critical", "Decision overdue".to_owned())
        );
        assert_eq!(status(&SeatState::Holdover).0, "warning");
        assert_eq!(status(&SeatState::Vacant).0, "neutral");
        assert_eq!(
            status(&SeatState::TermEnding(d)),
            ("info", "Term ends Nov 3, 2026".to_owned())
        );
    }
}
