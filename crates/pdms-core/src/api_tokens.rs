// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Scoped API tokens and the API request log.
//!
//! A token acts as the user who created it, limited to its scopes: a request
//! may do only what the user may do *and* what the scope allows. Every
//! permission check still goes through the central authorization engine with
//! the user's current principal, so revoking a grant, deactivating the user,
//! or revoking the token takes effect on the next request.

use chrono::{DateTime, Duration, Utc};
use pdms_audit::{AuditEvent, AuthMethod, Category};
use pdms_authz::{Capability, Principal};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::Core;
use crate::context::{ClientInfo, Ctx};
use crate::crypto::{new_token, sha256};
use crate::error::{CoreError, CoreResult};
use crate::model::UserStatus;

/// Prefix of every token, so leaked tokens are easy to recognize and scan for.
pub const TOKEN_PREFIX: &str = "pdms_";

/// (scope, what it allows).
pub const SCOPES: &[(&str, &str)] = &[
    (
        "documents:read",
        "Search and read documents, versions, and files you can view",
    ),
    (
        "documents:write",
        "Create documents and change their status where you have permission",
    ),
    (
        "calendar:read",
        "Read review, renewal, expiration, and appointment dates",
    ),
    (
        "appointments:read",
        "Read positions and appointments you can see",
    ),
    ("reports:read", "Run reports you have access to"),
    (
        "audit:read",
        "Search the audit log (auditors and administrators only)",
    ),
];

pub fn is_scope(s: &str) -> bool {
    SCOPES.iter().any(|(k, _)| *k == s)
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_name: String,
    pub name: String,
    pub token_prefix: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiToken {
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

async fn list(db: &PgPool, user: Option<Uuid>) -> CoreResult<Vec<ApiToken>> {
    Ok(sqlx::query_as!(
        ApiToken,
        r#"SELECT t.id, t.user_id, u.display_name AS user_name, t.name, t.token_prefix, t.scopes,
                  t.created_at, t.expires_at, t.last_used_at, t.revoked_at
             FROM api_tokens t JOIN users u ON u.id = t.user_id
            WHERE ($1::uuid IS NULL OR t.user_id = $1)
            ORDER BY t.revoked_at NULLS FIRST, t.created_at DESC"#,
        user
    )
    .fetch_all(db)
    .await?)
}

/// The caller's tokens.
pub async fn list_mine(core: &Core, ctx: &Ctx) -> CoreResult<Vec<ApiToken>> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    list(&core.db, Some(user)).await
}

/// Every token (integration managers).
pub async fn list_all(core: &Core, ctx: &Ctx) -> CoreResult<Vec<ApiToken>> {
    ctx.require(Capability::ManageIntegrations)?;
    list(&core.db, None).await
}

fn token_event(ctx: &Ctx, action: &str, id: Uuid, name: &str, owner: Uuid) -> AuditEvent {
    AuditEvent::new(Category::Admin, action, ctx.actor.clone(), "api_token")
        .object(id, Some(name))
        .context("token_user_id", owner)
}

/// Create a token for the caller. Returns its ID and the secret (shown once).
pub async fn create(
    core: &Core,
    ctx: &Ctx,
    name: &str,
    scopes: &[String],
    days: i64,
) -> CoreResult<(Uuid, String)> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    if !core.config.api.enabled {
        return Err(CoreError::validation("the API is turned off"));
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(CoreError::validation("name the token (1–200 characters)"));
    }
    let mut scopes: Vec<String> = scopes.iter().map(|s| s.trim().to_owned()).collect();
    scopes.sort();
    scopes.dedup();
    if scopes.is_empty() || !scopes.iter().all(|s| is_scope(s)) {
        return Err(CoreError::validation("choose one or more valid scopes"));
    }
    let max = core.config.api.max_token_days;
    if !(1..=max).contains(&days) {
        return Err(CoreError::validation(format!(
            "tokens may last 1–{max} days"
        )));
    }
    let secret = format!("{TOKEN_PREFIX}{}", new_token());
    let prefix: String = secret.chars().take(TOKEN_PREFIX.len() + 6).collect();
    let id = Uuid::now_v7();
    let expires_at = Utc::now() + Duration::days(days);
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, scopes, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $2, $7)",
        id,
        user,
        name,
        &sha256(secret.as_bytes())[..],
        prefix,
        &scopes,
        expires_at
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            token_event(ctx, "api_token.created", id, name, user)
                .context("scopes", scopes.join(" "))
                .context("expires_at", expires_at.to_rfc3339()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok((id, secret))
}

