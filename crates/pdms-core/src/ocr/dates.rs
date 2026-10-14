// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Finding dates and terms in document text, as *suggestions*.
//!
//! Pure functions. Dates are found in the usual written forms ("January 12,
//! 2013", "the 1st day of June, 2014", "12 February 2013", "06/01/2014",
//! "2014-06-01") and classified by the nearest preceding keyword
//! ("effective", "expires", "renew", "executed", "review", ...). Terms ("a
//! term of five (5) years") and review cycles ("reviewed annually") are found
//! too. Nothing here decides anything: every result is shown to a person who
//! confirms or dismisses it.

use std::sync::LazyLock;

use chrono::{Duration, NaiveDate};
use regex::Regex;

/// The document field a suggestion is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Execution,
    Effective,
    Renewal,
    Expiration,
    NextReview,
    ReviewInterval,
}

impl Field {
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Execution => "execution_date",
            Field::Effective => "effective_date",
            Field::Renewal => "renewal_date",
            Field::Expiration => "expiration_date",
            Field::NextReview => "next_review_date",
            Field::ReviewInterval => "review_interval_months",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Field::Execution,
            Field::Effective,
            Field::Renewal,
            Field::Expiration,
            Field::NextReview,
            Field::ReviewInterval,
        ]
        .into_iter()
        .find(|f| f.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::Execution => "Execution date",
            Field::Effective => "Effective date",
            Field::Renewal => "Renewal date",
            Field::Expiration => "Expiration date",
            Field::NextReview => "Next review",
            Field::ReviewInterval => "Review interval",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub field: Field,
    pub date: Option<NaiveDate>,
    pub months: Option<i32>,
    /// The words around the find, for the person deciding.
    pub snippet: String,
    /// 0–100: how sure the classification is (not whether the date is right).
    pub confidence: u8,
}

const MONTHS: &str = r"(january|february|march|april|may|june|july|august|september|october|november|december|jan\.?|feb\.?|mar\.?|apr\.?|jun\.?|jul\.?|aug\.?|sept?\.?|oct\.?|nov\.?|dec\.?)";

#[allow(clippy::expect_used)]
fn re(s: &str) -> Regex {
    Regex::new(s).expect("static regex")
}

static MONTH_DAY_YEAR: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b{MONTHS}\s+(\d{{1,2}})(?:st|nd|rd|th)?,?\s+(\d{{4}})\b"
    ))
});
static DAY_OF_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b(\d{{1,2}})(?:st|nd|rd|th)?\s+day\s+of\s+{MONTHS},?\s+(\d{{4}})\b"
    ))
});
static DAY_MONTH_YEAR: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)\b(\d{{1,2}})\s+{MONTHS},?\s+(\d{{4}})\b")));
static NUMERIC_US: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})/(\d{1,2})/(\d{4})\b"));
static ISO: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{4})-(\d{2})-(\d{2})\b"));
static TERM: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(?:term|period)\s+of\s+([a-z-]+)?\s*(?:\(\s*(\d{1,3})\s*\))?\s*(years?|months?)\b")
});
static REVIEW_EVERY: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\breview(?:ed)?\b[^.;]{0,60}?\b(?:every|each)\s+([a-z-]+)?\s*(?:\(\s*(\d{1,2})\s*\))?\s*(years?|months?)\b",
    )
});
static ANNUAL: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:review(?:ed)?\b[^.;]{0,40}?\b(?:annually|each\s+year|every\s+year|yearly)|annual(?:ly)?\s+review)\b",
    )
});

fn month_number(s: &str) -> Option<u32> {
    let s = s.trim_end_matches('.').to_ascii_lowercase();
    let names = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    names
        .iter()
        .position(|n| s.starts_with(n))
        .and_then(|i| u32::try_from(i + 1).ok())
}

fn number_word(s: &str) -> Option<i32> {
    let words = [
        ("one", 1),
        ("two", 2),
        ("three", 3),
        ("four", 4),
        ("five", 5),
        ("six", 6),
        ("seven", 7),
        ("eight", 8),
        ("nine", 9),
        ("ten", 10),
        ("eleven", 11),
        ("twelve", 12),
        ("fifteen", 15),
        ("eighteen", 18),
        ("twenty", 20),
        ("twenty-four", 24),
        ("thirty", 30),
        ("thirty-six", 36),
        ("forty-eight", 48),
        ("sixty", 60),
    ];
    let s = s.to_ascii_lowercase();
    s.parse()
        .ok()
        .or_else(|| words.iter().find(|(w, _)| *w == s).map(|(_, n)| *n))
}

