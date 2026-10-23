// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document comments: threads (a comment and its replies), @mentions that
//! notify people, and edits and deletions that never lose the original text.
//!
//! - Reading comments needs `view` on the document; writing needs `view`
//!   (or `edit` when `[comments] viewers_can_comment = false`).
//! - On County-wide documents only the document's owner and PDMS
//!   administrators may comment or reply (D-091); everyone who can view still
//!   reads. Refused attempts are audited (`comment.post_denied`).
//! - Authors edit their own comments (while they may still comment). Only a
//!   comment's author deletes it; nobody else can, administrators included.
//!   Every version of the text is kept in `comment_revisions` (immutable); a
//!   deleted comment shows as deleted.
//! - Prior versions are shown to the author, to those who manage the
//!   document's permissions, and to auditors.
//! - `@someone@example.org` or `@someone` (the part of a user's email before
//!   the `@`, when unique) mentions a person; mentioned people who can view
//!   the document are notified. Replying notifies the comment's author.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::{Capability, Permission};
use regex::Regex;
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Core;
use crate::context::Ctx;
use crate::documents::access::{self, DocRef};
use crate::error::{CoreError, CoreResult};
use crate::notifications::actions::{self, Notice};
use crate::notifications::dispatch::Visibility;

pub const MAX_LEN: usize = 10_000;

#[derive(Clone, Debug, Serialize)]
pub struct Comment {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_id: Uuid,
    pub author: String,
    /// The current text (empty when deleted).
    pub body: String,
    pub mentions: Vec<(Uuid, String)>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by: Option<String>,
    pub revisions: i64,
    pub can_edit: bool,
    pub can_delete: bool,
    pub can_see_history: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Thread {
    pub comment: Comment,
    pub replies: Vec<Comment>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Revision {
    pub revision_no: i32,
    pub body: String,
    pub edited_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[allow(clippy::expect_used)]
static MENTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^\w@.])@([A-Za-z0-9._%+-]+(?:@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+)?)")
        .expect("static regex")
});

/// The handles mentioned in a text (`name@example.org` or `name`), lowercased.
pub fn mentioned_handles(body: &str) -> Vec<String> {
    let mut out: Vec<String> = MENTION
        .captures_iter(body)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().trim_end_matches('.').to_ascii_lowercase())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Active users the handles refer to. A bare handle must match exactly one
