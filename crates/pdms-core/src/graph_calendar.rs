// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Optional Outlook calendar events through Microsoft Graph.
//!
//! Off by default; ICS subscriptions cover most needs. When enabled, a
//! periodic job reconciles one all-day event per date (document reviews,
//! renewals, and expirations; appointment term ends and reappointment
//! decisions) in a shared mailbox's calendar, or a department's:
//! - a new date creates an event;
//! - a changed date or title updates it;
//! - a date that no longer applies (resolved, archived, deleted, made
//!   confidential, out of the window) removes it.
//!
//! The calendar is shared by its readers, so confidential documents are
//! never included (as with ICS feeds). [`CalendarProvider`] is the seam;
//! [`GraphProvider`] uses application permissions (client credentials).

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use chrono::{Duration, NaiveDate, Utc};
use futures_util::future::BoxFuture;
use pdms_audit::{AuditEvent, Category, Outcome};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::Core;
use crate::config::GraphCalendarConfig;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

pub const SYNC_JOB: &str = "calendar.graph_sync";

/// One all-day event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarEvent {
    pub subject: String,
    pub body_html: String,
    pub date: NaiveDate,
    /// Stable per date, so a retried create is not duplicated.
    pub transaction_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The event no longer exists (deleted in Outlook).
    #[error("event not found")]
    NotFound,
    #[error("{0}")]
    Other(String),
}

pub trait CalendarProvider: Send + Sync + fmt::Debug {
    fn key(&self) -> &'static str;
    /// Create an event; returns its ID.
    fn create<'a>(
        &'a self,
        mailbox: &'a str,
        e: &'a CalendarEvent,
    ) -> BoxFuture<'a, Result<String, ProviderError>>;
    fn update<'a>(
        &'a self,
        mailbox: &'a str,
        id: &'a str,
        e: &'a CalendarEvent,
    ) -> BoxFuture<'a, Result<(), ProviderError>>;
    /// Remove an event (already gone counts as success).
    fn delete<'a>(
        &'a self,
        mailbox: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderError>>;
}

/// Microsoft Graph with client credentials.
pub struct GraphProvider {
    cfg: GraphCalendarConfig,
    client: reqwest::Client,
    token: Mutex<Option<(String, std::time::Instant)>>,
}

impl fmt::Debug for GraphProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphProvider")
            .field("tenant_id", &self.cfg.tenant_id)
            .field("mailbox", &self.cfg.mailbox)
            .finish_non_exhaustive()
    }
}

impl GraphProvider {
    pub fn new(cfg: GraphCalendarConfig) -> CoreResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("PDMS-Calendar/1")
            .build()
            .map_err(CoreError::internal)?;
        Ok(GraphProvider {
            cfg,
            client,
            token: Mutex::new(None),
        })
    }

    async fn token(&self) -> Result<String, ProviderError> {
        let mut cached = self.token.lock().await;
        if let Some((t, until)) = cached.as_ref()
            && *until > std::time::Instant::now()
        {
            return Ok(t.clone());
        }
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.cfg.login_base_url.trim_end_matches('/'),
            self.cfg.tenant_id
        );
        let resp = self
            .client
            .post(url)
            .form(&[
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.expose()),
                ("scope", "https://graph.microsoft.com/.default"),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|e| ProviderError::Other(format!("token request: {e}")))?;
        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "token request: HTTP {}",
                resp.status()
            )));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("token response: {e}")))?;
        let token = v["access_token"]
            .as_str()
            .ok_or_else(|| ProviderError::Other("token response has no access_token".into()))?
            .to_owned();
        let secs = v["expires_in"]
            .as_u64()
            .unwrap_or(3600)
            .saturating_sub(120)
            .max(60);
        *cached = Some((
            token.clone(),
            std::time::Instant::now() + std::time::Duration::from_secs(secs),
        ));
        Ok(token)
    }

    fn url(&self, mailbox: &str, tail: &str) -> String {
        format!(
            "{}/users/{}/{tail}",
            self.cfg.graph_base_url.trim_end_matches('/'),
            url::form_urlencoded::byte_serialize(mailbox.as_bytes()).collect::<String>()
        )
    }

    fn body(e: &CalendarEvent) -> serde_json::Value {
        let next = e.date + Duration::days(1);
        json!({
            "subject": e.subject,
            "body": { "contentType": "HTML", "content": e.body_html },
            "start": { "dateTime": format!("{}T00:00:00", e.date), "timeZone": "UTC" },
            "end": { "dateTime": format!("{next}T00:00:00"), "timeZone": "UTC" },
            "isAllDay": true,
            "showAs": "free",
            "isReminderOn": false,
            "categories": ["PDMS"],
            "transactionId": e.transaction_id,
        })
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
        match resp.status() {
            s if s.is_success() => Ok(resp),
            reqwest::StatusCode::NOT_FOUND => Err(ProviderError::NotFound),
            s => {
                let text = resp.text().await.unwrap_or_default();
                Err(ProviderError::Other(format!(
                    "Graph HTTP {s}: {}",
                    text.chars().take(300).collect::<String>()
                )))
            }
        }
    }
}

