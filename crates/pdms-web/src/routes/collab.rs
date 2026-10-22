// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Comments, extracted text with date suggestions, and the Outlook calendar
//! status page.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use axum_extra::extract::Form;
use pdms_authz::Capability;
use pdms_core::comments::{self, Comment};
use pdms_core::ocr;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, redirect_with};

pub struct CommentView {
    pub id: Uuid,
    pub author: String,
    pub when: String,
    pub edited: String,
    pub deleted: String,
    /// Escaped HTML with mentions emphasized.
    pub body_html: String,
    pub body: String,
    pub can_edit: bool,
    pub can_delete: bool,
    pub can_see_history: bool,
}

pub struct ThreadView {
    pub c: CommentView,
    pub replies: Vec<CommentView>,
}

pub struct SuggestionView {
    pub id: Uuid,
    pub field: &'static str,
    pub value: String,
    pub snippet: String,
    pub confidence: i16,
    pub status: String,
    pub decided: String,
    pub pending: bool,
}

/// What the comment and suggestion sections of the document page need.
pub struct CollabView {
    pub threads: Vec<ThreadView>,
    pub can_comment: bool,
    pub count: usize,
    pub text: Option<ocr::ExtractedText>,
    pub text_when: String,
    pub suggestions: Vec<SuggestionView>,
    pub pending: usize,
    pub can_decide: bool,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Escape, keep line breaks, and emphasize @mentions of resolved people.
pub fn render_body(body: &str, mentioned: bool) -> String {
    let escaped = esc(body);
    let marked = if mentioned {
        let re = regex_mention();
        re.replace_all(&escaped, |c: &regex::Captures<'_>| {
            format!("{}<strong class=\"mention\">@{}</strong>", &c[1], &c[2])
        })
        .into_owned()
    } else {
        escaped
    };
    marked.replace('\n', "<br>")
}

fn regex_mention() -> regex::Regex {
    regex::Regex::new(r"(^|[^\w@.])@([A-Za-z0-9._%+-]+(?:@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+)?)")
        .unwrap_or_else(|_| unreachable!("static regex"))
}

fn comment_view(state: &AppState, c: &Comment) -> CommentView {
    CommentView {
        id: c.id,
        author: c.author.clone(),
        when: fmt_time(state.tz, c.created_at),
        edited: c
            .edited_at
            .map(|t| format!("edited {}", fmt_time(state.tz, t)))
            .unwrap_or_default(),
        deleted: c
            .deleted_at
            .map(|t| {
                format!(
                    "Deleted{} {}",
                    c.deleted_by
                        .as_ref()
                        .map(|b| format!(" by {b}"))
                        .unwrap_or_default(),
                    fmt_time(state.tz, t)
                )
            })
            .unwrap_or_default(),
        body_html: render_body(&c.body, !c.mentions.is_empty()),
        body: c.body.clone(),
        can_edit: c.can_edit,
        can_delete: c.can_delete,
        can_see_history: c.can_see_history,
    }
}

pub async fn collab_view(
    state: &AppState,
    user: &CurrentUser,
    document_id: Uuid,
    can_edit: bool,
) -> AppResult<CollabView> {
    let core = &state.core;
    let threads = comments::list(core, &user.ctx, document_id).await?;
    let count = threads.iter().map(|t| 1 + t.replies.len()).sum();
    let can_comment = comments::can_comment(core, &user.ctx, document_id).await?;
    let text = ocr::latest_text(core, &user.ctx, document_id).await?;
    let suggestions: Vec<SuggestionView> = ocr::suggestions(core, &user.ctx, document_id)
        .await?
        .into_iter()
        .map(|s| SuggestionView {
            id: s.id,
            field: s.field_label,
            value: s.value_label(),
            pending: s.status == "pending",
            decided: match (&s.decided_by, s.decided_at) {
                (Some(by), Some(at)) => format!("{} by {by}, {}", s.status, fmt_time(state.tz, at)),
                _ => String::new(),
            },
            snippet: s.snippet,
            confidence: s.confidence,
            status: s.status,
        })
        .collect();
    Ok(CollabView {
        threads: threads
            .iter()
            .map(|t| ThreadView {
                c: comment_view(state, &t.comment),
                replies: t.replies.iter().map(|r| comment_view(state, r)).collect(),
            })
            .collect(),
        can_comment,
        count,
        text_when: text
            .as_ref()
            .map(|t| fmt_time(state.tz, t.extracted_at))
            .unwrap_or_default(),
        text,
        pending: suggestions.iter().filter(|s| s.pending).count(),
        suggestions,
        can_decide: can_edit,
    })
}

fn back(id: Uuid, anchor: &str) -> String {
    format!("/documents/{id}#{anchor}")
}

