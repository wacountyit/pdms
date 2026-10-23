// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Webhook management and delivery.
//!
//! Each request is a JSON `POST` of an [`OutboundEvent`](super::OutboundEvent)
//! with these headers:
//! - `X-PDMS-Event`: the event name, e.g. `document.status_changed`;
//! - `X-PDMS-Delivery`: the delivery ID (the same on every retry);
//! - `X-PDMS-Signature`: `t=<unix seconds>,v1=<hex HMAC-SHA256>` of
//!   `"<t>.<body>"` with the webhook's secret.
//!
//! Any 2xx answer is success. Anything else is retried with exponential
//! backoff, up to `[integrations] webhook_max_attempts`; every attempt is
//! logged.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Capability;
use serde::Serialize;
use sha2::Sha256;
use uuid::Uuid;

use super::{DELIVER_JOB, providers};
use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::jobs::{self, NewJob};

#[derive(Clone, Debug, Serialize)]
pub struct Webhook {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub acts_as_user_id: Uuid,
    pub acts_as: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct WebhookSpec {
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub acts_as_user_id: Uuid,
    pub is_active: bool,
}

pub async fn list(core: &Core, ctx: &Ctx) -> CoreResult<Vec<Webhook>> {
    ctx.require(Capability::ManageIntegrations)?;
    Ok(sqlx::query_as!(
        Webhook,
        r#"SELECT w.id, w.name, w.url, w.events, w.acts_as_user_id, u.display_name AS acts_as, w.is_active, w.created_at
             FROM webhooks w JOIN users u ON u.id = w.acts_as_user_id ORDER BY w.name"#
    )
    .fetch_all(&core.db)
    .await?)
}

pub async fn get(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<Webhook> {
    list(core, ctx)
        .await?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or(CoreError::NotFound)
}

fn check(core: &Core, s: &mut WebhookSpec) -> CoreResult<()> {
    s.name = s.name.trim().to_owned();
    if s.name.is_empty() || s.name.chars().count() > 200 {
        return Err(CoreError::validation("name the webhook (1–200 characters)"));
    }
    let url =
        url::Url::parse(s.url.trim()).map_err(|_| CoreError::validation("enter a valid URL"))?;
    match url.scheme() {
        "https" => {}
        "http" if core.config().integrations.allow_http_webhooks => {}
        _ => {
            return Err(CoreError::validation(
                "webhook URLs must use https (plain http only when allow_http_webhooks is set)",
            ));
        }
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(CoreError::validation(
            "the URL needs a host and must not contain credentials",
        ));
    }
    s.url = url.to_string();
    let mut events: Vec<String> = s
        .events
        .iter()
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty())
        .collect();
    events.sort();
    events.dedup();
    let valid = |e: &str| {
        e == "*"
            || e.chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.' || c == '*')
                && e.contains('.')
                && (!e.contains('*') || e.ends_with(".*"))
    };
    if events.is_empty() || events.len() > 50 || !events.iter().all(|e| valid(e)) {
        return Err(CoreError::validation(
            "list the events to send, e.g. document.* or workflow.approved",
        ));
    }
    s.events = events;
    Ok(())
}

fn new_secret() -> String {
    format!(
        "whsec_{}",
        URL_SAFE_NO_PAD.encode(crate::crypto::random_bytes::<32>())
    )
}

fn event(ctx: &Ctx, action: &str, id: Uuid, name: &str) -> AuditEvent {
    AuditEvent::new(Category::Admin, action, ctx.actor.clone(), "webhook").object(id, Some(name))
}

/// Create a webhook. Returns its ID and signing secret (shown once).
pub async fn create(core: &Core, ctx: &Ctx, mut spec: WebhookSpec) -> CoreResult<(Uuid, String)> {
    ctx.require(Capability::ManageIntegrations)?;
    check(core, &mut spec)?;
    let id = Uuid::now_v7();
    let secret = new_secret();
    let enc = core
        .keys
        .encrypt_integration_secret(id, secret.as_bytes())
        .map_err(CoreError::internal)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO webhooks (id, name, url, secret_enc, events, acts_as_user_id, is_active, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        spec.name,
        spec.url,
        enc,
        &spec.events,
        spec.acts_as_user_id,
        spec.is_active,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
            CoreError::validation("the chosen service user no longer exists")
        }
        _ => CoreError::Db(e),
    })?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, "webhook.created", id, &spec.name)
                .change("url", None::<String>, spec.url.clone())
                .change("events", None::<String>, spec.events.join(" "))
                .change("acts_as_user_id", None::<Uuid>, spec.acts_as_user_id)
                .change("is_active", None::<bool>, spec.is_active),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok((id, secret))
}

