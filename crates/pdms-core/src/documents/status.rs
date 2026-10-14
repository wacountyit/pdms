// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document lifecycle statuses and the transitions allowed between them.

use pdms_authz::Permission;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "document_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Draft,
    InReview,
    Active,
    RenewalDue,
    Overdue,
    Expired,
    Superseded,
    Archived,
}

impl DocumentStatus {
    pub const ALL: [DocumentStatus; 8] = [
        DocumentStatus::Draft,
        DocumentStatus::InReview,
        DocumentStatus::Active,
        DocumentStatus::RenewalDue,
        DocumentStatus::Overdue,
        DocumentStatus::Expired,
        DocumentStatus::Superseded,
        DocumentStatus::Archived,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DocumentStatus::Draft => "draft",
            DocumentStatus::InReview => "in_review",
            DocumentStatus::Active => "active",
            DocumentStatus::RenewalDue => "renewal_due",
            DocumentStatus::Overdue => "overdue",
            DocumentStatus::Expired => "expired",
            DocumentStatus::Superseded => "superseded",
            DocumentStatus::Archived => "archived",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DocumentStatus::Draft => "Draft",
            DocumentStatus::InReview => "In Review",
            DocumentStatus::Active => "Active",
            DocumentStatus::RenewalDue => "Renewal Due",
            DocumentStatus::Overdue => "Overdue",
            DocumentStatus::Expired => "Expired",
            DocumentStatus::Superseded => "Superseded",
            DocumentStatus::Archived => "Archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Statuses driven by dates. The scheduler (Phase 2) sets them; people
    /// resolve them by completing a review or renewal, or by changing status.
    pub fn is_date_driven(self) -> bool {
        matches!(
            self,
            DocumentStatus::RenewalDue | DocumentStatus::Overdue | DocumentStatus::Expired
        )
    }
}

/// Who is asking for a transition.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    Person,
    /// The scheduler applying date rules.
    System,
}

/// The permission required to move `from` → `to`, or `None` if the
/// transition is not allowed for this actor.
pub fn transition(from: DocumentStatus, to: DocumentStatus, actor: Actor) -> Option<Permission> {
    use DocumentStatus::*;
    if from == to {
        return None;
    }
    let managed = |s: DocumentStatus| matches!(s, Active | RenewalDue | Overdue | Expired);
    if actor == Actor::System && managed(from) && managed(to) {
        // The scheduler moves documents among the date-driven statuses (and
        // back to Active once the dates are resolved).
        return Some(Permission::Edit);
    }
    if to.is_date_driven() {
        return None;
    }
    let allowed = match from {
        Draft => matches!(to, InReview | Active | Archived),
        InReview => matches!(to, Draft | Active | Archived),
        Active => matches!(to, InReview | Superseded | Archived),
        // People resolve these by completing the review or renewal (or
        // correcting the dates); the scheduler then returns them to Active.
        RenewalDue | Overdue => matches!(to, InReview | Superseded | Archived),
        Expired => matches!(to, Superseded | Archived),
        Superseded => matches!(to, Archived),
        Archived => matches!(to, Active | Draft),
    };
    if !allowed {
        return None;
    }
    // Approving a document out of review needs `approve`; everything else `edit`.
    Some(if from == InReview && to == Active {
        Permission::Approve
    } else {
        Permission::Edit
    })
}

/// Statuses a person may move a document to from `from`.
pub fn next_statuses(from: DocumentStatus) -> Vec<(DocumentStatus, Permission)> {
    DocumentStatus::ALL
        .into_iter()
        .filter_map(|to| transition(from, to, Actor::Person).map(|p| (to, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use DocumentStatus::*;

    #[test]
    fn people_cannot_set_date_driven_statuses() {
        for from in DocumentStatus::ALL {
            for to in [RenewalDue, Overdue, Expired] {
                assert_eq!(
                    transition(from, to, Actor::Person),
                    None,
                    "{from:?} -> {to:?}"
                );
            }
        }
        assert!(transition(Active, Overdue, Actor::System).is_some());
        assert!(
            transition(Expired, RenewalDue, Actor::System).is_some(),
            "expiration extended"
        );
        assert!(transition(Draft, Overdue, Actor::System).is_none());
        assert!(transition(Archived, Expired, Actor::System).is_none());
    }

    #[test]
    fn approval_out_of_review_needs_approve() {
        assert_eq!(
            transition(InReview, Active, Actor::Person),
            Some(Permission::Approve)
        );
        assert_eq!(
            transition(Draft, Active, Actor::Person),
            Some(Permission::Edit)
        );
        assert_eq!(
            transition(Overdue, Active, Actor::Person),
            None,
            "complete the review instead"
        );
        assert_eq!(
            transition(Overdue, Active, Actor::System),
            Some(Permission::Edit)
        );
    }

    #[test]
    fn superseded_and_archived_are_near_terminal() {
        assert_eq!(
            next_statuses(Superseded)
                .into_iter()
                .map(|t| t.0)
                .collect::<Vec<_>>(),
            vec![Archived]
        );
        assert!(
            transition(Archived, Active, Actor::Person).is_some(),
            "archived documents can be restored"
        );
        assert!(transition(Archived, Superseded, Actor::Person).is_none());
    }

    #[test]
    fn every_status_round_trips() {
        for s in DocumentStatus::ALL {
            assert_eq!(DocumentStatus::parse(s.as_str()), Some(s));
        }
    }
}
