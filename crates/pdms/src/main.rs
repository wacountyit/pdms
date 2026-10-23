// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! `pdms`: server, worker, and administration commands.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
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
enum SettingsCommand {
    /// Every setting, its effective value (secrets only as set or not set),
    /// and where it comes from: the configuration file, Administration (the
    /// database), or the default.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Set one setting, with the same checks and audit as the admin pages
    /// (for example `settings set email.smtp_port 587`).
    Set { key: String, value: String },
    /// Set a password or secret, read from standard input.
    SetSecret { key: String },
    /// Run an admin page's test: microsoft-365, sign-in, sharepoint, entra,
    /// email (with --to), ocr or virus-scanning.
    Test {
        area: String,
        /// The address for the email test.
        #[arg(long)]
        to: Option<String>,
    },
    /// Copy the settings an earlier installer wrote into config/pdms.toml
    /// (and its secret files) into the database. The installer runs this
    /// when it moves settings into Administration.
    Import,
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
        /// Also download every file stored in SharePoint and re-hash it with SHA-256 (slow).
        #[arg(long)]
        rehash_sharepoint: bool,
        #[arg(long)]
        json: bool,
    },
    /// Load the demo dataset (evaluation only).
    SeedDemo,
    /// Run the reminder planner now (statuses, tasks, notifications). The worker
    /// also does this every 15 minutes; emails are sent by the worker.
    RunScheduler,
    /// Read people and departments from Entra ID now (the worker does this
    /// nightly). Refused unless [entra_sync] enabled = true. --full ignores
    /// the saved cursor.
    SyncDirectory {
        #[arg(long)]
        full: bool,
    },
    /// Print a new random master secret for PDMS_SECRET_KEY.
    GenSecret,
    /// Create the certificate for the Microsoft Graph app registration
    /// ("PDMS Service"). Writes certificate + private key to OUT (mode 0600)
    /// and the public certificate to OUT with ".crt" appended (graph.pem
    /// gives graph.pem.crt), which you upload in Entra ID. Needs no
    /// configuration.
    GenGraphCert {
        #[arg(long)]
        out: PathBuf,
        /// Validity in days (30 to 1095).
        #[arg(long, default_value_t = 730)]
        days: u32,
        #[arg(long, default_value = "PDMS Service")]
        name: String,
    },
    /// Validate configuration and print it with secrets redacted.
    CheckConfig,
    /// The settings managed in Administration (S1).
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
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
    let config_path = pdms_core::config::file_path(cli.config.as_deref());
    match run(cli.command, config, config_path).await {
        Ok(code) => code,
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "command failed");
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(
    command: Command,
    config: Config,
    config_path: Option<PathBuf>,
) -> anyhow::Result<ExitCode> {
    match command {
        Command::GenSecret | Command::Healthcheck { .. } | Command::GenGraphCert { .. } => {}
        Command::Settings { command } => {
            let core = connect(config).await?;
            return settings_command(&core, command, config_path.as_deref()).await;
        }
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
            let settings = pdms_core::settings::watch(core.clone());
            pdms_core::maintenance::run_worker(core, rx, settings).await;
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
        Command::VerifyIntegrity {
            skip_files,
            rehash_sharepoint,
            json,
        } => {
            let core = connect(config).await?;
            let r = pdms_core::integrity::verify_with(
                &core,
                pdms_core::integrity::VerifyOptions {
                    rehash: !skip_files,
                    download_sharepoint: rehash_sharepoint,
                },
            )
            .await?;
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
                if r.sharepoint_files_checked > 0 {
                    println!(
                        "SharePoint files: {} checked ({}).",
                        r.sharepoint_files_checked,
                        if rehash_sharepoint {
                            "downloaded and re-hashed"
                        } else if skip_files {
                            "presence and size only"
                        } else {
                            "size and QuickXorHash"
                        }
                    );
                }
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
        Command::SyncDirectory { full } => {
            use pdms_core::directory::entra_sync;
            let core = connect(config).await?;
            let ctx = Ctx::system("cli:sync-directory");
            let r = if full {
                entra_sync::run_full(&core, &ctx).await?
            } else {
                entra_sync::run(&core, &ctx).await?
            };
            println!(
                "Created {}, updated {}, departed {}; {} without a mapped department.",
                r.created, r.updated, r.departed, r.unmapped
            );
            for e in &r.errors {
                println!("  needs attention: {e}");
            }
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
            if core.config().auth.local.require_totp {
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

async fn settings_command(
    core: &Core,
    command: SettingsCommand,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<ExitCode> {
    use pdms_core::settings::{self, Change, Kind, NewValue};
    let ctx = Ctx::system("settings-cli");
    let set = |key: &str, value: serde_json::Value| {
        vec![Change {
            key: key.to_owned(),
            value: NewValue::Set(value),
        }]
    };
    match command {
        SettingsCommand::Show { json } => settings_show(core, json).await?,
        SettingsCommand::Set { key, value } => {
            let d = settings::find(&key).with_context(|| format!("unknown setting {key}"))?;
            let value = match d.kind {
                Kind::Secret => bail!("{key} is a secret: use `settings set-secret {key}`"),
                Kind::Bool => match value.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" => serde_json::Value::Bool(true),
                    "false" | "no" | "off" | "0" => serde_json::Value::Bool(false),
                    _ => bail!("{key} is true or false"),
                },
                Kind::Number => serde_json::Value::from(
                    value
                        .trim()
                        .parse::<u64>()
                        .with_context(|| format!("{key} is a whole number"))?,
                ),
                Kind::Choice(options) => {
                    if !options.contains(&value.as_str()) {
                        bail!("{key} is one of: {}", options.join(", "));
                    }
                    serde_json::Value::String(value)
                }
                Kind::Text => serde_json::Value::String(value),
            };
            settings::save(core, &ctx, set(&key, value)).await?;
            println!("Saved {key}.");
        }
        SettingsCommand::SetSecret { key } => {
            let d = settings::find(&key).with_context(|| format!("unknown setting {key}"))?;
            if d.kind != Kind::Secret {
                bail!("{key} is not a secret: use `settings set {key} VALUE`");
            }
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            let text = text.trim_end_matches(['\r', '\n']).to_owned();
            settings::save(core, &ctx, set(&key, serde_json::Value::String(text))).await?;
            println!("Saved {key} (encrypted).");
        }
        SettingsCommand::Test { area, to } => {
            let check = settings_test(core, &ctx, &area, to.as_deref()).await?;
            for (ok, line) in &check.lines {
                println!("{} {line}", if *ok { "OK     " } else { "PROBLEM" });
            }
            if !check.ok {
                return Ok(ExitCode::FAILURE);
            }
        }
        SettingsCommand::Import => {
            let path = config_path.context("no configuration file to import from")?;
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let r = pdms_core::settings_import::import(core, &text).await?;
            if r.imported.is_empty() && r.certificate.is_none() {
                println!("Nothing to import: these settings are already in the database.");
            }
            for k in &r.imported {
                println!("imported      {k}");
            }
            if let Some(t) = &r.certificate {
                println!("imported      the Microsoft Graph certificate ({t}) as the active one");
            }
            for k in &r.already_stored {
                println!("already set   {k} (kept the value in the database)");
            }
            for k in &r.stay_in_file {
                println!(
                    "in the file   {k} (written by hand: change it in the file, or delete it there to manage it in Administration)"
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The test behind each admin page's **Test** button, with the saved settings.
async fn settings_test(
    core: &Core,
    ctx: &Ctx,
    area: &str,
    to: Option<&str>,
) -> anyhow::Result<pdms_core::settings::Check> {
    use pdms_core::settings::Check;
    let config = core.config();
    Ok(match area {
        "microsoft-365" => {
            let o = pdms_core::graph::certs::test_connection(
                core,
                ctx,
                &config.graph.tenant_id,
                &config.graph.client_id,
            )
            .await?;
            let lines = o.lines.into_iter().map(|l| (o.ok, l)).collect();
            Check { ok: o.ok, lines }
        }
        "sign-in" => {
            let o = &config.auth.oidc;
            let form = pdms_core::auth::oidc_setup::SignInForm {
                enabled: o.enabled,
                tenant: o.issuer_url.clone(),
                client_id: o.client_id.clone(),
                client_secret: String::new(),
                display_name: o.display_name.clone(),
                auto_provision: o.auto_provision,
            };
            let t = pdms_core::auth::oidc_setup::test(core, ctx, &form).await?;
            Check {
                ok: t.ok,
                lines: t.lines,
            }
        }
        "sharepoint" => {
            let p = pdms_core::sharepoint::setup::test(
                core,
                ctx,
                &config.sharepoint.site_url,
                &config.sharepoint.library,
            )
            .await?;
            Check {
                ok: p.ok,
                lines: p.lines,
            }
        }
        "entra" => {
            let rows = pdms_core::directory::entra_sync::preview(core, ctx).await?;
            let lines = rows
                .into_iter()
                .map(|r| {
                    let what = match (r.skipped, r.maps_to) {
                        (Some(why), _) => format!("skipped: {why}"),
                        (None, Some(d)) => format!("department {d}"),
                        (None, None) => "no mapped department".to_owned(),
                    };
                    (
                        true,
                        format!("{} <{}> [{}]: {what}", r.name, r.email, r.department),
                    )
                })
                .collect();
            Check::passed(lines)
        }
        "email" => {
            let to = to.context("the email test needs --to ADDRESS")?;
            let e = &config.email;
            let form = pdms_core::notifications::mail_setup::EmailForm {
                on: true,
                host: e.smtp_host.clone(),
                port: e.smtp_port,
                security: e.smtp_security,
                username: e.smtp_username.clone(),
                password: String::new(),
                from: e.from.clone(),
                reply_to: e.reply_to.clone(),
            };
            pdms_core::notifications::mail_setup::send_test(core, ctx, &form, to).await?
        }
        "ocr" => pdms_core::ocr::setup::test(core, ctx).await?,
        "virus-scanning" => pdms_core::scanning::test(core, ctx).await?,
        other => bail!(
            "unknown area {other}: microsoft-365, sign-in, sharepoint, entra, email, ocr or virus-scanning"
        ),
    })
}

async fn settings_show(core: &Core, json: bool) -> anyhow::Result<()> {
    use pdms_core::settings::{self, Kind};
    let sources = settings::sources(core).await?;
    let config = core.config();
    let mut rows = Vec::new();
    for d in settings::SETTINGS {
        let value = settings::get(&config, d.key)?;
        let shown = if d.kind == Kind::Secret {
            let set = value.as_str().is_some_and(|s| !s.is_empty());
            serde_json::Value::String(if set { "set" } else { "not set" }.into())
        } else {
            value
        };
        let source = sources.get(d.key).map_or("default", |s| s.as_str());
        rows.push((d.key, shown, source));
    }
    if json {
        let out: Vec<serde_json::Value> = rows
            .iter()
            .map(|(k, v, s)| serde_json::json!({"key": k, "value": v, "source": s}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for (k, v, s) in rows {
            println!("{k:<36} {:<40} {s}", v.to_string());
        }
    }
    Ok(())
}

async fn connect(config: Config) -> anyhow::Result<Core> {
    let core = Core::connect(config).await?;
    let pending = pdms_core::db::pending_migrations(&core.db).await?;
    if !pending.is_empty() {
        bail!(
            "database schema is out of date (pending migrations {pending:?}); run `pdms migrate` first"
        );
    }
    // The settings stored in Administration (S1). If they can't be applied,
    // start with the configuration file's and say why.
    if let Err(e) = core.reload().await {
        tracing::error!(error = %e, "the settings stored in Administration can't be applied; using the configuration file's");
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
    let cfg = core.config().clone();
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
    if cfg.auth.oidc.enabled && !cfg.auth.oidc.auto_provision && !cfg.entra_sync.enabled {
        tracing::warn!(
            "auth.oidc.auto_provision and entra_sync are both off: single sign-on never links \
             accounts by email, so only people already linked to their single sign-on account \
             can sign in"
        );
    }

    let (tx, rx) = watch::channel(false);
    // Settings changed in Administration apply to this process at once (S1).
    let settings = pdms_core::settings::watch(core.clone());
    let worker = with_worker.then(|| {
        tokio::spawn(pdms_core::maintenance::run_worker(
            core.clone(),
            rx.clone(),
            settings.clone(),
        ))
    });

    let state = pdms_web::AppState::new(core, None, metrics);
    // Microsoft sign-in: discovered now, rebuilt when its settings change,
    // and retried while Microsoft can't be reached.
    state.refresh_oidc(false).await;
    state.spawn_oidc_watch(settings.clone());
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
        "Certificate and private key: {} (secret: give it to PDMS as PDMS_GRAPH__CERTIFICATE_FILE)",
        out.display()
    );
    println!(
        "Public certificate for Entra ID (App registrations > PDMS Service > Certificates & secrets > Upload): {}",
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

    let uri = totp::provisioning_uri(&secret, &core.config().org.name, email)?;
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
        let d = std::env::temp_dir().join(format!("pdms-cert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&d).unwrap();
        d
    }

    #[test]
    fn an_existing_public_certificate_file_is_left_untouched() {
        let d = scratch();
        let out = d.join("graph.pem");
        std::fs::write(d.join("graph.pem.crt"), "keep me").unwrap();
        assert!(write_graph_cert(&out, 30, "PDMS test").is_err());
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
        write_graph_cert(&out, 30, "PDMS test").unwrap();
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