/// user's email local part.
async fn resolve_mentions(conn: &mut PgConnection, body: &str) -> CoreResult<Vec<Uuid>> {
    let handles = mentioned_handles(body);
    if handles.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query!(
        "SELECT id, lower(email) AS \"email!\" FROM users WHERE status = 'active'
          AND (lower(email) = ANY($1) OR lower(split_part(email, '@', 1)) = ANY($1))",
        &handles
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out = BTreeSet::new();
    for h in &handles {
        let exact: Vec<Uuid> = rows
            .iter()
            .filter(|r| &r.email == h)
            .map(|r| r.id)
            .collect();
        let local: Vec<Uuid> = rows
            .iter()
            .filter(|r| r.email.split('@').next() == Some(h.as_str()))
            .map(|r| r.id)
            .collect();
        match (exact.as_slice(), local.as_slice()) {
            ([one], _) | ([], [one]) => {
                out.insert(*one);
            }
            _ => {}
        }
    }
    Ok(out.into_iter().collect())
}

fn clean(body: &str) -> CoreResult<String> {
    let body = body.trim().replace("\r\n", "\n");
    if body.is_empty() {
        return Err(CoreError::validation("write a comment"));
    }
    if body.chars().count() > MAX_LEN {
        return Err(CoreError::validation(format!(
            "comments are limited to {MAX_LEN} characters"
        )));
    }
    Ok(body)
}

fn base_permission(core: &Core) -> Permission {
    if core.config.comments.viewers_can_comment {
        Permission::View
    } else {
        Permission::Edit
    }
}

/// Rule: on County-wide documents only the owner and administrators comment.
fn county_wide_allows(ctx: &Ctx, doc: &DocRef) -> bool {
    if doc.attrs.visibility != pdms_authz::Visibility::CountyWide {
        return true;
    }
    match ctx.principal() {
        None => ctx.is_system(),
        Some(p) => p.user_id == doc.attrs.owner_id || p.has_org_role(pdms_authz::role_keys::ADMIN),
    }
}

/// Authorize writing a comment; a refusal by the County-wide rule is audited
/// as `denied_action`. `comment_id` is set only when an existing comment is
/// involved (an edit); `parent_id` only for a reply.
async fn can_write(
    core: &Core,
    ctx: &Ctx,
    document_id: Uuid,
    denied_action: &str,
    comment_id: Option<Uuid>,
    parent_id: Option<Uuid>,
) -> CoreResult<DocRef> {
    let doc = access::authorize(core, ctx, document_id, base_permission(core)).await?;
    if !county_wide_allows(ctx, &doc) {
        let mut e = doc
            .event(ctx, Category::Content, denied_action)
            .outcome(pdms_audit::Outcome::Denied)
            .context("reason", "county_wide_owner_or_admin_only");
        if let Some(c) = comment_id {
            e = e.context("comment_id", c);
        }
        if let Some(p) = parent_id {
            e = e.context("parent_id", p);
        }
        pdms_audit::record(&core.db, vec![e]).await?;
        return Err(CoreError::Forbidden(None));
    }
    Ok(doc)
}

/// Whether the caller may post on this document now (no audit entry; for showing or hiding forms).
pub async fn can_comment(core: &Core, ctx: &Ctx, document_id: Uuid) -> CoreResult<bool> {
    if !access::can(core, ctx, document_id, base_permission(core)).await? {
        return Ok(false);
    }
    let Some(doc) = access::load(core, document_id).await? else {
        return Ok(false);
    };
    Ok(county_wide_allows(ctx, &doc))
}

/// A document's comments as threads, oldest first (requires `view`).
pub async fn list(core: &Core, ctx: &Ctx, document_id: Uuid) -> CoreResult<Vec<Thread>> {
    let doc = access::authorize(core, ctx, document_id, Permission::View).await?;
    let moderator = access::can(core, ctx, document_id, Permission::ManagePermissions).await?;
    let may_write = access::can(core, ctx, document_id, base_permission(core)).await?
        && county_wide_allows(ctx, &doc);
    let auditor = ctx.can(Capability::ViewAudit);
    let me = ctx.user_id();
    let rows = sqlx::query!(
        r#"SELECT c.id, c.parent_id, c.author_id, a.display_name AS author, c.body, c.mentions, c.created_at,
                  c.edited_at, c.deleted_at, d.display_name AS "deleted_by?",
                  (SELECT count(*) FROM comment_revisions r WHERE r.comment_id = c.id) AS "revisions!"
             FROM comments c
             JOIN users a ON a.id = c.author_id
             LEFT JOIN users d ON d.id = c.deleted_by
            WHERE c.document_id = $1 ORDER BY c.created_at, c.id"#,
        doc.id()
    )
    .fetch_all(&core.db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().flat_map(|r| r.mentions.clone()).collect();
    let names = sqlx::query!(
        "SELECT id, display_name FROM users WHERE id = ANY($1)",
        &ids
    )
    .fetch_all(&core.db)
    .await?;
    let comments: Vec<Comment> = rows
        .into_iter()
        .map(|r| {
            let mine = me == Some(r.author_id);
            let deleted = r.deleted_at.is_some();
            Comment {
                mentions: r
                    .mentions
                    .iter()
                    .filter_map(|m| {
                        names
                            .iter()
                            .find(|n| n.id == *m)
                            .map(|n| (n.id, n.display_name.clone()))
                    })
                    .collect(),
                body: if deleted { String::new() } else { r.body },
                can_edit: mine && !deleted && may_write,
                can_delete: mine && !deleted,
                can_see_history: (mine || moderator || auditor) && r.revisions > 1
                    || (deleted && (mine || moderator || auditor)),
                id: r.id,
                parent_id: r.parent_id,
                author_id: r.author_id,
                author: r.author,
                created_at: r.created_at,
                edited_at: r.edited_at,
                deleted_at: r.deleted_at,
                deleted_by: r.deleted_by,
                revisions: r.revisions,
            }
        })
        .collect();
    let mut threads: Vec<Thread> = comments
        .iter()
        .filter(|c| c.parent_id.is_none())
        .map(|c| Thread {
            comment: c.clone(),
            replies: Vec::new(),
        })
        .collect();
    for c in comments.into_iter().filter(|c| c.parent_id.is_some()) {
        if let Some(t) = threads
            .iter_mut()
            .find(|t| Some(t.comment.id) == c.parent_id)
        {
            t.replies.push(c);
        }
    }
    Ok(threads)
}

fn event(ctx: &Ctx, doc: &DocRef, action: &str, comment_id: Uuid) -> AuditEvent {
    doc.event(ctx, Category::Content, action)
        .context("comment_id", comment_id)
}

#[allow(clippy::too_many_arguments)]
async fn notify(
    conn: &mut PgConnection,
    core: &Core,
    ctx: &Ctx,
    doc: &DocRef,
    comment_id: Uuid,
    key: &str,
    users: BTreeSet<Uuid>,
    subject: String,
    body: String,
) -> CoreResult<()> {
    let mut users = users;
    if let Some(me) = ctx.user_id() {
        users.remove(&me);
    }
    if users.is_empty() {
        return Ok(());
    }
    let visibility = Visibility::document(core, doc.attrs.clone()).await?;
    actions::send(
        conn,
        core,
        ctx,
        Notice {
            subject,
            body,
            link: format!("/documents/{}#comment-{comment_id}", doc.id()),
            document_id: Some(doc.id()),
            appointment_id: None,
            dedupe_key: key.to_owned(),
        },
        users,
        &visibility,
        event(ctx, doc, "notification.created", comment_id),
    )
    .await?;
    Ok(())
}

fn excerpt(body: &str) -> String {
    let one_line: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 280 {
        format!("{}…", one_line.chars().take(280).collect::<String>())
    } else {
        one_line
    }
}

/// Add a comment, or a reply to `parent`. Returns its ID.
pub async fn post(
    core: &Core,
    ctx: &Ctx,
    document_id: Uuid,
    parent: Option<Uuid>,
    body: &str,
) -> CoreResult<Uuid> {
    let author = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let doc = can_write(core, ctx, document_id, "comment.post_denied", None, parent).await?;
    let body = clean(body)?;
    let mut tx = core.db.begin().await?;
    let parent_author = match parent {
        Some(p) => {
            let r = sqlx::query!(
                "SELECT author_id, parent_id, document_id, deleted_at FROM comments WHERE id = $1",
                p
            )
            .fetch_optional(&mut *tx)
            .await?
            .filter(|r| r.document_id == document_id)
            .ok_or(CoreError::NotFound)?;
            if r.parent_id.is_some() {
                return Err(CoreError::validation(
                    "reply to the first comment of the thread",
                ));
            }
            Some(r.author_id)
        }
        None => None,
    };
    let mentions = resolve_mentions(&mut tx, &body).await?;
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO comments (id, document_id, parent_id, author_id, body, mentions) VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        document_id,
        parent,
        author,
        body,
        &mentions
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO comment_revisions (id, comment_id, revision_no, body, edited_by) VALUES ($1, $2, 1, $3, $4)",
        Uuid::now_v7(),
        id,
        body,
        author
    )
    .execute(&mut *tx)
    .await?;
    let mut e = event(
        ctx,
        &doc,
        if parent.is_some() {
            "comment.replied"
        } else {
            "comment.created"
        },
        id,
    )
    .context("body", body.clone())
    .context(
        "mentions",
        mentions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    );
    if let Some(p) = parent {
        e = e.context("parent_id", p);
    }
    pdms_audit::append(&mut tx, vec![e]).await?;
    let who = ctx.actor.display.clone().unwrap_or_default();
    notify(
        &mut tx,
        core,
        ctx,
        &doc,
        id,
        &format!("comment:{id}:mentions:1"),
        mentions.iter().copied().collect(),
        format!("{who} mentioned you: {}", doc.label()),
        format!(
            "{who} mentioned you in a comment on {} \"{}\": {}",
            doc.external_id,
            doc.title,
            excerpt(&body)
        ),
    )
    .await?;
    if let Some(pa) = parent_author.filter(|pa| !mentions.contains(pa)) {
        notify(
            &mut tx,
            core,
            ctx,
            &doc,
            id,
            &format!("comment:{id}:reply"),
            [pa].into_iter().collect(),
            format!("{who} replied to your comment: {}", doc.label()),
            format!(
                "{who} replied on {} \"{}\": {}",
                doc.external_id,
                doc.title,
                excerpt(&body)
            ),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

async fn load_one(
    conn: &mut PgConnection,
    id: Uuid,
) -> CoreResult<(Uuid, Uuid, String, Option<DateTime<Utc>>, Vec<Uuid>)> {
    let r = sqlx::query!(
        "SELECT document_id, author_id, body, deleted_at, mentions FROM comments WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(conn)
    .await?
    .ok_or(CoreError::NotFound)?;
    Ok((r.document_id, r.author_id, r.body, r.deleted_at, r.mentions))
}

/// Change your own comment. The earlier text is kept; newly mentioned people are notified.
pub async fn edit(core: &Core, ctx: &Ctx, comment_id: Uuid, body: &str) -> CoreResult<()> {
    let me = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let body = clean(body)?;
    let mut conn = core.db.acquire().await?;
    let (doc_id, author, _, _, _) = load_one(&mut conn, comment_id).await?;
    drop(conn);
    let doc = can_write(
        core,
        ctx,
        doc_id,
        "comment.edit_denied",
        Some(comment_id),
        None,
    )
    .await?;
    let mut tx = core.db.begin().await?;
    let (_, _, before, deleted, old_mentions) = load_one(&mut tx, comment_id).await?;
    if author != me {
        drop(tx);
        pdms_audit::record(
            &core.db,
            vec![
                event(ctx, &doc, "comment.edit_denied", comment_id)
                    .outcome(pdms_audit::Outcome::Denied),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(None));
    }
    if deleted.is_some() {
        return Err(CoreError::validation("this comment was deleted"));
    }
    if before == body {
        return Ok(());
    }
    let mentions = resolve_mentions(&mut tx, &body).await?;
    let rev = sqlx::query_scalar!(
        r#"SELECT coalesce(max(revision_no), 0) + 1 AS "n!" FROM comment_revisions WHERE comment_id = $1"#,
        comment_id
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE comments SET body = $2, mentions = $3, edited_at = now() WHERE id = $1",
        comment_id,
        body,
        &mentions
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO comment_revisions (id, comment_id, revision_no, body, edited_by) VALUES ($1, $2, $3, $4, $5)",
        Uuid::now_v7(),
        comment_id,
        rev,
        body,
        me
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, &doc, "comment.edited", comment_id)
                .context("revision", rev)
                .change("body", before, body.clone()),
        ],
    )
    .await?;
    let new: BTreeSet<Uuid> = mentions
        .iter()
        .filter(|m| !old_mentions.contains(m))
        .copied()
        .collect();
    let who = ctx.actor.display.clone().unwrap_or_default();
    notify(
        &mut tx,
        core,
        ctx,
        &doc,
        comment_id,
        &format!("comment:{comment_id}:mentions:{rev}"),
        new,
        format!("{who} mentioned you: {}", doc.label()),
        format!(
            "{who} mentioned you in a comment on {} \"{}\": {}",
            doc.external_id,
            doc.title,
            excerpt(&body)
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Delete a comment. Only its author may (D-091): administrators, the
/// document's owner, and permission managers may not. The text stays in the
/// history; the thread shows it as deleted.
pub async fn delete(core: &Core, ctx: &Ctx, comment_id: Uuid) -> CoreResult<()> {
    let me = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let mut conn = core.db.acquire().await?;
    let (doc_id, author, _, _, _) = load_one(&mut conn, comment_id).await?;
    drop(conn);
    let doc = access::authorize(core, ctx, doc_id, Permission::View).await?;
    if author != me {
        pdms_audit::record(
            &core.db,
            vec![
                event(ctx, &doc, "comment.delete_denied", comment_id)
                    .outcome(pdms_audit::Outcome::Denied),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(None));
    }
    let mut tx = core.db.begin().await?;
    let (_, _, body, deleted, _) = load_one(&mut tx, comment_id).await?;
    if deleted.is_some() {
        return Ok(());
    }
    sqlx::query!(
        "UPDATE comments SET deleted_at = now(), deleted_by = $2 WHERE id = $1",
        comment_id,
        me
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![event(ctx, &doc, "comment.deleted", comment_id).context("body", body)],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Every version of a comment, original first (the author, moderators, and
/// auditors).
pub async fn history(core: &Core, ctx: &Ctx, comment_id: Uuid) -> CoreResult<Vec<Revision>> {
    let mut conn = core.db.acquire().await?;
    let (doc_id, author, _, _, _) = load_one(&mut conn, comment_id).await?;
    drop(conn);
    let doc = access::authorize(core, ctx, doc_id, Permission::View).await?;
    let allowed = ctx.user_id() == Some(author)
        || ctx.can(Capability::ViewAudit)
        || access::can(core, ctx, doc_id, Permission::ManagePermissions).await?;
    if !allowed {
        pdms_audit::record(
            &core.db,
            vec![
                event(ctx, &doc, "comment.history_denied", comment_id)
                    .outcome(pdms_audit::Outcome::Denied),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(None));
    }
    Ok(sqlx::query_as!(
        Revision,
        r#"SELECT r.revision_no, r.body, u.display_name AS "edited_by?", r.created_at
             FROM comment_revisions r LEFT JOIN users u ON u.id = r.edited_by
            WHERE r.comment_id = $1 ORDER BY r.revision_no"#,
        comment_id
    )
    .fetch_all(&core.db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_found() {
        assert_eq!(
            mentioned_handles(
                "Thanks @carol@example.org and @Gus, see section 4. Email me@x.org not a mention. (@dana)"
            ),
            vec!["carol@example.org", "dana", "gus"]
        );
        assert!(mentioned_handles("no mentions here").is_empty());
        assert_eq!(mentioned_handles("@alice."), vec!["alice"]);
    }
}
