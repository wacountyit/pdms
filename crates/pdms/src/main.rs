// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! `pdms`: server, worker, and administration commands.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use pdms_core::auth::oidc::OidcProvider;
use pdms_core::auth::totp;
use pdms_core::config::LogFormat;
use pdms_core::model::UserStatus;
use pdms_core::{Config, Core, Ctx, directory};
use tokio::sync::watch;

#[derive(Parser)]
#[command(
    name = "pdms",
    version,
    about = "Policy and Document Management System"
)]
struct Cli {
    /// Configuration file (default: $PDMS_CONFIG or config/pdms.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server.
    Serve {
        /// Also run the background worker in this process (small installs).
        #[arg(long)]
        with_worker: bool,
    },
    /// Run the background job worker.
    Worker,
    /// Apply database migrations (requires database.owner_url).
    Migrate,
    /// Create or reset a break-glass local administrator.
    CreateAdmin {
        #[arg(long)]
        email: String,
        #[arg(long)]
        name: String,
        /// Plain username to sign in with, such as `sysadmin` (optional).
        #[arg(long)]
        username: Option<String>,
        /// Read the password from this environment variable instead of prompting.
        #[arg(long, value_name = "VAR")]
        password_env: Option<String>,
    },
    /// Set the sign-in username of a local (break-glass) account.
    SetUsername {
        #[arg(long)]
        email: String,
        #[arg(long)]
        username: String,
    },
    /// Verify the audit log hash chain. Exit code 2 if it is broken.
    VerifyAudit {
        #[arg(long)]
        json: bool,
    },
    /// Check that every stored file matches the database (present, right size,
    /// right SHA-256), export packages are intact, and the audit chain is
    /// unbroken. Run after every restore. Exit code 2 if anything is wrong.
    VerifyIntegrity {
        /// Check presence and size only; do not re-read file contents.
        #[arg(long)]
        skip_files: bool,
        #[arg(long)]
        json: bool,
    },
    /// Load the demo dataset (evaluation only).
    SeedDemo,
    /// Run the reminder planner now (statuses, tasks, notifications). The worker
    /// also does this every 15 minutes; emails are sent by the worker.
    RunScheduler,
    /// Print a new random master secret for PDMS_SECRET_KEY.
    GenSecret,
    /// Create the certificate for the Microsoft Graph app registration
    /// ("PADM Service"). Writes certificate + private key to OUT (mode 0600)
    /// and the public certificate to OUT with the extension .crt, which you
    /// upload in Entra ID. Needs no configuration.
    GenGraphCert {
        #[arg(long)]
        out: PathBuf,
        /// Validity in days (30 to 1095).
        #[arg(long, default_value_t = 730)]
        days: u32,
        #[arg(long, default_value = "PADM Service")]
        name: String,
    },
    /// Validate configuration and print it with secrets redacted.
    CheckConfig,
    /// Exit 0 if the local server answers /healthz (for container health checks).
    Healthcheck {
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
}

fn init_logging(config: &Config) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log.filter));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match config.log.format {
        LogFormat::Json => builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .init(),
        LogFormat::Pretty => builder.init(),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::GenSecret => {
            println!("{}", pdms_core::crypto::generate_master_secret());
            return ExitCode::SUCCESS;
        }
        Command::Healthcheck { addr } => return healthcheck(addr).await,
        Command::GenGraphCert {
            ref out,
            days,
            ref name,
        } => return gen_graph_cert(out, days, name),
        _ => {}
    }
    let config = match Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    init_logging(&config);
    match run(cli.command, config).await {
        Ok(code) => code,
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "command failed");
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command, config: Config) -> anyhow::Result<ExitCode> {
    match command {
        Command::GenSecret | Command::Healthcheck { .. } | Command::GenGraphCert { .. } => {}
        Command::CheckConfig => {
            config.master_key().context("secret_key")?;
            println!("{config:#?}");
            println!("\nConfiguration is valid.");
        }
        Command::Migrate => {
            let applied = pdms_core::db::migrate(&config).await?;
            if applied.is_empty() {
                println!("Database schema is up to date.");
            } else {
                for m in applied {
                    println!("Applied migration {m}");
                }
            }
        }
        Command::Serve { with_worker } => serve(config, with_worker).await?,
        Command::Worker => {
            let core = connect(config).await?;
            let (tx, rx) = watch::channel(false);
            tokio::spawn(shutdown_signal(tx));
            pdms_core::maintenance::worker(&core).run(rx).await;
        }
        Command::CreateAdmin {
            email,
            name,
            username,
            password_env,
        } => {
            let core = connect(config).await?;
            create_admin(
                &core,
                &email,
                &name,
                username.as_deref(),
                password_env.as_deref(),
            )
            .await?;
        }
        Command::SetUsername { email, username } => {
            let core = connect(config).await?;
            let ctx = Ctx::system("cli:set-username");
            let user = directory::list_users(&core.db, &email, 10)
                .await?
                .into_iter()
                .find(|u| u.email.eq_ignore_ascii_case(&email))
                .ok_or_else(|| anyhow::anyhow!("no user with email {email}"))?;
            directory::set_username(&core.db, &ctx, user.id, Some(&username)).await?;
            println!("Username for {email} set to {username}.");
        }
        Command::VerifyAudit { json } => {
            let core = connect(config).await?;
            let report =
                pdms_core::audit_log::verify(&core.db, &Ctx::system("cli:verify-audit")).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "intact": report.is_intact(),
                        "entries_checked": report.entries_checked,
                        "last_seq": report.last.seq,
                        "last_hash": report.last.entry_hash.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        "breaks": report.breaks.iter().map(ToString::to_string).collect::<Vec<_>>(),
                        "truncated": report.truncated,
                    })
                );
            } else if report.is_intact() {
                println!(
                    "Audit chain intact: {} entries verified (last seq {}).",
                    report.entries_checked, report.last.seq
                );
            } else {
                println!(
                    "AUDIT CHAIN BROKEN: {} problem(s) in {} entries:",
                    report.breaks.len(),
                    report.entries_checked
                );
                for b in &report.breaks {
                    println!("  {b}");
                }
            }
            if !report.is_intact() {
                return Ok(ExitCode::from(2));
            }
        }
        Command::VerifyIntegrity { skip_files, json } => {
            let core = connect(config).await?;
            let r = pdms_core::integrity::verify(&core, !skip_files).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!(
                    "Files: {} checked ({} bytes{}). Export packages: {}. Document versions: {}. Audit entries: {} (last seq {}).",
                    r.files_checked,
                    r.bytes_checked,
                    if r.contents_skipped {
                        ", presence and size only"
                    } else {
                        ", contents re-hashed"
                    },
                    r.packages_checked,
                    r.versions_checked,
                    r.audit_entries_checked,
                    r.audit_last_seq
                );
                if r.is_ok() {
                    println!("Integrity verified: no problems found.");
                } else {
                    println!("INTEGRITY PROBLEMS: {}", r.problems.len());
                    for p in &r.problems {
                        println!("  [{}] {}: {}", p.kind, p.subject, p.detail);
                    }
                }
            }
            if !r.is_ok() {
                return Ok(ExitCode::from(2));
            }
        }
        Command::RunScheduler => {
            let core = connect(config).await?;
            let r = pdms_core::notifications::plan(&core, chrono::Utc::now()).await?;
            println!(
                "Status changes: {}, tasks created: {}, tasks now overdue: {}, notifications: {}, deliveries: {}",
                r.status_changes, r.tasks_created, r.tasks_overdue, r.notifications, r.deliveries
            );
        }
        Command::SeedDemo => {
            let core = connect(config).await?;
            let summary = pdms_core::demo::seed(&core).await?;
            if summary.created {
                println!(
                    "Demo data created. Users (password: {}):",
                    pdms_core::demo::DEMO_PASSWORD
                );
            } else {
                println!(
                    "Demo data already present. Users (password: {}):",
                    pdms_core::demo::DEMO_PASSWORD
                );
            }
            for (email, note) in summary.users {
                println!("  {email:<22} {note}");
            }
            if core.config.auth.local.require_totp {
                println!(
                    "\nNote: auth.local.require_totp is on, so demo users cannot sign in with a password alone."
                );
                println!("For a demo, set PDMS_AUTH__LOCAL__REQUIRE_TOTP=false.");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn healthcheck(addr: SocketAddr) -> ExitCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut s = tokio::net::TcpStream::connect(addr).await?;
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        Ok::<bool, std::io::Error>(buf.starts_with(b"HTTP/1.1 200"))
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), attempt).await {
        Ok(Ok(true)) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

async fn connect(config: Config) -> anyhow::Result<Core> {
    let core = Core::connect(config).await?;
    let pending = pdms_core::db::pending_migrations(&core.db).await?;
    if !pending.is_empty() {
        bail!(
            "database schema is out of date (pending migrations {pending:?}); run `pdms migrate` first"
        );
    }
    Ok(core)
}

async fn serve(config: Config, with_worker: bool) -> anyhow::Result<()> {
    let metrics = if config.server.metrics_enabled {
        Some(
            metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .context("installing metrics recorder")?,
        )
    } else {
        None
    };
    let core = connect(config).await?;
    let cfg = core.config.clone();
    if cfg.org.source_url.trim().is_empty() {
        tracing::warn!(
            "org.source_url is not set: the AGPL-3.0 requires offering users the source code; \
             /source tells them to contact you until it is set"
        );
    }
    if !cfg.server.secure_cookies {
        tracing::warn!(
            "server.secure_cookies is off: only acceptable for local development over HTTP"
        );
    }
    if cfg.auth.local.enabled && !cfg.auth.local.require_totp {
        tracing::warn!(
            "auth.local.require_totp is off: local sign-in is password-only (demo use only)"
        );
    }

    let oidc = if cfg.auth.oidc.enabled {
        match OidcProvider::discover(&cfg.auth.oidc, &cfg.server.public_url).await {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!(error = %e, "OIDC discovery failed; single sign-on is unavailable until restart");
                None
            }
        }
    } else {
        None
    };

    let (tx, rx) = watch::channel(false);
    let worker =
        with_worker.then(|| tokio::spawn(pdms_core::maintenance::worker(&core).run(rx.clone())));

    let state = pdms_web::AppState::new(core, oidc, metrics);
    state.reload_branding().await;
    let app = pdms_web::router(state);
    let listener = tokio::net::TcpListener::bind(cfg.server.bind)
        .await
        .with_context(|| format!("binding {}", cfg.server.bind))?;
    tracing::info!(addr = %cfg.server.bind, org = %cfg.org.name, "listening");

    let mut shutdown = rx;
    tokio::spawn(shutdown_signal(tx));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.wait_for(|stop| *stop).await;
    })
    .await?;
    if let Some(w) = worker {
        let _ = w.await;
    }
    tracing::info!("server stopped");
    Ok(())
}