impl CalendarProvider for GraphProvider {
    fn key(&self) -> &'static str {
        "graph"
    }

    fn create<'a>(
        &'a self,
        mailbox: &'a str,
        e: &'a CalendarEvent,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let token = self.token().await?;
            let resp = self
                .client
                .post(self.url(mailbox, "calendar/events"))
                .bearer_auth(token)
                .json(&Self::body(e))
                .send()
                .await
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            let v: serde_json::Value = Self::check(resp)
                .await?
                .json()
                .await
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            v["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ProviderError::Other("created event has no id".into()))
        })
    }

    fn update<'a>(
        &'a self,
        mailbox: &'a str,
        id: &'a str,
        e: &'a CalendarEvent,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let token = self.token().await?;
            let mut body = Self::body(e);
            if let Some(o) = body.as_object_mut() {
                o.remove("transactionId");
            }
            let resp = self
                .client
                .patch(self.url(mailbox, &format!("events/{id}")))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            Self::check(resp).await.map(|_| ())
        })
    }

    fn delete<'a>(
        &'a self,
        mailbox: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let token = self.token().await?;
            let resp = self
                .client
                .delete(self.url(mailbox, &format!("events/{id}")))
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            match Self::check(resp).await {
                Ok(_) | Err(ProviderError::NotFound) => Ok(()),
                Err(e) => Err(e),
            }
        })
    }
}

