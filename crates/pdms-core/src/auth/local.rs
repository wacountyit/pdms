// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Break-glass local login: email + Argon2id password + TOTP, with lockout.

use chrono::{Duration, Utc};
use pdms_audit::{Actor, AuditEvent, AuthMethod, Category, Outcome};
use uuid::Uuid;

use crate::Core;
use crate::auth::{password, session, totp};
use crate::context::ClientInfo;
use crate::error::CoreError;
use crate::model::UserStatus;

#[derive(Debug)]
pub struct LoginSuccess {
    pub token: String,
    pub user_id: Uuid,
    pub display_name: String,
}

/// Failure reasons are recorded in the audit log but never shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFailure {
    Disabled,
    UnknownUser,
    NoLocalCredentials,
    BadPassword,
    BadTotp,
    TotpNotEnrolled,
    Locked,
    Inactive,
}

impl LoginFailure {
    fn as_str(self) -> &'static str {
        match self {
            LoginFailure::Disabled => "local_login_disabled",
            LoginFailure::UnknownUser => "unknown_user",
            LoginFailure::NoLocalCredentials => "no_local_credentials",
            LoginFailure::BadPassword => "bad_password",
            LoginFailure::BadTotp => "bad_totp",
            LoginFailure::TotpNotEnrolled => "totp_not_enrolled",
            LoginFailure::Locked => "locked",
            LoginFailure::Inactive => "inactive",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("sign-in failed")]
    Failed(LoginFailure),
    #[error(transparent)]
    Core(#[from] CoreError),
}

fn actor(client: &ClientInfo, user_id: Option<Uuid>, display: Option<String>) -> Actor {
    Actor {
        user_id,
        display,
        auth_method: AuthMethod::Local,
        ip: client.ip,
        user_agent: client.user_agent.clone(),
        api_token_id: None,
        request_id: client.request_id.clone(),
    }
}

pub async fn login(
    core: &Core,
    email: &str,
    password_input: &str,
    totp_code: &str,
    client: &ClientInfo,
) -> Result<LoginSuccess, LoginError> {
    let cfg = &core.config.auth.local;
    let email = email.trim();
    let failed = |user_id: Option<Uuid>, display: Option<String>, reason: LoginFailure| {
        AuditEvent::new(
            Category::Auth,
            "auth.login_failed",
            actor(client, user_id, display),
            "user",
        )
        .outcome(Outcome::Failure)
        .context(
            "attempted_identifier",
            email.chars().take(320).collect::<String>(),
        )
        .context("reason", reason.as_str())
    };

    if !cfg.enabled {
        pdms_audit::record(&core.db, vec![failed(None, None, LoginFailure::Disabled)])
            .await
            .map_err(CoreError::from)?;
        return Err(LoginError::Failed(LoginFailure::Disabled));
    }

    let row = sqlx::query!(
        r#"SELECT u.id, u.display_name, u.status AS "status: UserStatus",
                  c.password_hash AS "password_hash?", c.totp_secret_enc, c.totp_last_step,
                  c.failed_attempts AS "failed_attempts?", c.locked_until
             FROM users u
             LEFT JOIN local_credentials c ON c.user_id = u.id
            WHERE lower(u.email) = lower($1)"#,
        email
    )
    .fetch_optional(&core.db)
    .await
    .map_err(CoreError::from)?;

    // Always run one Argon2 verification so timing does not reveal whether the account exists.
    let password_ok = password::verify(
        row.as_ref().and_then(|r| r.password_hash.clone()),
        password_input.to_owned(),
    )
    .await;

    let Some(user) = row else {
        pdms_audit::record(
            &core.db,
            vec![failed(None, None, LoginFailure::UnknownUser)],
        )
        .await
        .map_err(CoreError::from)?;
        return Err(LoginError::Failed(LoginFailure::UnknownUser));
    };
    let who = (Some(user.id), Some(user.display_name.clone()));

    let reject = |reason: LoginFailure| failed(who.0, who.1.clone(), reason);

    if user.password_hash.is_none() {
        pdms_audit::record(&core.db, vec![reject(LoginFailure::NoLocalCredentials)])
            .await
            .map_err(CoreError::from)?;
        return Err(LoginError::Failed(LoginFailure::NoLocalCredentials));
    }
    let now = Utc::now();
    if user.locked_until.is_some_and(|t| t > now) {
        pdms_audit::record(&core.db, vec![reject(LoginFailure::Locked)])
            .await
            .map_err(CoreError::from)?;
        return Err(LoginError::Failed(LoginFailure::Locked));
    }

    let mut accepted_step = None;
    let mut failure = if !password_ok {
        Some(LoginFailure::BadPassword)
    } else if cfg.require_totp {
        match user.totp_secret_enc.as_deref() {
            None => Some(LoginFailure::TotpNotEnrolled),
            Some(enc) => {
                let secret = core
                    .keys
                    .decrypt_totp_secret(user.id, enc)
                    .map_err(CoreError::internal)?;
                let unix = u64::try_from(now.timestamp()).unwrap_or(0);
                accepted_step = totp::verify(&secret, totp_code, unix, user.totp_last_step);
                accepted_step.is_none().then_some(LoginFailure::BadTotp)
            }
        }
    } else {
        None
    };
    if failure.is_none() && user.status != UserStatus::Active {
        failure = Some(LoginFailure::Inactive);
    }

    let mut tx = core.db.begin().await.map_err(CoreError::from)?;
    if failure.is_none() {
        // Claim the TOTP step atomically so a code cannot be replayed concurrently.
        let r = sqlx::query!(
            "UPDATE local_credentials
                SET failed_attempts = 0, locked_until = NULL,
                    totp_last_step = coalesce($2, totp_last_step)
              WHERE user_id = $1
                AND ($2::bigint IS NULL OR totp_last_step IS NULL OR totp_last_step < $2)",
            user.id,
            accepted_step,
        )
        .execute(&mut *tx)
        .await
        .map_err(CoreError::from)?;
        if r.rows_affected() != 1 {
            failure = Some(LoginFailure::BadTotp);
        }
    }

    if let Some(reason) = failure {
        let counts_toward_lockout =
            matches!(reason, LoginFailure::BadPassword | LoginFailure::BadTotp);
        let mut events = vec![reject(reason)];
        if counts_toward_lockout {
            let attempts = sqlx::query_scalar!(
                "UPDATE local_credentials SET failed_attempts = failed_attempts + 1
                  WHERE user_id = $1 RETURNING failed_attempts",
                user.id,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(CoreError::from)?;
            if attempts >= cfg.max_failed_attempts {
                let until = now + Duration::minutes(cfg.lockout_minutes);
                sqlx::query!(
                    "UPDATE local_credentials SET failed_attempts = 0, locked_until = $2 WHERE user_id = $1",
                    user.id,
                    until,
                )
                .execute(&mut *tx)
                .await
                .map_err(CoreError::from)?;
                events.push(
                    AuditEvent::new(
                        Category::Auth,
                        "auth.account_locked",
                        actor(client, who.0, who.1.clone()),
                        "user",
                    )
                    .object(user.id, Some(&user.display_name))
                    .context("locked_until", until)
                    .context("failed_attempts", attempts),
                );
            }
        }
        pdms_audit::append(&mut tx, events)
            .await
            .map_err(CoreError::from)?;
        tx.commit().await.map_err(CoreError::from)?;
        return Err(LoginError::Failed(reason));
    }

    sqlx::query!(
        "UPDATE users SET last_login_at = now() WHERE id = $1",
        user.id
    )
    .execute(&mut *tx)
    .await
    .map_err(CoreError::from)?;
    let token = session::create(
        &mut tx,
        &core.config.auth,
        user.id,
        AuthMethod::Local,
        client,
    )
    .await?;
    let mut event = AuditEvent::new(
        Category::Auth,
        "auth.login",
        actor(client, who.0, who.1.clone()),
        "user",
    )
    .object(user.id, Some(&user.display_name))
    .context("mfa", cfg.require_totp);
    if !cfg.require_totp {
        event = event.context("warning", "TOTP not required by configuration");
    }
    pdms_audit::append(&mut tx, vec![event])
        .await
        .map_err(CoreError::from)?;
    tx.commit().await.map_err(CoreError::from)?;

    Ok(LoginSuccess {
        token,
        user_id: user.id,
        display_name: user.display_name,
    })
}
