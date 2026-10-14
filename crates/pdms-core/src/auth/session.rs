// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Server-side sessions. The cookie carries a random token; only its SHA-256 is stored.

use chrono::{Duration, Utc};
use ipnet::IpNet;
use pdms_audit::AuthMethod;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::config::AuthConfig;
use crate::context::ClientInfo;
use crate::crypto::{new_token, sha256};
use crate::error::CoreResult;

/// How often `last_seen_at` is refreshed, to avoid a write per request.
const TOUCH_INTERVAL_SECS: i64 = 60;

#[derive(Clone, Debug)]
pub struct SessionUser {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub display_name: String,
    pub email: String,
    pub auth_method: AuthMethod,
}

/// Create a session and return the bearer token for the cookie.
pub async fn create(
    conn: &mut PgConnection,
    cfg: &AuthConfig,
    user_id: Uuid,
    method: AuthMethod,
    client: &ClientInfo,
) -> CoreResult<String> {
    let token = new_token();
    let expires_at = Utc::now() + Duration::hours(cfg.session_absolute_hours);
    sqlx::query!(
        r#"INSERT INTO sessions (id, token_hash, user_id, auth_method, ip, user_agent, expires_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        Uuid::now_v7(),
        &sha256(token.as_bytes())[..],
        user_id,
        method as AuthMethod,
        client.ip.map(IpNet::from),
        client.user_agent.as_deref(),
        expires_at,
    )
    .execute(conn)
    .await?;
    Ok(token)
}

/// Resolve a token to an active session for an active user.
pub async fn lookup(db: &PgPool, cfg: &AuthConfig, token: &str) -> CoreResult<Option<SessionUser>> {
    let row = sqlx::query!(
        r#"SELECT s.id, s.user_id, s.auth_method AS "auth_method: AuthMethod",
                  (s.last_seen_at < now() - make_interval(secs => $3)) AS "stale!",
                  u.display_name, u.email
             FROM sessions s
             JOIN users u ON u.id = s.user_id
            WHERE s.token_hash = $1
              AND s.revoked_at IS NULL
              AND s.expires_at > now()
              AND s.last_seen_at > now() - make_interval(mins => $2)
              AND u.status = 'active'"#,
        &sha256(token.as_bytes())[..],
        i32::try_from(cfg.session_idle_minutes).unwrap_or(i32::MAX),
        TOUCH_INTERVAL_SECS as f64,
    )
    .fetch_optional(db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    if r.stale {
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = now() WHERE id = $1",
            r.id
        )
        .execute(db)
        .await?;
    }
    Ok(Some(SessionUser {
        session_id: r.id,
        user_id: r.user_id,
        display_name: r.display_name,
        email: r.email,
        auth_method: r.auth_method,
    }))
}

pub async fn revoke(conn: &mut PgConnection, session_id: Uuid) -> CoreResult<()> {
    sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        session_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Revoke every session for a user (deactivation, credential reset). Returns how many.
pub async fn revoke_all_for_user(conn: &mut PgConnection, user_id: Uuid) -> CoreResult<u64> {
    let r = sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user_id
    )
    .execute(conn)
    .await?;
    Ok(r.rows_affected())
}

/// Delete sessions that ended more than `keep_days` ago.
pub async fn purge_expired(db: &PgPool, keep_days: i32) -> CoreResult<u64> {
    let r = sqlx::query!(
        "DELETE FROM sessions
          WHERE coalesce(revoked_at, expires_at) < now() - make_interval(days => $1)",
        keep_days
    )
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}
