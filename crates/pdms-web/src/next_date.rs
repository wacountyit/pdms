// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! The documents list's single "Next date" column.

use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextDate {
    pub label: &'static str,
    pub date: NaiveDate,
    pub overdue: bool,
}

/// Overdue review or renewal wins (earliest past one); otherwise the earliest
/// future review, renewal, or expiration; otherwise nothing.
pub fn next_date(
    today: NaiveDate,
    review: Option<NaiveDate>,
    renewal: Option<NaiveDate>,
    expiration: Option<NaiveDate>,
) -> Option<NextDate> {
    let past = [(review, "Review"), (renewal, "Renewal")]
        .into_iter()
        .filter_map(|(d, l)| d.filter(|d| *d < today).map(|d| (d, l)))
        .min_by_key(|(d, _)| *d);
    if let Some((date, _)) = past {
        return Some(NextDate {
            label: "Overdue",
            date,
            overdue: true,
        });
    }
    [
        (review, "Review"),
        (renewal, "Renewal"),
        (expiration, "Expires"),
    ]
    .into_iter()
    .filter_map(|(d, l)| d.filter(|d| *d >= today).map(|d| (d, l)))
    .min_by_key(|(d, _)| *d)
    .map(|(date, label)| NextDate {
        label,
        date,
        overdue: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn overdue_review_beats_future_dates() {
        let n = next_date(
            d("2026-10-15"),
            Some(d("2023-05-01")),
            Some(d("2027-01-01")),
            None,
        )
        .unwrap();
        assert_eq!(
            (n.label, n.date, n.overdue),
            ("Overdue", d("2023-05-01"), true)
        );
    }

    #[test]
    fn earliest_future_date_with_label() {
        let n = next_date(
            d("2026-10-15"),
            Some(d("2026-11-03")),
            Some(d("2026-11-07")),
            Some(d("2026-12-31")),
        )
        .unwrap();
        assert_eq!(
            (n.label, n.date, n.overdue),
            ("Review", d("2026-11-03"), false)
        );
        let n = next_date(d("2026-10-15"), None, None, Some(d("2026-12-31"))).unwrap();
        assert_eq!(n.label, "Expires");
        let n = next_date(d("2026-10-15"), Some(d("2026-10-15")), None, None).unwrap();
        assert_eq!(
            (n.label, n.overdue),
            ("Review", false),
            "today is not overdue"
        );
    }

    #[test]
    fn past_expiration_alone_is_nothing() {
        assert_eq!(
            next_date(d("2026-10-15"), None, None, Some(d("2025-01-01"))),
            None
        );
        assert_eq!(next_date(d("2026-10-15"), None, None, None), None);
    }
}