/// Revoke a token: the caller's own, or anyone's for integration managers.
pub async fn revoke(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let user = ctx.user_id();
    let any = ctx.can(Capability::ManageIntegrations);
    let mut tx = core.db.begin().await?;
    let r = sqlx::query!(
        "UPDATE api_tokens SET revoked_at = now(), revoked_by = $2
          WHERE id = $1 AND revoked_at IS NULL AND ($3 OR user_id = $2)
         RETURNING name, user_id",
        id,
        user,
        any
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![token_event(
            ctx,
            "api_token.revoked",
            id,
            &r.name,
            r.user_id,
        )],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A request authenticated by a token.
#[derive(Clone, Debug)]
pub struct TokenAuth {
    pub token_id: Uuid,
    pub user_id: Uuid,
    pub scopes: Vec<String>,
    pub ctx: Ctx,
}

impl TokenAuth {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

/// Look up a presented token. `None` for unknown, revoked, or expired tokens
/// and inactive users. Use is audited at most once an hour per token.
pub async fn authenticate(
    core: &Core,
    presented: &str,
    client: &ClientInfo,
) -> CoreResult<Option<TokenAuth>> {
    if !core.config.api.enabled || !presented.starts_with(TOKEN_PREFIX) {
        return Ok(None);
    }
    let t = sqlx::query!(
        r#"SELECT t.id, t.name, t.user_id, t.scopes, t.expires_at, t.revoked_at, t.last_audited_at,
                  u.display_name, u.status AS "status: UserStatus"
             FROM api_tokens t JOIN users u ON u.id = t.user_id
            WHERE t.token_hash = $1"#,
        &sha256(presented.as_bytes())[..]
    )
    .fetch_optional(&core.db)
    .await?;
    let now = Utc::now();
    let Some(t) = t.filter(|t| t.revoked_at.is_none() && t.expires_at > now) else {
        return Ok(None);
    };
    if t.status != UserStatus::Active {
        return Ok(None);
    }
    let principal: Principal = crate::auth::principal::load(&core.db, t.user_id).await?;
    let mut ctx = Ctx::user(principal, &t.display_name, AuthMethod::ApiToken, client);
    ctx.actor.api_token_id = Some(t.id);
    let audit_due = t
        .last_audited_at
        .is_none_or(|at| now - at > Duration::hours(1));
    sqlx::query!(
        "UPDATE api_tokens SET last_used_at = now(),
                last_audited_at = CASE WHEN $2 THEN now() ELSE last_audited_at END WHERE id = $1",
        t.id,
        audit_due
    )
    .execute(&core.db)
    .await?;
    if audit_due {
        pdms_audit::record(
            &core.db,
            vec![token_event(
                &ctx,
                "api_token.used",
                t.id,
                &t.name,
                t.user_id,
            )],
        )
        .await?;
    }
    Ok(Some(TokenAuth {
        token_id: t.id,
        user_id: t.user_id,
        scopes: t.scopes,
        ctx,
    }))
}

/// One API request, for the request log.
#[derive(Clone, Debug)]
pub struct RequestRecord {
    pub token_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    pub client: ClientInfo,
}

pub async fn log_request(db: &PgPool, r: RequestRecord) -> CoreResult<()> {
    let path: String = r.path.chars().take(2000).collect();
    sqlx::query!(
        "INSERT INTO api_request_log (id, token_id, user_id, method, path, status, duration_ms, ip, user_agent, request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        Uuid::now_v7(),
        r.token_id,
        r.user_id,
        r.method,
        path,
        i32::from(r.status),
        i32::try_from(r.duration_ms).unwrap_or(i32::MAX),
        r.client.ip.map(ipnet::IpNet::from),
        r.client.user_agent,
        r.client.request_id,
    )
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct LoggedRequest {
    pub created_at: DateTime<Utc>,
    pub token_name: Option<String>,
    pub user_name: Option<String>,
    pub method: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i32,
    pub ip: Option<String>,
}

/// The most recent API requests (integration managers).
pub async fn recent_requests(core: &Core, ctx: &Ctx, limit: i64) -> CoreResult<Vec<LoggedRequest>> {
    ctx.require(Capability::ManageIntegrations)?;
    Ok(sqlx::query_as!(
        LoggedRequest,
        r#"SELECT l.created_at, t.name AS "token_name?", u.display_name AS "user_name?", l.method, l.path,
                  l.status, l.duration_ms, host(l.ip) AS ip
             FROM api_request_log l
             LEFT JOIN api_tokens t ON t.id = l.token_id
             LEFT JOIN users u ON u.id = l.user_id
            ORDER BY l.created_at DESC LIMIT $1"#,
        limit.clamp(1, 5000)
    )
    .fetch_all(&core.db)
    .await?)
}

/// Delete request-log entries older than `days`. Returns how many.
pub async fn purge_request_log(db: &PgPool, days: i64) -> CoreResult<u64> {
    let cutoff = Utc::now() - Duration::days(days.max(1));
    Ok(
        sqlx::query!("DELETE FROM api_request_log WHERE created_at < $1", cutoff)
            .execute(db)
            .await?
            .rows_affected(),
    )
}