/// An event that should exist.
#[derive(Clone, Debug)]
struct Desired {
    key: String,
    mailbox: String,
    event: CalendarEvent,
    hash: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn desired_event(
    key: String,
    mailbox: String,
    subject: String,
    details: String,
    url: String,
    date: NaiveDate,
) -> Desired {
    let body_html = format!(
        "<p>{}</p><p><a href=\"{}\">Open in PDMS</a></p><p style=\"color:#57606a\">Created by PDMS. \
         Changes here are overwritten; update the date in PDMS.</p>",
        esc(&details),
        esc(&url)
    );
    let hash = hex::encode(Sha256::digest(
        format!("{mailbox}\u{1f}{subject}\u{1f}{body_html}\u{1f}{date}").as_bytes(),
    ));
    let transaction_id = {
        let h = Sha256::digest(format!("{key}:{date}").as_bytes());
        let mut b = [0u8; 16];
        b.copy_from_slice(&h[..16]);
        uuid::Builder::from_random_bytes(b).into_uuid().to_string()
    };
    Desired {
        key,
        mailbox,
        hash,
        event: CalendarEvent {
            subject,
            body_html,
            date,
            transaction_id,
        },
    }
}

async fn desired(core: &Core) -> CoreResult<Vec<Desired>> {
    let cfg = &core.config.graph_calendar;
    let today = crate::schedule::local_today(Utc::now(), core.config.timezone());
    let (from, to) = (
        today - Duration::days(cfg.past_days),
        today + Duration::days(cfg.future_days),
    );
    let mailbox = |code: &str| {
        cfg.department_mailboxes
            .get(code)
            .cloned()
            .unwrap_or_else(|| cfg.mailbox.clone())
    };
    let url = |path: String| {
        core.config
            .server
            .public_url
            .join(&path)
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    let docs = sqlx::query!(
        r#"SELECT d.id, d.external_id, d.title, d.status::text AS "status!", t.name AS type_name, dep.code, dep.name AS dept,
                  d.next_review_date, d.renewal_date, d.expiration_date
             FROM documents d
             JOIN document_types t ON t.id = d.document_type_id
             JOIN departments dep ON dep.id = d.department_id
            WHERE d.deleted_at IS NULL AND d.visibility <> 'restricted' AND d.status NOT IN ('superseded', 'archived')"#
    )
    .fetch_all(&core.db)
    .await?;
    for d in docs {
        for (kind, label, date) in [
            ("review", "Review due", d.next_review_date),
            ("renewal", "Renewal due", d.renewal_date),
            ("expiration", "Expires", d.expiration_date),
        ] {
            let Some(date) = date.filter(|x| *x >= from && *x <= to) else {
                continue;
            };
            out.push(desired_event(
                format!("doc:{}:{kind}", d.id),
                mailbox(&d.code),
                format!("{label}: {} {}", d.external_id, d.title),
                format!(
                    "{} · {} · {}",
                    d.type_name,
                    d.dept,
                    d.status.replace('_', " ")
                ),
                url(format!("documents/{}", d.id)),
                date,
            ));
        }
    }
    if cfg.include_appointments {
        let appts = sqlx::query!(
            r#"SELECT a.id, a.person_name, a.end_date, a.reappointment_due_date, p.id AS position_id, p.name,
                      p.body_name, dep.code AS "code?"
                 FROM appointments a
                 JOIN positions p ON p.id = a.position_id
                 LEFT JOIN departments dep ON dep.id = p.department_id
                WHERE a.status = 'active' AND p.is_active"#
        )
        .fetch_all(&core.db)
        .await?;
        for a in appts {
            for (kind, label, date) in [
                ("term_end", "Term ends", a.end_date),
                (
                    "reappointment",
                    "Reappointment decision due",
                    a.reappointment_due_date,
                ),
            ] {
                let Some(date) = date.filter(|x| *x >= from && *x <= to) else {
                    continue;
                };
                out.push(desired_event(
                    format!("appt:{}:{kind}", a.id),
                    a.code
                        .as_deref()
                        .map_or_else(|| cfg.mailbox.clone(), mailbox),
                    format!("{label}: {} ({}, {})", a.person_name, a.name, a.body_name),
                    format!("Appointment · {}", a.body_name),
                    url(format!("positions/{}", a.position_id)),
                    date,
                ));
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub errors: Vec<String>,
}

/// Reconcile Outlook with PDMS using the configured Graph provider.
pub async fn sync(core: &Core) -> CoreResult<SyncReport> {
    if !core.config.graph_calendar.enabled {
        return Ok(SyncReport::default());
    }
    let p = GraphProvider::new(core.config.graph_calendar.clone())?;
    sync_with(core, &p).await
}

/// Reconcile with any provider (one run at a time, across workers).
pub async fn sync_with(core: &Core, provider: &dyn CalendarProvider) -> CoreResult<SyncReport> {
    let mut lock = core.db.begin().await?;
    // Serialize runs across workers for the duration (transaction-scoped lock).
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pdms.graph_calendar_sync'))")
        .execute(&mut *lock)
        .await?;
    let mut report = SyncReport::default();
    let want = desired(core).await?;
    let links: HashMap<String, (String, String, String)> = sqlx::query!(
        "SELECT source_key, mailbox, event_id, content_hash FROM graph_calendar_links"
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.source_key, (r.mailbox, r.event_id, r.content_hash)))
    .collect();
    let wanted: BTreeMap<&str, &Desired> = want.iter().map(|d| (d.key.as_str(), d)).collect();

    let save = |key: String, d: Desired, id: String| async move {
        sqlx::query!(
            "INSERT INTO graph_calendar_links (source_key, mailbox, event_id, event_date, content_hash)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (source_key) DO UPDATE SET mailbox = EXCLUDED.mailbox, event_id = EXCLUDED.event_id,
                 event_date = EXCLUDED.event_date, content_hash = EXCLUDED.content_hash, updated_at = now(),
                 last_error = NULL",
            key,
            d.mailbox,
            id,
            d.event.date,
            d.hash
        )
        .execute(&core.db)
        .await
    };
    let fail = |key: String, e: String| async move {
        let _ = sqlx::query!(
            "UPDATE graph_calendar_links SET last_error = $2, updated_at = now() WHERE source_key = $1",
            key,
            e
        )
        .execute(&core.db)
        .await;
    };

    for d in &want {
        match links.get(&d.key) {
            Some((_, _, hash)) if *hash == d.hash => report.unchanged += 1,
            Some((mailbox, id, _)) if *mailbox == d.mailbox => {
                match provider.update(mailbox, id, &d.event).await {
                    Ok(()) => {
                        save(d.key.clone(), d.clone(), id.clone()).await?;
                        report.updated += 1;
                    }
                    Err(ProviderError::NotFound) => {
                        match provider.create(&d.mailbox, &d.event).await {
                            Ok(new_id) => {
                                save(d.key.clone(), d.clone(), new_id).await?;
                                report.created += 1;
                            }
                            Err(e) => {
                                fail(d.key.clone(), e.to_string()).await;
                                report.errors.push(format!("{}: {e}", d.key));
                            }
                        }
                    }
                    Err(e) => {
                        fail(d.key.clone(), e.to_string()).await;
                        report.errors.push(format!("{}: {e}", d.key));
                    }
                }
            }
            existing => {
                if let Some((mailbox, id, _)) = existing {
                    // Moved to another calendar.
                    if let Err(e) = provider.delete(mailbox, id).await {
                        report.errors.push(format!("{}: {e}", d.key));
                        continue;
                    }
                }
                match provider.create(&d.mailbox, &d.event).await {
                    Ok(id) => {
                        save(d.key.clone(), d.clone(), id).await?;
                        report.created += 1;
                    }
                    Err(e) => {
                        fail(d.key.clone(), e.to_string()).await;
                        report.errors.push(format!("{}: {e}", d.key));
                    }
                }
            }
        }
    }
    for (key, (mailbox, id, _)) in &links {
        if wanted.contains_key(key.as_str()) {
            continue;
        }
        match provider.delete(mailbox, id).await {
            Ok(()) => {
                sqlx::query!(
                    "DELETE FROM graph_calendar_links WHERE source_key = $1",
                    key
                )
                .execute(&core.db)
                .await?;
                report.deleted += 1;
            }
            Err(e) => {
                fail(key.clone(), e.to_string()).await;
                report.errors.push(format!("{key}: {e}"));
            }
        }
    }
    if report.created + report.updated + report.deleted > 0 || !report.errors.is_empty() {
        let ctx = Ctx::system("calendar-sync");
        let mut e = AuditEvent::new(
            Category::System,
            "calendar.graph_synced",
            ctx.actor,
            "calendar",
        )
        .object(provider.key(), Some("Outlook calendar"))
        .context("created", i64::try_from(report.created).unwrap_or(i64::MAX))
        .context("updated", i64::try_from(report.updated).unwrap_or(i64::MAX))
        .context("deleted", i64::try_from(report.deleted).unwrap_or(i64::MAX))
        .context(
            "errors",
            i64::try_from(report.errors.len()).unwrap_or(i64::MAX),
        );
        if !report.errors.is_empty() {
            e = e.outcome(Outcome::Failure).context(
                "first_errors",
                report.errors.iter().take(5).cloned().collect::<Vec<_>>(),
            );
        }
        pdms_audit::append(&mut lock, vec![e]).await?;
    }
    lock.commit().await?;
    Ok(report)
}

/// How many events are linked, and those whose last attempt failed
/// (integration managers).
pub async fn status(core: &Core, ctx: &Ctx) -> CoreResult<(i64, Vec<(String, String)>)> {
    ctx.require(pdms_authz::Capability::ManageIntegrations)?;
    let n = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM graph_calendar_links"#)
        .fetch_one(&core.db)
        .await?;
    let failing = sqlx::query!(
        r#"SELECT source_key, last_error AS "e!" FROM graph_calendar_links WHERE last_error IS NOT NULL
            ORDER BY updated_at DESC LIMIT 50"#
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.source_key, r.e))
    .collect();
    Ok((n, failing))
}