async fn shutdown_signal(tx: watch::Sender<bool>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("shutdown requested");
    let _ = tx.send(true);
}

fn gen_graph_cert(out: &std::path::Path, days: u32, name: &str) -> ExitCode {
    match write_graph_cert(out, days, name) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn write_graph_cert(out: &std::path::Path, days: u32, name: &str) -> anyhow::Result<()> {
    use std::io::Write;
    if !(30..=1095).contains(&days) {
        bail!("--days must be 30–1095");
    }
    let mut public_name = out.as_os_str().to_owned();
    public_name.push(".crt");
    let public = PathBuf::from(public_name);
    for path in [out, public.as_path()] {
        if path.symlink_metadata().is_ok() {
            bail!(
                "{} already exists; refusing to overwrite it",
                path.display()
            );
        }
    }
    let g = pdms_core::graph::cert::generate(name, days)?;
    let create = |path: &std::path::Path, mode: u32| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        opts.open(path)
            .with_context(|| format!("creating {} (it must not exist yet)", path.display()))
    };
    create(out, 0o600)?.write_all(g.pem.as_bytes())?;
    create(&public, 0o644)?.write_all(g.certificate_pem.as_bytes())?;
    println!(
        "Certificate and private key: {} (secret: give it to PADM as PDMS_GRAPH__CERTIFICATE_FILE)",
        out.display()
    );
    println!(
        "Public certificate for Entra ID (App registrations > PADM Service > Certificates & secrets > Upload): {}",
        public.display()
    );
    println!("SHA-256 thumbprint (x5t#S256): {}", g.thumbprint);
    println!("Expires: {}", g.not_after.format("%Y-%m-%d"));
    Ok(())
}

