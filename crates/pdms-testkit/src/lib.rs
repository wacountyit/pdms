// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Disposable databases for integration tests.
//!
//! One Postgres server per test process: a testcontainers `postgres:15-alpine`
//! by default, or the superuser URL in `PDMS_TEST_DATABASE_URL` (an existing
//! server such as the dev database, or CI service containers). A migrated
//! template database is built once; every test gets a fresh copy, so tests are
//! isolated and fast.
//!
//! Cleanup: the container lives in a `static`, and statics are never dropped,
//! so testcontainers cannot remove it at exit. Instead each test process
//! starts a small detached watcher that removes the container once the process
//! is gone (normal exit, panic, or kill). On a shared server, databases are
//! named after the test process, and those of finished processes are dropped
//! the next time tests start.
//!
//! Test-only crate: panics on setup failure are intentional.
#![allow(clippy::expect_used, clippy::panic)]

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Executor, PgPool};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::{Mutex, OnceCell};

/// Default passwords for the roles the test kit creates. On a shared server
/// whose roles already exist (for example the dev database), set
/// `PDMS_TEST_OWNER_PASSWORD` and `PDMS_TEST_APP_PASSWORD` to theirs.
pub const OWNER_PASSWORD: &str = "test-owner";
pub const APP_PASSWORD: &str = "test-app";

fn owner_password() -> String {
    std::env::var("PDMS_TEST_OWNER_PASSWORD").unwrap_or_else(|_| OWNER_PASSWORD.to_owned())
}

fn app_password() -> String {
    std::env::var("PDMS_TEST_APP_PASSWORD").unwrap_or_else(|_| APP_PASSWORD.to_owned())
}

/// Whether a process is still running.
fn alive(pid: u32) -> bool {
    let proc_dir = std::path::Path::new("/proc");
    if proc_dir.is_dir() {
        return proc_dir.join(pid.to_string()).exists();
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Remove `container_id` once this process has exited. The watcher is a
/// detached shell (it outlives the test process); if it cannot be started,
/// the container is left behind as before, labeled for manual cleanup.
fn reap_on_exit(container_id: &str) {
    let script =
        r#"while kill -0 "$1" 2>/dev/null; do sleep 2; done; docker rm -f -v "$2" >/dev/null 2>&1"#;
    let _ = std::process::Command::new("sh")
        .args(["-c", script, "pdms-testkit-reaper"])
        .arg(std::process::id().to_string())
        .arg(container_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Database names carry the owning process ID: `pdms_t_<pid>_<uuid>`.
fn db_prefix() -> String {
    format!("{}_", std::process::id())
}

struct Server {
    superuser: PgConnectOptions,
    template: String,
    _container: Option<ContainerAsync<GenericImage>>,
}

static SERVER: OnceCell<Server> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

/// A freshly created, migrated database with pools for each role.
pub struct TestDb {
    pub name: String,
    /// Connected as `pdms_app`: what the application uses. Use for most tests.
    pub app: PgPool,
    /// Connected as `pdms_owner`: schema owner (runs migrations).
    pub owner: PgPool,
    /// Connected as a superuser: for simulating out-of-band tampering.
    pub superuser: PgPool,
    pub app_options: PgConnectOptions,
}

async fn server() -> &'static Server {
    SERVER
        .get_or_init(|| async {
            let (superuser, container) = match std::env::var("PDMS_TEST_DATABASE_URL") {
                Ok(url) => (
                    url.parse::<PgConnectOptions>()
                        .expect("PDMS_TEST_DATABASE_URL"),
                    None,
                ),
                Err(_) => {
                    let c = GenericImage::new("postgres", "15-alpine")
                        .with_exposed_port(5432.tcp())
                        .with_wait_for(WaitFor::message_on_stderr(
                            "database system is ready to accept connections",
                        ))
                        .with_env_var("POSTGRES_PASSWORD", "postgres")
                        .with_label("pdms.testkit", "postgres")
                        .with_label("pdms.testkit.pid", std::process::id().to_string())
                        .start()
                        .await
                        .expect("start postgres container (is Docker running?)");
                    reap_on_exit(c.id());
                    let port = c.get_host_port_ipv4(5432).await.expect("container port");
                    let host = c.get_host().await.expect("container host").to_string();
                    let opts = PgConnectOptions::new()
                        .host(&host)
                        .port(port)
                        .username("postgres")
                        .password("postgres")
                        .database("postgres");
                    (opts, Some(c))
                }
            };
            let template = format!("pdms_tpl_{}{}", db_prefix(), uuid::Uuid::new_v4().simple());
            bootstrap(&superuser, &template).await;
            Server {
                superuser,
                template,
                _container: container,
            }
        })
        .await
}

async fn bootstrap(superuser: &PgConnectOptions, template: &str) {
    // The image logs "ready" once for its init server (socket only) and again
    // for the real one, so retry until TCP connections succeed.
    let mut attempts = 0;
    let mut conn = loop {
        match superuser.connect().await {
            Ok(c) => break c,
            Err(e) if attempts < 60 => {
                attempts += 1;
                let _ = e;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(e) => panic!("connect as superuser: {e}"),
        }
    };
    drop_stale_databases(&mut conn).await;
    let (owner_pw, app_pw) = (owner_password(), app_password());
    // Mirrors deploy/postgres/00_roles.sql; idempotent for shared servers.
    conn.execute(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pdms_owner') THEN
             CREATE ROLE pdms_owner LOGIN PASSWORD '{owner_pw}';
           END IF;
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pdms_app') THEN
             CREATE ROLE pdms_app LOGIN PASSWORD '{app_pw}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT;
           END IF;
         END $$"
    )))
    .await
    .expect("create roles");
    conn.execute(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {template} OWNER pdms_owner"
    )))
    .await
    .expect("create template database");

    let owner_opts = superuser
        .clone()
        .username("pdms_owner")
        .password(&owner_pw)
        .database(template);
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(owner_opts)
        .await
        .expect("connect template as owner");
    sqlx::migrate!("../../migrations")
        .run(&owner)
        .await
        .expect("run migrations on template");
    owner.close().await;
}

