// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Date rules: review cycles, reminder triggers, and date-driven statuses.
//!
//! Everything here is pure and works on civil dates in the organization's
//! time zone; callers convert "now" with [`local_today`].

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;

use crate::documents::DocumentStatus;

/// The organization's current civil date.
pub fn local_today(now: DateTime<Utc>, tz: Tz) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
}

/// Whether today's reminders may go out yet (on or after `send_hour` local
/// time). Converting through the zone makes this correct across DST changes.
pub fn send_window_open(now: DateTime<Utc>, tz: Tz, send_hour: u32) -> bool {
    now.with_timezone(&tz).hour() >= send_hour
}

/// Add calendar months, clamping to the end of shorter months (Jan 31 + 1 = Feb 28/29).
pub fn add_months(d: NaiveDate, months: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + months;
    let (y, m0) = (total.div_euclid(12), total.rem_euclid(12) as u32);
    let mut day = d.day();
    loop {
        if let Some(r) = NaiveDate::from_ymd_opt(y, m0 + 1, day) {
            return r;
        }
        day -= 1;
        if day == 0 {
            return d;
        }
    }
}

/// The next review date after a review on `last` (or starting from `last`).
pub fn next_review(last: NaiveDate, interval_months: i32) -> NaiveDate {
    add_months(last, interval_months)
}

/// Review due dates that have passed without a review: `next`, then every
/// `interval` months, up to and including `today`.
pub fn missed_cycles(
    next: NaiveDate,
    interval_months: Option<i32>,
    today: NaiveDate,
) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    if next > today {
        return out;
    }
    let Some(interval) = interval_months.filter(|m| *m > 0) else {
        return vec![next];
    };
    let mut d = next;
    while d <= today && out.len() < 1000 {
        out.push(d);
        d = add_months(next, interval * out.len() as i32);
    }
    out
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriggerKind {
    /// `offset` days before the due date.
    Advance,
    /// On the due date.
    Due,
    /// `offset` days after the due date.
    Overdue,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Advance => "advance",
            TriggerKind::Due => "due",
            TriggerKind::Overdue => "overdue",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub offset_days: i32,
    pub date: NaiveDate,
}

/// The reminder that should exist for `due` as of `today`: the latest
/// scheduled trigger on or before today.
///
/// Earlier triggers that were missed (for example while the system was down,
/// or for a document entered with a past due date) are not sent in a burst;
/// only the latest one is. A non-repeating trigger older than `stale_days` is
/// not sent at all.
pub fn latest_trigger(
    due: NaiveDate,
    offsets: &[i32],
    overdue_repeat_days: Option<i32>,
    today: NaiveDate,
    stale_days: i64,
) -> Option<Trigger> {
    let mut candidates: Vec<Trigger> = offsets
        .iter()
        .filter(|o| **o >= 0)
        .map(|o| Trigger {
            kind: if *o == 0 {
                TriggerKind::Due
            } else {
                TriggerKind::Advance
            },
            offset_days: *o,
            date: due - Duration::days(i64::from(*o)),
        })
        .collect();
    if let Some(repeat) = overdue_repeat_days.filter(|r| *r > 0)
        && today > due
    {
        let elapsed = (today - due).num_days();
        let k = elapsed / i64::from(repeat);
        if k >= 1 {
            let off = i32::try_from(k * i64::from(repeat)).unwrap_or(i32::MAX);
            candidates.push(Trigger {
                kind: TriggerKind::Overdue,
                offset_days: off,
                date: due + Duration::days(i64::from(off)),
            });
        }
    }
    let latest = candidates
        .into_iter()
        .filter(|t| t.date <= today)
        .max_by_key(|t| t.date)?;
    // While the due date is still ahead, a late reminder is still useful (it
    // states the real number of days left). Once it has passed, a missed
    // non-repeating reminder older than `stale_days` is dropped.
    let stale = today > due
        && latest.kind != TriggerKind::Overdue
        && (today - latest.date).num_days() > stale_days;
    (!stale).then_some(latest)
}