fn plausible(d: NaiveDate) -> Option<NaiveDate> {
    use chrono::Datelike;
    (1950..=2150).contains(&d.year()).then_some(d)
}

/// Every date in the text: (start, end, date), in order of appearance.
pub fn find_dates(text: &str) -> Vec<(usize, usize, NaiveDate)> {
    let mut out: Vec<(usize, usize, NaiveDate)> = Vec::new();
    let mut push = |start: usize, end: usize, d: Option<NaiveDate>| {
        if let Some(d) = d.and_then(plausible)
            && !out.iter().any(|(s, e, _)| start < *e && end > *s)
        {
            out.push((start, end, d));
        }
    };
    let num = |s: &str| s.parse::<u32>().ok();
    let year = |s: &str| s.parse::<i32>().ok();
    // Most specific first, so "1st day of June, 2014" is not also read as "June, 2014".
    for c in DAY_OF_MONTH.captures_iter(text) {
        let (Some(m), Some(day), Some(mon), Some(y)) = (c.get(0), c.get(1), c.get(2), c.get(3))
        else {
            continue;
        };
        let d = month_number(mon.as_str())
            .and_then(|mo| NaiveDate::from_ymd_opt(year(y.as_str())?, mo, num(day.as_str())?));
        push(m.start(), m.end(), d);
    }
    for c in MONTH_DAY_YEAR.captures_iter(text) {
        let (Some(m), Some(mon), Some(day), Some(y)) = (c.get(0), c.get(1), c.get(2), c.get(3))
        else {
            continue;
        };
        let d = month_number(mon.as_str())
            .and_then(|mo| NaiveDate::from_ymd_opt(year(y.as_str())?, mo, num(day.as_str())?));
        push(m.start(), m.end(), d);
    }
    for c in DAY_MONTH_YEAR.captures_iter(text) {
        let (Some(m), Some(day), Some(mon), Some(y)) = (c.get(0), c.get(1), c.get(2), c.get(3))
        else {
            continue;
        };
        let d = month_number(mon.as_str())
            .and_then(|mo| NaiveDate::from_ymd_opt(year(y.as_str())?, mo, num(day.as_str())?));
        push(m.start(), m.end(), d);
    }
    for c in NUMERIC_US.captures_iter(text) {
        let (Some(m), Some(mo), Some(day), Some(y)) = (c.get(0), c.get(1), c.get(2), c.get(3))
        else {
            continue;
        };
        let d = NaiveDate::from_ymd_opt(
            year(y.as_str()).unwrap_or(0),
            num(mo.as_str()).unwrap_or(0),
            num(day.as_str()).unwrap_or(0),
        );
        push(m.start(), m.end(), d);
    }
    for c in ISO.captures_iter(text) {
        let (Some(m), Some(y), Some(mo), Some(day)) = (c.get(0), c.get(1), c.get(2), c.get(3))
        else {
            continue;
        };
        let d = NaiveDate::from_ymd_opt(
            year(y.as_str()).unwrap_or(0),
            num(mo.as_str()).unwrap_or(0),
            num(day.as_str()).unwrap_or(0),
        );
        push(m.start(), m.end(), d);
    }
    out.sort_by_key(|(s, _, _)| *s);
    out
}

/// Keywords that classify a following date, with a base confidence.
const KEYWORDS: &[(&str, Field, u8)] = &[
    ("expires", Field::Expiration, 85),
    ("expire", Field::Expiration, 85),
    ("expiration", Field::Expiration, 85),
    ("terminate", Field::Expiration, 75),
    ("termination", Field::Expiration, 75),
    ("ending on", Field::Expiration, 75),
    ("end on", Field::Expiration, 75),
    ("through", Field::Expiration, 60),
    ("until", Field::Expiration, 60),
    ("renewal", Field::Renewal, 80),
    ("renew", Field::Renewal, 80),
    ("effective", Field::Effective, 85),
    ("commencing", Field::Effective, 80),
    ("commence", Field::Effective, 80),
    ("beginning", Field::Effective, 70),
    ("begin", Field::Effective, 70),
    ("starting", Field::Effective, 65),
    ("from", Field::Effective, 50),
    ("executed", Field::Execution, 85),
    ("execution", Field::Execution, 85),
    ("signed", Field::Execution, 75),
    ("entered into", Field::Execution, 85),
    ("dated", Field::Execution, 70),
    ("approved", Field::Execution, 55),
    ("review", Field::NextReview, 70),
];

