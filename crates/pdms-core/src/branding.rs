// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! County branding: display name, accent color and seal image.
//!
//! The name and accent live in `app_settings` (`branding.org_name`,
//! `branding.accent`); the seal is a row in `branding_assets`. Nothing here is
//! required: with no settings the configured organization name and the default
//! accent apply.

pub mod color;
pub mod seal;

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Capability;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

pub const DEFAULT_ACCENT: &str = "#B45309";

const KEY_NAME: &str = "branding.org_name";
const KEY_ACCENT: &str = "branding.accent";
const MAX_NAME_CHARS: usize = 120;

pub use color::ThemeColors;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealInfo {
    pub sha256: String,
    pub content_type: String,
    pub width: i32,
    pub height: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branding {
    pub org_name: Option<String>,
    pub accent: Option<String>,
    pub seal: Option<SealInfo>,
}

impl Branding {
    pub fn accent_or_default(&self) -> &str {
        self.accent.as_deref().unwrap_or(DEFAULT_ACCENT)
    }

    /// Theme shades for the stored accent, or for the default when the stored
    /// value is missing or unusable.
    pub fn theme(&self) -> ThemeColors {
        let accent = color::parse_hex(self.accent_or_default())
            .or_else(|| color::parse_hex(DEFAULT_ACCENT))
            .unwrap_or(color::Rgb(0xB4, 0x53, 0x09));
        color::derive_theme(accent)
    }
}

fn event(ctx: &Ctx, category: Category, action: &str, object_type: &str) -> AuditEvent {
    AuditEvent::new(category, action, ctx.actor.clone(), object_type)
}

/// A setting stored as a JSON string; anything else counts as missing.
fn as_string(v: Option<Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

async fn setting(conn: &mut PgConnection, key: &str, lock: bool) -> CoreResult<Option<String>> {
    let v = if lock {
        sqlx::query_scalar!(
            "SELECT value FROM app_settings WHERE key = $1 FOR UPDATE",
            key
        )
        .fetch_optional(&mut *conn)
        .await?
    } else {
        sqlx::query_scalar!("SELECT value FROM app_settings WHERE key = $1", key)
            .fetch_optional(&mut *conn)
            .await?
    };
    Ok(as_string(v))
}

async fn write_setting(
    conn: &mut PgConnection,
    ctx: &Ctx,
    key: &str,
    value: Option<&str>,
) -> CoreResult<()> {
    match value {
        Some(v) => {
            sqlx::query!(
                "INSERT INTO app_settings (key, value, updated_by) VALUES ($1, $2, $3)
                 ON CONFLICT (key) DO UPDATE
                    SET value = EXCLUDED.value, updated_by = $3, updated_at = now()",
                key,
                Value::String(v.to_owned()),
                ctx.user_id()
            )
            .execute(&mut *conn)
            .await?;
        }
        None => {
            sqlx::query!("DELETE FROM app_settings WHERE key = $1", key)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

pub async fn load(db: &PgPool) -> CoreResult<Branding> {
    let mut conn = db.acquire().await?;
    let org_name = setting(&mut conn, KEY_NAME, false).await?;
    let accent = setting(&mut conn, KEY_ACCENT, false).await?;
    let seal = sqlx::query!(
        "SELECT sha256, content_type, width, height, updated_at
           FROM branding_assets WHERE name = 'seal'"
    )
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| SealInfo {
        sha256: r.sha256,
        content_type: r.content_type,
        width: r.width,
        height: r.height,
        updated_at: r.updated_at,
    });
    Ok(Branding {
        org_name,
        accent,
        seal,
    })
}

/// The name shown to people and written into exports: the branding setting if
/// set, else the configured organization name. Never fails.
pub async fn org_name(core: &Core) -> String {
    let stored = match core.db.acquire().await {
        Ok(mut conn) => match setting(&mut conn, KEY_NAME, false).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "branding org name unreadable; using configured name");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "branding org name unreadable; using configured name");
            None
        }
    };
    match stored {
        Some(s) if !s.trim().is_empty() => s,
        _ => core.config.org.name.clone(),
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// Sets the county name and accent. An empty value clears that setting.
pub async fn update(core: &Core, ctx: &Ctx, org_name: &str, accent: &str) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let name = non_empty(org_name);
    if name
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NAME_CHARS)
    {
        return Err(CoreError::validation(
            "The county name must be 120 characters or fewer.",
        ));
    }
    let accent = match non_empty(accent) {
        Some(a) => Some(
            color::validate_accent(&a)
                .map_err(|e| CoreError::validation(e.message(&a)))?
                .hex(),
        ),
        None => None,
    };

    let mut tx = core.db.begin().await?;
    let before_name = setting(&mut tx, KEY_NAME, true).await?;
    let before_accent = setting(&mut tx, KEY_ACCENT, true).await?;
    if before_name != name {
        write_setting(&mut tx, ctx, KEY_NAME, name.as_deref()).await?;
    }
    if before_accent != accent {
        write_setting(&mut tx, ctx, KEY_ACCENT, accent.as_deref()).await?;
    }
    let e = event(ctx, Category::Admin, "branding.updated", "settings")
        .change("org_name", before_name, name)
        .change("accent", before_accent, accent);
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Validates and stores the seal image.
pub async fn set_seal(core: &Core, ctx: &Ctx, bytes: &[u8]) -> CoreResult<SealInfo> {
    ctx.require(Capability::ManageSettings)?;
    let (content_type, width, height) =
        seal::sniff(bytes).map_err(|e| CoreError::validation(e.message()))?;
    let sha256 = hex::encode(Sha256::digest(bytes));
    let (w, h) = (width as i32, height as i32);

    let mut tx = core.db.begin().await?;
    let row = sqlx::query!(
        "INSERT INTO branding_assets (name, content_type, bytes, sha256, width, height, updated_by)
         VALUES ('seal', $1, $2, $3, $4, $5, $6)
         ON CONFLICT (name) DO UPDATE
            SET content_type = EXCLUDED.content_type, bytes = EXCLUDED.bytes,
                sha256 = EXCLUDED.sha256, width = EXCLUDED.width, height = EXCLUDED.height,
                updated_by = EXCLUDED.updated_by, updated_at = now()
         RETURNING updated_at",
        content_type,
        bytes,
        sha256,
        w,
        h,
        ctx.user_id()
    )
    .fetch_one(&mut *tx)
    .await?;
    let e = event(ctx, Category::Admin, "branding.seal_set", "settings")
        .context("sha256", sha256.as_str())
        .context("bytes", bytes.len() as i64)
        .context("width", w)
        .context("height", h);
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(SealInfo {
        sha256,
        content_type: content_type.to_owned(),
        width: w,
        height: h,
        updated_at: row.updated_at,
    })
}

pub async fn remove_seal(core: &Core, ctx: &Ctx) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let mut tx = core.db.begin().await?;
    let removed =
        sqlx::query_scalar!("DELETE FROM branding_assets WHERE name = 'seal' RETURNING sha256")
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(sha) = removed {
        let e =
            event(ctx, Category::Admin, "branding.seal_removed", "settings").context("sha256", sha);
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Clears the name, accent and seal.
pub async fn reset(core: &Core, ctx: &Ctx) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let mut tx = core.db.begin().await?;
    let before_name = setting(&mut tx, KEY_NAME, true).await?;
    let before_accent = setting(&mut tx, KEY_ACCENT, true).await?;
    write_setting(&mut tx, ctx, KEY_NAME, None).await?;
    write_setting(&mut tx, ctx, KEY_ACCENT, None).await?;
    let before_seal =
        sqlx::query_scalar!("DELETE FROM branding_assets WHERE name = 'seal' RETURNING sha256")
            .fetch_optional(&mut *tx)
            .await?;
    let e = event(ctx, Category::Admin, "branding.reset", "settings")
        .change("org_name", before_name, None::<String>)
        .change("accent", before_accent, None::<String>)
        .change("seal_sha256", before_seal, None::<String>);
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

/// `(content_type, bytes)` of the stored seal.
pub async fn seal_bytes(db: &PgPool) -> CoreResult<Option<(String, Vec<u8>)>> {
    Ok(
        sqlx::query!("SELECT content_type, bytes FROM branding_assets WHERE name = 'seal'")
            .fetch_optional(db)
            .await?
            .map(|r| (r.content_type, r.bytes)),
    )
}