/// The date-driven status a document should have, given its dates. Only
/// documents that are Active, Renewal Due, Overdue, or Expired are managed;
/// drafts, documents in review, superseded, and archived documents are not.
pub fn evaluate_status(
    current: DocumentStatus,
    next_review: Option<NaiveDate>,
    renewal: Option<NaiveDate>,
    expiration: Option<NaiveDate>,
    today: NaiveDate,
    renewal_due_days: i64,
) -> Option<DocumentStatus> {
    use DocumentStatus::*;
    if !matches!(current, Active | RenewalDue | Overdue | Expired) {
        return None;
    }
    let target = if expiration.is_some_and(|e| e < today) {
        Expired
    } else if next_review.is_some_and(|d| d < today) || renewal.is_some_and(|d| d < today) {
        Overdue
    } else if renewal.is_some_and(|d| (d - today).num_days() <= renewal_due_days) {
        RenewalDue
    } else {
        Active
    };
    (target != current).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use DocumentStatus::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn month_arithmetic_clamps() {
        assert_eq!(add_months(d("2024-01-31"), 1), d("2024-02-29"));
        assert_eq!(add_months(d("2023-01-31"), 1), d("2023-02-28"));
        assert_eq!(add_months(d("2014-06-01"), 60), d("2019-06-01"));
        assert_eq!(add_months(d("2026-11-15"), 2), d("2027-01-15"));
        assert_eq!(add_months(d("2026-03-31"), -1), d("2026-02-28"));
    }

    #[test]
    fn missed_review_cycles_for_the_ila() {
        // Reviewed 2014, five-year cycle, never reviewed again: due 2019 and 2024 were missed.
        let next = next_review(d("2014-06-01"), 60);
        assert_eq!(
            missed_cycles(next, Some(60), d("2026-10-13")),
            vec![d("2019-06-01"), d("2024-06-01")]
        );
        assert!(missed_cycles(next, Some(60), d("2019-05-31")).is_empty());
        assert_eq!(
            missed_cycles(next, Some(60), d("2019-06-01")),
            vec![d("2019-06-01")]
        );
        assert_eq!(
            missed_cycles(d("2020-01-01"), None, d("2026-01-01")),
            vec![d("2020-01-01")]
        );
    }

    #[test]
    fn reminders_follow_offsets_then_repeat() {
        let due = d("2026-12-01");
        let t = |today: &str| latest_trigger(due, &[90, 30, 7, 0], Some(7), d(today), 30);
        assert_eq!(t("2026-09-01"), None, "before the first reminder");
        assert_eq!(
            t("2026-09-02").unwrap(),
            Trigger {
                kind: TriggerKind::Advance,
                offset_days: 90,
                date: d("2026-09-02")
            }
        );
        assert_eq!(t("2026-11-15").unwrap().offset_days, 30);
        assert_eq!(t("2026-12-01").unwrap().kind, TriggerKind::Due);
        assert_eq!(
            t("2026-12-07").unwrap().kind,
            TriggerKind::Due,
            "repeats start a full interval after due"
        );
        assert_eq!(
            t("2026-12-08").unwrap(),
            Trigger {
                kind: TriggerKind::Overdue,
                offset_days: 7,
                date: d("2026-12-08")
            }
        );
        assert_eq!(t("2026-12-20").unwrap().offset_days, 14);
    }

    #[test]
    fn late_advance_reminders_still_go_out_before_the_due_date() {
        // Entered 45 days before renewal: the 90-day reminder is sent now.
        let t = latest_trigger(
            d("2026-11-27"),
            &[90, 30, 7, 0],
            Some(7),
            d("2026-10-13"),
            30,
        )
        .unwrap();
        assert_eq!((t.kind, t.offset_days), (TriggerKind::Advance, 90));
    }

    #[test]
    fn missed_reminders_are_not_sent_in_a_burst_or_late() {
        // Due long ago, no repeats: nothing (stale). With repeats: only the latest repeat.
        assert_eq!(
            latest_trigger(d("2019-06-01"), &[90, 30, 7, 0], None, d("2026-10-13"), 30),
            None
        );
        let t = latest_trigger(
            d("2019-06-01"),
            &[90, 30, 7, 0],
            Some(7),
            d("2026-10-13"),
            30,
        )
        .unwrap();
        assert_eq!(t.kind, TriggerKind::Overdue);
        assert!(t.date <= d("2026-10-13") && (d("2026-10-13") - t.date).num_days() < 7);
    }

    #[test]
    fn date_driven_statuses() {
        let today = d("2026-10-13");
        assert_eq!(
            evaluate_status(Active, Some(d("2019-06-01")), None, None, today, 90),
            Some(Overdue)
        );
        assert_eq!(
            evaluate_status(Active, None, Some(d("2026-12-01")), None, today, 90),
            Some(RenewalDue)
        );
        assert_eq!(
            evaluate_status(
                RenewalDue,
                None,
                Some(d("2026-12-01")),
                Some(d("2026-10-12")),
                today,
                90
            ),
            Some(Expired)
        );
        assert_eq!(
            evaluate_status(Overdue, Some(d("2031-10-13")), None, None, today, 90),
            Some(Active),
            "review completed"
        );
        assert_eq!(
            evaluate_status(Active, Some(today), None, None, today, 90),
            None,
            "due today is not overdue yet"
        );
        // No expiration date: never expires.
        assert_eq!(evaluate_status(Active, None, None, None, today, 90), None);
        assert_eq!(
            evaluate_status(Draft, Some(d("2019-06-01")), None, None, today, 90),
            None,
            "drafts are not managed"
        );
        assert_eq!(
            evaluate_status(Archived, None, None, Some(d("2000-01-01")), today, 90),
            None
        );
    }

    #[test]
    fn send_window_respects_local_time_across_dst() {
        let tz: Tz = "America/New_York".parse().unwrap();
        // 12:30 UTC is 08:30 EDT in summer but 07:30 EST in winter.
        let summer = "2026-07-01T12:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let winter = "2026-12-01T12:30:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(send_window_open(summer, tz, 8));
        assert!(!send_window_open(winter, tz, 8));
        // Late evening UTC is still "today" locally the previous day.
        let late = "2026-10-14T03:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(local_today(late, tz), d("2026-10-13"));
    }
}
