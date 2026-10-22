// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Pure helpers for the dashboard: greeting and "Needs your attention" ordering.

use chrono::{NaiveDate, NaiveTime, Timelike};

pub fn greeting(local: NaiveTime) -> &'static str {
    match local.hour() {
        0..=11 => "Good morning",
        12..=16 => "Good afternoon",
        _ => "Good evening",
    }
}

/// "Good morning, Ada", or just "Good morning" when the name is empty.
pub fn greeting_line(local: NaiveTime, display_name: &str) -> String {
    let greeting = greeting(local);
    match first_name(display_name) {
        "" => greeting.to_owned(),
        name => format!("{greeting}, {name}"),
    }
}

pub fn first_name(display_name: &str) -> &str {
    display_name.split_whitespace().next().unwrap_or("")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    Overdue,
    Approval,
    DueSoon,
    Upcoming,
}

#[derive(Debug, Clone)]
pub struct AttentionRow {
    pub urgency: Urgency,
    /// Overdue: days late (sorted descending). Others: the due or submitted date.
    pub days_late: i64,
    pub date: NaiveDate,
    pub status_class: &'static str,
    pub status_label: &'static str,
    pub item: String,
    pub detail: String,
    pub due_text: String,
    pub due_date_text: String,
    pub action_label: &'static str,
    pub action_primary: bool,
    pub href: String,
}

/// Overdue tasks (most days late first), approvals (oldest first),
/// tasks due within 30 days (soonest first), then other tasks (soonest first).
pub fn order_attention(rows: &mut [AttentionRow]) {
    rows.sort_by(|a, b| {
        a.urgency.cmp(&b.urgency).then_with(|| match a.urgency {
            Urgency::Overdue => b.days_late.cmp(&a.days_late),
            _ => a.date.cmp(&b.date),
        })
    });
}

pub fn task_urgency(today: NaiveDate, due: NaiveDate) -> Urgency {
    let days = (due - today).num_days();
    if days < 0 {
        Urgency::Overdue
    } else if days <= 30 {
        Urgency::DueSoon
    } else {
        Urgency::Upcoming
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn row(u: Urgency, days_late: i64, date: &str, item: &str) -> AttentionRow {
        AttentionRow {
            urgency: u,
            days_late,
            date: d(date),
            status_class: "",
            status_label: "",
            item: item.into(),
            detail: String::new(),
            due_text: String::new(),
            due_date_text: String::new(),
            action_label: "",
            action_primary: false,
            href: String::new(),
        }
    }

    #[test]
    fn greeting_line_drops_the_comma_without_a_name() {
        let t = NaiveTime::from_hms_opt(8, 0, 0).unwrap();
        assert_eq!(greeting_line(t, "Ada Lovelace"), "Good morning, Ada");
        assert_eq!(greeting_line(t, ""), "Good morning");
        assert_eq!(greeting_line(t, "   "), "Good morning");
    }

    #[test]
    fn greeting_follows_the_clock() {
        assert_eq!(
            greeting(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            "Good morning"
        );
        assert_eq!(
            greeting(NaiveTime::from_hms_opt(11, 59, 0).unwrap()),
            "Good morning"
        );
        assert_eq!(
            greeting(NaiveTime::from_hms_opt(12, 0, 0).unwrap()),
            "Good afternoon"
        );
        assert_eq!(
            greeting(NaiveTime::from_hms_opt(16, 59, 0).unwrap()),
            "Good afternoon"
        );
        assert_eq!(
            greeting(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
            "Good evening"
        );
        assert_eq!(first_name("Alice Ops"), "Alice");
        assert_eq!(first_name("ada@example.org"), "ada@example.org");
    }

    #[test]
    fn attention_order() {
        let mut rows = vec![
            row(Urgency::Upcoming, 0, "2026-12-31", "later"),
            row(Urgency::DueSoon, 0, "2026-11-07", "soon-b"),
            row(Urgency::Approval, 0, "2026-10-01", "approval-old"),
            row(Urgency::Overdue, 44, "2026-09-01", "late-44"),
            row(Urgency::DueSoon, 0, "2026-11-03", "soon-a"),
            row(Urgency::Overdue, 1263, "2023-05-01", "late-1263"),
            row(Urgency::Approval, 0, "2026-10-10", "approval-new"),
        ];
        order_attention(&mut rows);
        let names: Vec<_> = rows.iter().map(|r| r.item.as_str()).collect();
        assert_eq!(
            names,
            [
                "late-1263",
                "late-44",
                "approval-old",
                "approval-new",
                "soon-a",
                "soon-b",
                "later"
            ]
        );
        assert_eq!(
            task_urgency(d("2026-10-15"), d("2026-10-14")),
            Urgency::Overdue
        );
        assert_eq!(
            task_urgency(d("2026-10-15"), d("2026-11-14")),
            Urgency::DueSoon
        );
        assert_eq!(
            task_urgency(d("2026-10-15"), d("2026-11-15")),
            Urgency::Upcoming
        );
    }
}