/// Drop test databases left by test processes that have exited (only
/// matters on a shared server; a fresh container has none).
async fn drop_stale_databases(conn: &mut sqlx::PgConnection) {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database WHERE datname LIKE 'pdms\\_t\\_%' OR datname LIKE 'pdms\\_tpl\\_%'",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap_or_default();
    for name in names {
        let rest = name
            .trim_start_matches("pdms_tpl_")
            .trim_start_matches("pdms_t_");
        // Names without a process ID predate this scheme: leftovers, drop them.
        let owner = rest
            .split_once('_')
            .and_then(|(pid, _)| pid.parse::<u32>().ok());
        if owner.is_some_and(alive) {
            continue;
        }
        let _ = conn
            .execute(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
            )))
            .await;
    }
}

/// Create a fresh migrated database for one test.
pub async fn test_db() -> TestDb {
    let server = server().await;
    let name = format!("pdms_t_{}{}", db_prefix(), uuid::Uuid::new_v4().simple());
    {
        let _guard = CREATE_LOCK.lock().await;
        let mut conn = server
            .superuser
            .connect()
            .await
            .expect("connect as superuser");
        conn.execute(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {name} TEMPLATE {} OWNER pdms_owner",
            server.template
        )))
        .await
        .expect("create test database");
    }
    let (owner_pw, app_pw) = (owner_password(), app_password());
    let pool = |user: &str, password: &str| {
        let opts = server
            .superuser
            .clone()
            .username(user)
            .password(password)
            .database(&name);
        async move {
            PgPoolOptions::new()
                .max_connections(8)
                .connect_with(opts)
                .await
                .expect("connect test database")
        }
    };
    let app_options = server
        .superuser
        .clone()
        .username("pdms_app")
        .password(&app_pw)
        .database(&name);
    let su = server.superuser.clone().database(&name);
    TestDb {
        app: pool("pdms_app", &app_pw).await,
        owner: pool("pdms_owner", &owner_pw).await,
        superuser: PgPoolOptions::new()
            .max_connections(2)
            .connect_with(su)
            .await
            .expect("connect test database as superuser"),
        app_options,
        name,
    }
}
