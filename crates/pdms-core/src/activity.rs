// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! The dashboard's activity feed (R2 spec §8, D-095 amended).
//!
//! A feed built from the audit log could expose it, so this one is narrow by
//! construction:
//! - only a fixed list of document events ([`kind_of`]), with outcome
//!   `success`;
//! - only on documents the caller may view **now**, through the same
//!   visibility predicate as document search (D-085, D-090);
//! - the actor's name only for kinds whose actor the document page already
//!   shows to anyone who can view it (versions, comments, review steps,
//!   signatures, who created it);
//! - nothing from the event's changes or context: no before and after values,
//!   addresses, user agents or request IDs. The query does not even read them.
//!
//! Viewing the feed is not audited, like the rest of the dashboard.

use chrono::{DateTime, Duration, Utc};
use pdms_authz::{Permission, visible_documents_predicate};
use sqlx::{FromRow, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::Core;
use crate::acl::push_fragment;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

/// How far back the feed looks.
pub const WINDOW_DAYS: i64 = 90;

/// What happened, in the feed's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    Created,
    NewVersion,
    DetailsChanged,
    StatusChanged,
    ReviewSubmitted,
    StepApproved,
    StepRejected,
    StepChangesRequested,
    ReviewApproved,
    ReviewRejected,
    ReviewChangesRequested,
    ReviewCancelled,
    SignatureRecorded,
    SignatureDeclined,
    CommentPosted,
}

impl ActivityKind {
    /// The fixed phrase shown before the actor's name (if shown).
    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::Created => "Created",
            ActivityKind::NewVersion => "New version",
            ActivityKind::DetailsChanged => "Details changed",
            ActivityKind::StatusChanged => "Status changed",
            ActivityKind::ReviewSubmitted => "Submitted for review",
            ActivityKind::StepApproved => "Review step approved",
            ActivityKind::StepRejected => "Review step rejected",
            ActivityKind::StepChangesRequested => "Changes requested",
            ActivityKind::ReviewApproved => "Review approved",
            ActivityKind::ReviewRejected => "Review rejected",
            ActivityKind::ReviewChangesRequested => "Review returned for changes",
            ActivityKind::ReviewCancelled => "Review cancelled",
            ActivityKind::SignatureRecorded => "Signature recorded",
            ActivityKind::SignatureDeclined => "Signature declined",
            ActivityKind::CommentPosted => "Comment",
        }
    }

    /// Whether the document page already shows who did this to anyone who
    /// can view the document. Only then does the feed name them.
    pub fn shows_actor(self) -> bool {
        matches!(
            self,
            ActivityKind::Created
                | ActivityKind::NewVersion
                | ActivityKind::ReviewSubmitted
                | ActivityKind::StepApproved
                | ActivityKind::StepRejected
                | ActivityKind::StepChangesRequested
                | ActivityKind::SignatureRecorded
                | ActivityKind::CommentPosted
        )
    }
}

/// The audit actions the feed shows, and how. Anything not listed here is
/// never shown: views, downloads, exports, sharing and permission changes,
/// admin actions, denied or failed attempts, comment edits and deletions.
pub const ACTIONS: &[(&str, ActivityKind)] = &[
    ("document.created", ActivityKind::Created),
    ("document.file_uploaded", ActivityKind::NewVersion),
    ("document.metadata_updated", ActivityKind::DetailsChanged),
    ("document.status_changed", ActivityKind::StatusChanged),
    ("workflow.submitted", ActivityKind::ReviewSubmitted),
    ("workflow.step_approved", ActivityKind::StepApproved),
    ("workflow.step_rejected", ActivityKind::StepRejected),
    (
        "workflow.step_changes_requested",
        ActivityKind::StepChangesRequested,
    ),
    ("workflow.approved", ActivityKind::ReviewApproved),
    ("workflow.rejected", ActivityKind::ReviewRejected),
    (
        "workflow.changes_requested",
        ActivityKind::ReviewChangesRequested,
    ),
    ("workflow.cancelled", ActivityKind::ReviewCancelled),
    ("signature.recorded", ActivityKind::SignatureRecorded),
    ("signature.declined", ActivityKind::SignatureDeclined),
    ("comment.created", ActivityKind::CommentPosted),
    ("comment.replied", ActivityKind::CommentPosted),
];

pub fn kind_of(action: &str) -> Option<ActivityKind> {
    ACTIONS.iter().find(|a| a.0 == action).map(|a| a.1)
}

/// One line of the feed.
#[derive(Clone, Debug)]
pub struct ActivityItem {
    pub when: DateTime<Utc>,
    pub kind: ActivityKind,
    /// Present only when [`ActivityKind::shows_actor`].
    pub actor: Option<String>,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub department: String,
}

#[derive(FromRow)]
struct Row {
    occurred_at: DateTime<Utc>,
    action: String,
    actor_display: Option<String>,
    document_id: Uuid,
    external_id: String,
    title: String,
    department: String,
}

/// The newest `limit` feed items the caller may see.
pub async fn recent(core: &Core, ctx: &Ctx, limit: i64) -> CoreResult<Vec<ActivityItem>> {
    let actions: Vec<String> = ACTIONS.iter().map(|a| a.0.to_owned()).collect();
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT a.occurred_at, a.action, a.actor_display, d.id AS document_id,
                d.external_id, d.title, dep.name AS department
           FROM audit_log a
           JOIN documents d ON d.id = a.document_id
           JOIN departments dep ON dep.id = d.department_id
          WHERE a.outcome = 'success' AND d.deleted_at IS NULL AND a.action = ANY(",
    );
    qb.push_bind(actions)
        .push(") AND a.occurred_at >= ")
        .push_bind(Utc::now() - Duration::days(WINDOW_DAYS))
        .push(" AND ");
    // The same visibility as search: what the caller may view now.
    match ctx.principal() {
        Some(p) => {
            let frag =
                visible_documents_predicate(&core.policy, p, Permission::View, Utc::now(), "d")
                    .map_err(CoreError::internal)?;
            push_fragment(&mut qb, &frag);
        }
        None => {
            qb.push("TRUE");
        }
    }
    qb.push(" ORDER BY a.seq DESC LIMIT ")
        .push_bind(limit.clamp(1, 100));
    let rows: Vec<Row> = qb.build_query_as().fetch_all(&core.db).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let kind = kind_of(&r.action)?;
            Some(ActivityItem {
                when: r.occurred_at,
                kind,
                actor: if kind.shows_actor() {
                    r.actor_display
                } else {
                    None
                },
                document_id: r.document_id,
                external_id: r.external_id,
                title: r.title,
                department: r.department,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_actions_are_kinds() {
        assert_eq!(
            kind_of("comment.created"),
            Some(ActivityKind::CommentPosted)
        );
        for hidden in [
            "document.viewed",
            "document.downloaded",
            "document.exported",
            "acl.granted",
            "document.visibility_changed",
            "comment.edited",
            "comment.deleted",
            "comment.post_denied",
        ] {
            assert_eq!(kind_of(hidden), None, "{hidden}");
        }
    }

    #[test]
    fn names_only_where_the_document_page_does() {
        assert!(ActivityKind::CommentPosted.shows_actor());
        assert!(!ActivityKind::DetailsChanged.shows_actor());
        assert!(!ActivityKind::StatusChanged.shows_actor());
        assert!(!ActivityKind::SignatureDeclined.shows_actor());
    }
}