fn floor_char(text: &str, mut i: usize) -> usize {
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i.min(text.len())
}

fn snippet(text: &str, start: usize, end: usize) -> String {
    let a = floor_char(text, start.saturating_sub(90));
    let b = ceil_char(text, (end + 60).min(text.len()));
    let s: String = text[a..b].split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{}{s}{}",
        if a > 0 { "…" } else { "" },
        if b < text.len() { "…" } else { "" }
    )
}

/// The nearest classifying keyword in the 120 characters before a date,
/// within the same sentence: (field, confidence).
fn classify(text: &str, start: usize) -> Option<(Field, u8)> {
    let from = floor_char(text, start.saturating_sub(120));
    let before = text[from..start].to_ascii_lowercase();
    // Stop at the previous sentence boundary.
    let before = match before.rfind([';', '.']) {
        Some(i) if i + 1 < before.len() && !before[..i].ends_with(char::is_numeric) => {
            &before[i + 1..]
        }
        _ => &before[..],
    };
    let mut best: Option<(usize, Field, u8)> = None;
    for (kw, field, conf) in KEYWORDS {
        if let Some(pos) = before.rfind(kw) {
            // Whole-word start.
            if pos > 0 && before.as_bytes()[pos - 1].is_ascii_alphabetic() {
                continue;
            }
            let distance = before.len() - pos;
            if best.is_none_or(|(d, _, _)| distance < d) {
                best = Some((distance, *field, *conf));
            }
        }
    }
    best.map(|(distance, field, conf)| {
        let conf = if distance > 60 {
            conf.saturating_sub(20)
        } else {
            conf
        };
        (field, conf)
    })
}

fn months_of(word: Option<&str>, digits: Option<&str>, unit: &str) -> Option<i32> {
    let n = digits
        .and_then(|d| d.parse::<i32>().ok())
        .or_else(|| word.and_then(number_word))?;
    let m = if unit.to_ascii_lowercase().starts_with("year") {
        n * 12
    } else {
        n
    };
    (1..=600).contains(&m).then_some(m)
}

