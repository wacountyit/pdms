// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Whether a document may be deleted under its retention policy and legal hold.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "retention_basis", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RetentionBasis {
    CreatedAt,
    EffectiveDate,
    ExpirationDate,
    ArchivedAt,
}

impl RetentionBasis {
    pub const ALL: [RetentionBasis; 4] = [
        RetentionBasis::CreatedAt,
        RetentionBasis::EffectiveDate,
        RetentionBasis::ExpirationDate,
        RetentionBasis::ArchivedAt,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RetentionBasis::CreatedAt => "created_at",
            RetentionBasis::EffectiveDate => "effective_date",
            RetentionBasis::ExpirationDate => "expiration_date",
            RetentionBasis::ArchivedAt => "archived_at",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RetentionBasis::CreatedAt => "record creation",
            RetentionBasis::EffectiveDate => "effective date",
            RetentionBasis::ExpirationDate => "expiration date",
            RetentionBasis::ArchivedAt => "archiving",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.as_str() == s)
    }
}

/// The retention rule that applies to one document.
#[derive(Clone, Debug)]
pub struct Rule {
    pub policy_name: String,
    pub retain_years: Option<i32>,
    pub basis: RetentionBasis,
}

/// Dates a retention rule can be measured from.
#[derive(Clone, Debug, Default)]
pub struct Dates {
    pub created_at: Option<DateTime<Utc>>,
    pub effective_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Eligibility {
    Deletable,
    LegalHold,
    Permanent {
        policy: String,
    },
    RetainedUntil {
        policy: String,
        until: NaiveDate,
    },
    /// The basis date is not set, so the end of retention cannot be computed.
    BasisMissing {
        policy: String,
        basis: RetentionBasis,
    },
}

impl Eligibility {
    pub fn reason(&self) -> Option<String> {
        match self {
            Eligibility::Deletable => None,
            Eligibility::LegalHold => Some("The document is on legal hold.".into()),
            Eligibility::Permanent { policy } => Some(format!(
                "Retention policy “{policy}” requires keeping this document permanently."
            )),
            Eligibility::RetainedUntil { policy, until } => Some(format!(
                "Retention policy “{policy}” requires keeping this document until {until}."
            )),
            Eligibility::BasisMissing { policy, basis } => Some(format!(
                "Retention policy “{policy}” is measured from the {}, which is not set.",
                basis.label()
            )),
        }
    }
}

pub fn eligibility(
    rule: Option<&Rule>,
    legal_hold: bool,
    dates: &Dates,
    today: NaiveDate,
) -> Eligibility {
    if legal_hold {
        return Eligibility::LegalHold;
    }
    let Some(rule) = rule else {
        return Eligibility::Deletable;
    };
    let Some(years) = rule.retain_years else {
        return Eligibility::Permanent {
            policy: rule.policy_name.clone(),
        };
    };
    let start = match rule.basis {
        RetentionBasis::CreatedAt => dates.created_at.map(|t| t.date_naive()),
        RetentionBasis::EffectiveDate => dates.effective_date,
        RetentionBasis::ExpirationDate => dates.expiration_date,
        RetentionBasis::ArchivedAt => dates.archived_at.map(|t| t.date_naive()),
    };
    let Some(start) = start else {
        return Eligibility::BasisMissing {
            policy: rule.policy_name.clone(),
            basis: rule.basis,
        };
    };
    let until = add_years(start, years);
    if today >= until {
        Eligibility::Deletable
    } else {
        Eligibility::RetainedUntil {
            policy: rule.policy_name.clone(),
            until,
        }
    }
}

/// Add whole years; February 29 becomes February 28 in non-leap years.
fn add_years(d: NaiveDate, years: i32) -> NaiveDate {
    let y = d.year().saturating_add(years);
    NaiveDate::from_ymd_opt(y, d.month(), d.day())
        .or_else(|| NaiveDate::from_ymd_opt(y, d.month(), 28))
        .unwrap_or(NaiveDate::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn rule(years: Option<i32>, basis: RetentionBasis) -> Rule {
        Rule {
            policy_name: "P".into(),
            retain_years: years,
            basis,
        }
    }

    #[test]
    fn legal_hold_beats_everything() {
        assert_eq!(
            eligibility(None, true, &Dates::default(), d(2030, 1, 1)),
            Eligibility::LegalHold
        );
    }

    #[test]
    fn no_policy_means_deletable() {
        assert_eq!(
            eligibility(None, false, &Dates::default(), d(2030, 1, 1)),
            Eligibility::Deletable
        );
    }

    #[test]
    fn permanent_and_dated_rules() {
        let dates = Dates {
            expiration_date: Some(d(2020, 2, 29)),
            ..Dates::default()
        };
        assert!(matches!(
            eligibility(
                Some(&rule(None, RetentionBasis::ExpirationDate)),
                false,
                &dates,
                d(2099, 1, 1)
            ),
            Eligibility::Permanent { .. }
        ));
        let r = rule(Some(5), RetentionBasis::ExpirationDate);
        assert_eq!(
            eligibility(Some(&r), false, &dates, d(2025, 2, 27)),
            Eligibility::RetainedUntil {
                policy: "P".into(),
                until: d(2025, 2, 28)
            }
        );
        assert_eq!(
            eligibility(Some(&r), false, &dates, d(2025, 2, 28)),
            Eligibility::Deletable
        );
        assert!(matches!(
            eligibility(
                Some(&rule(Some(5), RetentionBasis::EffectiveDate)),
                false,
                &dates,
                d(2099, 1, 1)
            ),
            Eligibility::BasisMissing { .. }
        ));
    }
}
