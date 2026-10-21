// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Geometry for the document page's key-dates timeline (SVG, viewBox 0 0 1000 64).

use chrono::{Datelike, NaiveDate};

pub const WIDTH: f64 = 1000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Event,
    Missed,
    Future,
}

impl MarkKind {
    /// The CSS class suffix: `timeline-event`, `timeline-missed`, `timeline-future`.
    pub fn as_str(self) -> &'static str {
        match self {
            MarkKind::Event => "event",
            MarkKind::Missed => "missed",
            MarkKind::Future => "future",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub x: f64,
    pub kind: MarkKind,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub marks: Vec<Mark>,
    pub ticks: Vec<(f64, String)>,
    pub today_x: f64,
    /// Start of the red "overdue" span (earliest missed date), if any.
    pub overdue_from_x: Option<f64>,
}

/// `events`: (date, description, kind). Returns None when there are no events.
/// The axis runs from min(earliest event, today) to max(latest event, today).
pub fn build(today: NaiveDate, events: &[(NaiveDate, String, MarkKind)]) -> Option<Timeline> {
    let first = events.iter().map(|e| e.0).min()?.min(today);
    let last = events.iter().map(|e| e.0).max()?.max(today);
    let span = (last - first).num_days().max(1) as f64;
    let x = |d: NaiveDate| ((d - first).num_days() as f64 / span * WIDTH).clamp(0.0, WIDTH);
    let marks = events
        .iter()
        .map(|(d, text, kind)| Mark {
            x: x(*d),
            kind: *kind,
            title: format!("{} · {text}", d.format("%b %-d, %Y")),
        })
        .collect();
    let mut ticks = Vec::new();
    let years = (last.year() - first.year()).max(1);
    let step = if years > 12 {
        5
    } else if years > 6 {
        2
    } else {
        1
    };
    let mut y = first.year();
    while y <= last.year() {
        if let Some(d) = NaiveDate::from_ymd_opt(y, 1, 1)
            && d >= first
        {
            ticks.push((x(d), y.to_string()));
        }
        y += step;
    }
    let overdue_from_x = events
        .iter()
        .filter(|e| e.2 == MarkKind::Missed)
        .map(|e| x(e.0))
        .reduce(f64::min);
    Some(Timeline {
        marks,
        ticks,
        today_x: x(today),
        overdue_from_x,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn places_events_and_today_proportionally() {
        let ev = vec![
            (
                d("2013-05-01"),
                "Executed and effective".to_string(),
                MarkKind::Event,
            ),
            (
                d("2018-05-01"),
                "Five-year review".to_string(),
                MarkKind::Missed,
            ),
            (
                d("2023-05-01"),
                "Five-year review".to_string(),
                MarkKind::Missed,
            ),
        ];
        let t = build(d("2026-10-15"), &ev).unwrap();
        assert_eq!(t.marks[0].x, 0.0);
        assert!(
            (t.today_x - WIDTH).abs() < 1e-9,
            "today is the latest point"
        );
        assert!(t.marks[1].x > 300.0 && t.marks[1].x < 450.0);
        assert_eq!(t.overdue_from_x, Some(t.marks[1].x));
        assert!(t.marks[1].title.starts_with("May 1, 2018"));
        assert!(!t.ticks.is_empty());
    }

    #[test]
    fn future_events_extend_the_axis() {
        let ev = vec![
            (d("2026-01-01"), "Effective".to_string(), MarkKind::Event),
            (d("2027-01-01"), "Renewal".to_string(), MarkKind::Future),
        ];
        let t = build(d("2026-10-15"), &ev).unwrap();
        assert!(t.today_x < WIDTH);
        assert_eq!(t.marks[1].x, WIDTH);
        assert_eq!(build(d("2026-10-15"), &[]), None);
    }

    #[test]
    fn today_starts_the_axis_when_every_date_is_ahead() {
        let ev = vec![
            (d("2027-01-01"), "Effective".to_string(), MarkKind::Future),
            (d("2027-06-01"), "Next review".to_string(), MarkKind::Future),
        ];
        let t = build(d("2026-10-15"), &ev).unwrap();
        assert_eq!(t.today_x, 0.0);
        assert!(t.marks[0].x > 0.0, "the first date is after today");
        assert_eq!(t.marks[1].x, WIDTH);
        let single = build(
            d("2026-10-15"),
            &[(d("2026-11-04"), "Next review".to_string(), MarkKind::Future)],
        )
        .unwrap();
        assert_eq!(single.today_x, 0.0);
        assert_eq!(single.marks[0].x, WIDTH);
    }
}