fn read_password(env_var: Option<&str>) -> anyhow::Result<String> {
    if let Some(var) = env_var {
        return std::env::var(var)
            .with_context(|| format!("environment variable {var} is not set"));
    }
    let first = rpassword::prompt_password("New password (min 12 characters): ")?;
    let second = rpassword::prompt_password("Repeat password: ")?;
    if first != second {
        bail!("passwords do not match");
    }
    Ok(first)
}

async fn create_admin(
    core: &Core,
    email: &str,
    name: &str,
    username: Option<&str>,
    password_env: Option<&str>,
) -> anyhow::Result<()> {
    let ctx = Ctx::system("cli:create-admin");
    let password = read_password(password_env)?;
    pdms_core::auth::password::validate(&password)?;

    let existing = directory::list_users(&core.db, email, 10)
        .await?
        .into_iter()
        .find(|u| u.email.eq_ignore_ascii_case(email));

    // Validate the username before anything changes (reactivating or creating
    // the user), so a bad or taken one cannot leave the account half set up.
    if let Some(username) = username {
        directory::check_username(&core.db, username, existing.as_ref().map(|u| u.id)).await?;
    }

    let user_id = match existing {
        Some(u) => {
            if u.status != UserStatus::Active {
                directory::set_user_status(&core.db, &ctx, u.id, UserStatus::Active).await?;
            }
            println!(
                "Resetting credentials for existing user {} <{}>.",
                u.display_name, u.email
            );
            u.id
        }
        None => directory::create_user(&core.db, &ctx, name, email, None).await?,
    };

    let secret = totp::generate_secret();
    directory::set_local_credentials(
        &core.db,
        &core.keys,
        &ctx,
        user_id,
        &password,
        Some(&secret),
    )
    .await?;

    if let Some(username) = username {
        directory::set_username(&core.db, &ctx, user_id, Some(username)).await?;
    }

    let admin_role = directory::role_id_by_key(&core.db, pdms_authz::role_keys::ADMIN).await?;
    let detail = directory::get_user(&core.db, user_id).await?;
    if !detail
        .roles
        .iter()
        .any(|r| r.role_id == admin_role && r.department_id.is_none())
    {
        directory::assign_role(&core.db, &ctx, user_id, admin_role, None).await?;
    }

    let uri = totp::provisioning_uri(&secret, &core.config.org.name, email)?;
    println!("\nAdministrator ready: {email}");
    if let Some(username) = username {
        println!("Sign in with username {username} (or the email address).");
    }
    println!(
        "Add this account to an authenticator app, then sign in with username or email + password + 6-digit code."
    );
    println!("  Secret (manual entry): {}", totp::secret_base32(&secret));
    println!("  otpauth URI:           {uri}");
    println!(
        "\nStore the secret securely; it is not shown again. Run this command again to reset."
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::write_graph_cert;

    fn scratch() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("padm-cert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&d).unwrap();
        d
    }

    #[test]
    fn an_existing_public_certificate_file_is_left_untouched() {
        let d = scratch();
        let out = d.join("graph.pem");
        std::fs::write(d.join("graph.pem.crt"), "keep me").unwrap();
        assert!(write_graph_cert(&out, 30, "PADM test").is_err());
        assert_eq!(
            std::fs::read_to_string(d.join("graph.pem.crt")).unwrap(),
            "keep me"
        );
        assert!(!out.exists(), "no key is written when the .crt exists");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn out_ending_in_crt_keeps_the_key_and_writes_a_separate_public_file() {
        let d = scratch();
        let out = d.join("x.crt");
        write_graph_cert(&out, 30, "PADM test").unwrap();
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .contains("PRIVATE KEY")
        );
        let public = std::fs::read_to_string(d.join("x.crt.crt")).unwrap();
        assert!(!public.contains("PRIVATE KEY"));
        std::fs::remove_dir_all(d).unwrap();
    }
}