#[derive(Deserialize)]
pub struct CommentForm {
    #[serde(default)]
    body: String,
    #[serde(default)]
    parent: String,
}

pub async fn post_comment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<CommentForm>,
) -> AppResult<Response> {
    let parent = f.parent.parse::<Uuid>().ok();
    match comments::post(&state.core, &user.ctx, id, parent, &f.body).await {
        Ok(c) => Ok(redirect_with(
            &state,
            &back(id, &format!("comment-{c}")),
            true,
            "Comment posted.",
        )),
        Err(e) => done::<()>(&state, &back(id, "comments"), Err(e), ""),
    }
}

pub async fn edit_comment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, c)): Path<(Uuid, Uuid)>,
    Form(f): Form<CommentForm>,
) -> AppResult<Response> {
    let r = comments::edit(&state.core, &user.ctx, c, &f.body).await;
    done(
        &state,
        &back(id, &format!("comment-{c}")),
        r,
        "Comment updated. The earlier text is kept.",
    )
}

pub async fn delete_comment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, c)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = comments::delete(&state.core, &user.ctx, c).await;
    done(
        &state,
        &back(id, "comments"),
        r,
        "Comment deleted. Its text is kept in the history.",
    )
}

#[derive(Template)]
#[template(path = "documents/comment_history.html")]
struct HistoryPage {
    l: Layout,
    document_id: Uuid,
    revisions: Vec<(i32, String, String, String)>,
}

pub async fn comment_history(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path((id, c)): Path<(Uuid, Uuid)>,
) -> AppResult<Html<String>> {
    let revs = comments::history(&state.core, &user.ctx, c).await?;
    let page = HistoryPage {
        l: Layout::new(&state, &user, flash, "Comment history", "documents"),
        document_id: id,
        revisions: revs
            .into_iter()
            .map(|r| {
                (
                    r.revision_no,
                    r.edited_by.unwrap_or_default(),
                    fmt_time(state.tz, r.created_at),
                    render_body(&r.body, false),
                )
            })
            .collect(),
    };
    Ok(Html(page.render()?))
}

pub async fn accept_suggestion(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, s)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = ocr::accept(&state.core, &user.ctx, s).await;
    done(
        &state,
        &back(id, "suggestions"),
        r,
        "Date confirmed and saved to the document.",
    )
}

pub async fn dismiss_suggestion(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, s)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = ocr::dismiss(&state.core, &user.ctx, s).await;
    done(&state, &back(id, "suggestions"), r, "Suggestion dismissed.")
}

pub async fn reprocess(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = ocr::reprocess(&state.core, &user.ctx, id).await;
    done(
        &state,
        &back(id, "suggestions"),
        r,
        "The file will be read again in the background. Refresh in a minute.",
    )
}

// ---------------------------------------------------------------------------
// Outlook calendar (administration)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/outlook.html")]
struct OutlookPage {
    l: Layout,
    enabled: bool,
    mailbox: String,
    departments: Vec<(String, String)>,
    events: i64,
    failing: Vec<(String, String)>,
    include_appointments: bool,
    interval: u64,
    ocr_enabled: bool,
    ocr_url: String,
}

pub async fn outlook_page(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageIntegrations, "/admin/outlook")
        .await?;
    let cfg = &state.core.config.graph_calendar;
    let (events, failing) = pdms_core::graph_calendar::status(&state.core, &user.ctx).await?;
    let page = OutlookPage {
        l: Layout::new(&state, &user, flash, "Outlook calendar and OCR", "admin")
            .admin_tab("outlook"),
        enabled: cfg.enabled,
        mailbox: cfg.mailbox.clone(),
        departments: cfg
            .department_mailboxes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        events,
        failing,
        include_appointments: cfg.include_appointments,
        interval: cfg.sync_interval_minutes,
        ocr_enabled: state.core.ocr.is_some(),
        ocr_url: state.core.config.ocr.sidecar_url.clone(),
    };
    Ok(Html(page.render()?))
}

pub async fn outlook_sync(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageIntegrations, "/admin/outlook")
        .await?;
    match pdms_core::graph_calendar::sync(&state.core).await {
        Ok(r) if r.errors.is_empty() => Ok(redirect_with(
            &state,
            "/admin/outlook",
            true,
            &format!(
                "Synchronized: {} created, {} updated, {} removed, {} unchanged.",
                r.created, r.updated, r.deleted, r.unchanged
            ),
        )),
        Ok(r) => Ok(redirect_with(
            &state,
            "/admin/outlook",
            false,
            &format!("{} problem(s); first: {}", r.errors.len(), r.errors[0]),
        )),
        Err(e) => done::<()>(&state, "/admin/outlook", Err(e), ""),
    }
}