pub async fn update(core: &Core, ctx: &Ctx, id: Uuid, mut spec: WebhookSpec) -> CoreResult<()> {
    let before = get(core, ctx, id).await?;
    check(core, &mut spec)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE webhooks SET name = $2, url = $3, events = $4, acts_as_user_id = $5, is_active = $6, updated_at = now()
          WHERE id = $1",
        id,
        spec.name,
        spec.url,
        &spec.events,
        spec.acts_as_user_id,
        spec.is_active
    )
    .execute(&mut *tx)
    .await?;
    let e = event(ctx, "webhook.updated", id, &spec.name)
        .change("name", before.name, spec.name.clone())
        .change("url", before.url, spec.url.clone())
        .change("events", before.events.join(" "), spec.events.join(" "))
        .change(
            "acts_as_user_id",
            before.acts_as_user_id,
            spec.acts_as_user_id,
        )
        .change("is_active", before.is_active, spec.is_active);
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Replace the signing secret. Returns the new secret (shown once).
pub async fn rotate_secret(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<String> {
    let w = get(core, ctx, id).await?;
    let secret = new_secret();
    let enc = core
        .keys
        .encrypt_integration_secret(id, secret.as_bytes())
        .map_err(CoreError::internal)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE webhooks SET secret_enc = $2, updated_at = now() WHERE id = $1",
        id,
        enc
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![event(ctx, "webhook.secret_rotated", id, &w.name)],
    )
    .await?;
    tx.commit().await?;
    Ok(secret)
}

#[derive(Clone, Debug, Serialize)]
pub struct Delivery {
    pub id: Uuid,
    pub target: String,
    pub event: String,
    pub audit_seq: i64,
    pub status: String,
    pub attempts: i32,
    pub last_status_code: Option<i32>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Recent deliveries, optionally for one webhook.
pub async fn deliveries(
    core: &Core,
    ctx: &Ctx,
    webhook: Option<Uuid>,
    limit: i64,
) -> CoreResult<Vec<Delivery>> {
    ctx.require(Capability::ManageIntegrations)?;
    Ok(sqlx::query_as!(
        Delivery,
        r#"SELECT d.id, coalesce(w.name, 'provider: ' || d.provider) AS "target!", d.event, d.audit_seq,
                  d.status::text AS "status!", d.attempts, d.last_status_code, d.last_error, d.created_at,
                  d.delivered_at
             FROM webhook_deliveries d LEFT JOIN webhooks w ON w.id = d.webhook_id
            WHERE ($1::uuid IS NULL OR d.webhook_id = $1)
            ORDER BY d.created_at DESC LIMIT $2"#,
        webhook,
        limit.clamp(1, 1000)
    )
    .fetch_all(&core.db)
    .await?)
}

/// Send a delivery again (for example after fixing the endpoint).
pub async fn redeliver(core: &Core, ctx: &Ctx, delivery_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageIntegrations)?;
    let mut tx = core.db.begin().await?;
    let d = sqlx::query!(
        "UPDATE webhook_deliveries SET status = 'pending', updated_at = now() WHERE id = $1
         RETURNING event, audit_seq",
        delivery_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    jobs::enqueue(
        &mut *tx,
        NewJob::new(
            DELIVER_JOB,
            serde_json::json!({ "delivery_id": delivery_id }),
        )
        .idempotency_key(format!("outbound:{delivery_id}:{}", Uuid::now_v7()))
        .max_attempts(core.config().integrations.webhook_max_attempts),
    )
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "webhook.redelivered",
                ctx.actor.clone(),
                "webhook_delivery",
            )
            .object(delivery_id, Some(&d.event))
            .context("audit_seq", d.audit_seq),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `t=<ts>,v1=<hex>`: HMAC-SHA256 over `"<ts>.<body>"`.
pub fn signature(secret: &[u8], timestamp: i64, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret)
        .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!(
        "t={timestamp},v1={}",
        hex::encode(mac.finalize().into_bytes())
    )
}

