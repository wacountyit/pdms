// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::config::Config;
use crate::error::{CoreError, CoreResult};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Connect as the application role.
pub async fn connect(config: &Config) -> CoreResult<PgPool> {
    if config.database.url.is_empty() {
        return Err(CoreError::validation(
            "database.url is not set (PDMS_DATABASE__URL or PDMS_DATABASE__URL_FILE)",
        ));
    }
    let opts: PgConnectOptions = config
        .database
        .url
        .expose()
        .parse()
        .map_err(|e| CoreError::validation(format!("database.url: {e}")))?;
    Ok(PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts.application_name("pdms"))
        .await?)
}

/// Apply pending migrations as the schema owner.
pub async fn migrate(config: &Config) -> CoreResult<Vec<String>> {
    if config.database.owner_url.is_empty() {
        return Err(CoreError::validation(
            "database.owner_url is required for migrations (PDMS_DATABASE__OWNER_URL[_FILE])",
        ));
    }
    let opts: PgConnectOptions = config
        .database
        .owner_url
        .expose()
        .parse()
        .map_err(|e| CoreError::validation(format!("database.owner_url: {e}")))?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(opts.application_name("pdms-migrate"))
        .await?;
    let before: Vec<i64> = applied_versions(&pool).await;
    MIGRATOR
        .run(&pool)
        .await
        .map_err(|e| CoreError::internal(format!("migration failed: {e}")))?;
    let applied = MIGRATOR
        .iter()
        .filter(|m| !before.contains(&m.version))
        .map(|m| format!("{} {}", m.version, m.description))
        .collect();
    pool.close().await;
    Ok(applied)
}

async fn applied_versions(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
}

pub async fn ping(pool: &PgPool) -> CoreResult<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

/// Migrations embedded in this binary that the database has not applied.
pub async fn pending_migrations(pool: &PgPool) -> CoreResult<Vec<i64>> {
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(pool)
            .await?;
    Ok(MIGRATOR
        .iter()
        .map(|m| m.version)
        .filter(|v| !applied.contains(v))
        .collect())
}