/// Suggestions for a document's dates and review interval, best first per
/// field (several per field are kept, so a person can choose).
pub fn suggest(text: &str) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    for (start, end, date) in find_dates(text) {
        if let Some((field, confidence)) = classify(text, start) {
            out.push(Suggestion {
                field,
                date: Some(date),
                months: None,
                snippet: snippet(text, start, end),
                confidence,
            });
        }
    }
    // A stated term and a start date imply an end date.
    let best = |f: Field| {
        out.iter()
            .filter(|s| s.field == f)
            .max_by_key(|s| s.confidence)
            .and_then(|s| s.date)
    };
    let start_date = best(Field::Effective).or_else(|| best(Field::Execution));
    let has_expiration = out.iter().any(|s| s.field == Field::Expiration);
    for c in TERM.captures_iter(text) {
        let (Some(m), Some(unit)) = (c.get(0), c.get(3)) else {
            continue;
        };
        let Some(months) = months_of(
            c.get(1).map(|x| x.as_str()),
            c.get(2).map(|x| x.as_str()),
            unit.as_str(),
        ) else {
            continue;
        };
        if let Some(start) = start_date
            && !has_expiration
        {
            out.push(Suggestion {
                field: Field::Expiration,
                date: Some(crate::schedule::add_months(start, months) - Duration::days(1)),
                months: None,
                snippet: snippet(text, m.start(), m.end()),
                confidence: 50,
            });
        }
    }
    for c in REVIEW_EVERY.captures_iter(text) {
        let (Some(m), Some(unit)) = (c.get(0), c.get(3)) else {
            continue;
        };
        if let Some(months) = months_of(
            c.get(1).map(|x| x.as_str()),
            c.get(2).map(|x| x.as_str()),
            unit.as_str(),
        ) {
            out.push(Suggestion {
                field: Field::ReviewInterval,
                date: None,
                months: Some(months),
                snippet: snippet(text, m.start(), m.end()),
                confidence: 80,
            });
        }
    }
    for m in ANNUAL.find_iter(text) {
        out.push(Suggestion {
            field: Field::ReviewInterval,
            date: None,
            months: Some(12),
            snippet: snippet(text, m.start(), m.end()),
            confidence: 75,
        });
    }
    // One suggestion per (field, value), the most confident; best first.
    out.sort_by(|a, b| {
        (a.field, a.date, a.months)
            .cmp(&(b.field, b.date, b.months))
            .then(b.confidence.cmp(&a.confidence))
    });
    out.dedup_by(|a, b| a.field == b.field && a.date == b.date && a.months == b.months);
    // Ties go to the earlier date (an original agreement before its amendments).
    out.sort_by(|a, b| {
        a.field
            .cmp(&b.field)
            .then(b.confidence.cmp(&a.confidence))
            .then(a.date.cmp(&b.date))
    });
    out.truncate(30);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn written_forms() {
        let t = "On January 12, 2013 and Feb. 3 2014; the 1st day of June, 2014; 12 February 2013; \
                 06/01/2014; 2014-06-01; not 13/45/2014 or 3015-01-01.";
        let found: Vec<NaiveDate> = find_dates(t).into_iter().map(|x| x.2).collect();
        assert_eq!(
            found,
            vec![
                d("2013-01-12"),
                d("2014-02-03"),
                d("2014-06-01"),
                d("2013-02-12"),
                d("2014-06-01"),
                d("2014-06-01")
            ]
        );
    }

    #[test]
    fn an_partnership_agreement() {
        let t = "This Partnership Agreement is made and entered into this 12th day of February, 2013, \
                 by and between the Organization and the Partner. This Agreement shall be effective March 1, 2013 \
                 and shall continue for a term of five (5) years. The parties shall review this Agreement \
                 every five (5) years. Amendment No. 1 executed June 1, 2014.";
        let s = suggest(t);
        let get = |f: Field| s.iter().filter(|x| x.field == f).collect::<Vec<_>>();
        assert_eq!(
            get(Field::Execution)[0].date,
            Some(d("2013-02-12")),
            "{s:#?}"
        );
        assert!(
            get(Field::Execution)
                .iter()
                .any(|x| x.date == Some(d("2014-06-01")))
        );
        assert_eq!(get(Field::Effective)[0].date, Some(d("2013-03-01")));
        assert_eq!(
            get(Field::Expiration)[0].date,
            Some(d("2018-02-28")),
            "five years from March 1, 2013"
        );
        assert_eq!(get(Field::Expiration)[0].confidence, 50);
        assert_eq!(get(Field::ReviewInterval)[0].months, Some(60));
        assert!(
            get(Field::Effective)[0]
                .snippet
                .contains("effective March 1, 2013")
        );
    }

    #[test]
    fn contract_terms() {
        let t = "The term begins 10/01/2025 and expires on September 30, 2026, unless renewed. \
                 Renewal notice is due by July 1, 2026. This policy is reviewed annually.";
        let s = suggest(t);
        let first = |f: Field| s.iter().find(|x| x.field == f).unwrap();
        assert_eq!(first(Field::Effective).date, Some(d("2025-10-01")));
        assert_eq!(first(Field::Expiration).date, Some(d("2026-09-30")));
        assert_eq!(first(Field::Renewal).date, Some(d("2026-07-01")));
        assert_eq!(first(Field::ReviewInterval).months, Some(12));
        assert!(
            !s.iter()
                .any(|x| x.field == Field::Expiration && x.confidence == 50),
            "a stated expiration wins over a computed one"
        );
    }

    #[test]
    fn unclassified_dates_are_not_suggested_and_text_can_be_messy() {
        assert!(suggest("Printed 2024-01-01. Page 3 of 9.").is_empty());
        assert!(suggest("").is_empty());
        // Multi-byte text around dates does not break snippets.
        let s = suggest("Vigencia — effective «1 de enero» — effective January 1, 2027 ✓");
        assert_eq!(s[0].date, Some(d("2027-01-01")));
        assert!(s[0].snippet.contains('✓'));
    }

    #[test]
    fn fields_round_trip() {
        for f in [
            Field::Execution,
            Field::Effective,
            Field::Renewal,
            Field::Expiration,
            Field::NextReview,
            Field::ReviewInterval,
        ] {
            assert_eq!(Field::parse(f.as_str()), Some(f));
        }
    }
}