async fn finish(
    core: &Core,
    delivery_id: Uuid,
    attempt_id: Option<Uuid>,
    status: &str,
    code: Option<i32>,
    error: Option<String>,
) -> CoreResult<()> {
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE webhook_deliveries SET status = $2::text::outbound_status, last_status_code = $3, last_error = $4,
                delivered_at = CASE WHEN $2 = 'delivered' THEN now() END, updated_at = now()
          WHERE id = $1",
        delivery_id,
        status,
        code,
        error
    )
    .execute(&mut *tx)
    .await?;
    if let Some(a) = attempt_id {
        sqlx::query!(
            "UPDATE webhook_delivery_attempts SET finished_at = now(), status_code = $2, error = $3 WHERE id = $1",
            a,
            code,
            error
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Deliver one event to its target. `Err` means "retry later".
pub async fn deliver(core: &Core, delivery_id: Uuid, worker: &str) -> Result<(), String> {
    deliver_inner(core, delivery_id, worker)
        .await
        .map_err(|e| e.to_string())
}

async fn deliver_inner(core: &Core, delivery_id: Uuid, worker: &str) -> CoreResult<()> {
    let d = sqlx::query!(
        r#"SELECT d.webhook_id, d.provider, d.event, d.payload, d.status::text AS "status!", d.attempts,
                  w.url AS "url?", w.secret_enc AS "secret_enc?", w.is_active AS "is_active?"
             FROM webhook_deliveries d LEFT JOIN webhooks w ON w.id = d.webhook_id
            WHERE d.id = $1"#,
        delivery_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    if matches!(d.status.as_str(), "delivered" | "skipped") {
        return Ok(());
    }
    let attempt_no = d.attempts + 1;
    let attempt_id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE webhook_deliveries SET attempts = $2, updated_at = now() WHERE id = $1",
        delivery_id,
        attempt_no
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO webhook_delivery_attempts (id, delivery_id, attempt_no, worker) VALUES ($1, $2, $3, $4)",
        attempt_id,
        delivery_id,
        attempt_no,
        worker
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Integration providers.
    if let Some(key) = &d.provider {
        let Some(p) = providers(core).into_iter().find(|p| p.key() == key) else {
            return finish(
                core,
                delivery_id,
                Some(attempt_id),
                "skipped",
                None,
                Some("provider is not enabled".into()),
            )
            .await;
        };
        let event: super::OutboundEvent = serde_json::from_value(d.payload.clone())
            .map_err(|e| CoreError::internal(format!("stored event: {e}")))?;
        return match p.deliver(&event).await {
            Ok(()) => finish(core, delivery_id, Some(attempt_id), "delivered", None, None).await,
            Err(e) => {
                finish(
                    core,
                    delivery_id,
                    Some(attempt_id),
                    "failed",
                    None,
                    Some(e.clone()),
                )
                .await?;
                Err(CoreError::internal(format!("provider {key}: {e}")))
            }
        };
    }

    // Webhooks.
    let (Some(url), Some(secret_enc), Some(active), Some(webhook_id)) =
        (d.url, d.secret_enc, d.is_active, d.webhook_id)
    else {
        return finish(
            core,
            delivery_id,
            Some(attempt_id),
            "skipped",
            None,
            Some("webhook not found".into()),
        )
        .await;
    };
    if !active || !core.config().integrations.webhooks_enabled {
        return finish(
            core,
            delivery_id,
            Some(attempt_id),
            "skipped",
            None,
            Some("webhook is inactive".into()),
        )
        .await;
    }
    let secret = core
        .keys
        .decrypt_integration_secret(webhook_id, &secret_enc)
        .map_err(CoreError::internal)?;
    let body = serde_json::to_vec(&d.payload).map_err(CoreError::internal)?;
    let ts = Utc::now().timestamp();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            core.config().integrations.webhook_timeout_seconds,
        ))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("PDMS-Webhooks/1")
        .build()
        .map_err(CoreError::internal)?;
    let result = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-PDMS-Event", &d.event)
        .header("X-PDMS-Delivery", delivery_id.to_string())
        .header("X-PDMS-Signature", signature(&secret, ts, &body))
        .body(body)
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            let code = i32::from(resp.status().as_u16());
            finish(
                core,
                delivery_id,
                Some(attempt_id),
                "delivered",
                Some(code),
                None,
            )
            .await
        }
        Ok(resp) => {
            let code = i32::from(resp.status().as_u16());
            let msg = format!("endpoint answered HTTP {code}");
            finish(
                core,
                delivery_id,
                Some(attempt_id),
                "failed",
                Some(code),
                Some(msg.clone()),
            )
            .await?;
            Err(CoreError::internal(msg))
        }
        Err(e) => {
            let msg = format!("request failed: {e}");
            finish(
                core,
                delivery_id,
                Some(attempt_id),
                "failed",
                None,
                Some(msg.clone()),
            )
            .await?;
            Err(CoreError::internal(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_stable_and_keyed() {
        let a = signature(b"secret", 1_700_000_000, b"{\"x\":1}");
        assert_eq!(a, signature(b"secret", 1_700_000_000, b"{\"x\":1}"));
        assert!(a.starts_with("t=1700000000,v1="));
        assert_ne!(a, signature(b"other", 1_700_000_000, b"{\"x\":1}"));
        assert_ne!(a, signature(b"secret", 1_700_000_001, b"{\"x\":1}"));
    }
}
