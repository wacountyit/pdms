// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Built-in periodic jobs.

use std::time::Duration;

use crate::Core;
use crate::context::Ctx;
use crate::jobs::Worker;

pub const SESSIONS_PURGE: &str = "maintenance.sessions_purge";
pub const JOBS_PURGE: &str = "maintenance.jobs_purge";
pub const AUDIT_VERIFY: &str = "maintenance.audit_verify";
pub const PLAN_NOTIFICATIONS: &str = "notifications.plan";
pub const API_LOG_PURGE: &str = "maintenance.api_log_purge";

/// A worker with the built-in handlers and schedule registered.
pub fn worker(core: &Core) -> Worker {
    let w = &core.config.worker;
    let (db1, db2, db3) = (core.db.clone(), core.db.clone(), core.db.clone());
    let worker = Worker::new(
        core.db.clone(),
        w.concurrency,
        Duration::from_millis(w.poll_interval_ms),
        w.lease_seconds,
    );
    let worker_id = worker.id().to_owned();
    let worker_id2 = worker_id.clone();
    let (planner_core, mail_core) = (core.clone(), core.clone());
    let worker = worker
        .handle(PLAN_NOTIFICATIONS, move |_| {
            let core = planner_core.clone();
            async move {
                let r = crate::notifications::plan(&core, chrono::Utc::now())
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(?r, "notification planning finished");
                Ok(())
            }
        })
        .handle(crate::notifications::EMAIL_JOB, move |job| {
            let core = mail_core.clone();
            let worker_id = worker_id.clone();
            async move {
                let id = job
                    .payload
                    .get("delivery_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .ok_or("missing delivery_id")?;
                crate::notifications::delivery::send_email(&core, id, &worker_id).await
            }
        })
        .every(PLAN_NOTIFICATIONS, Duration::from_secs(15 * 60))
        .handle(SESSIONS_PURGE, move |_| {
            let db = db1.clone();
            async move {
                let n = crate::auth::session::purge_expired(&db, 7)
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(purged = n, "expired sessions purged");
                Ok(())
            }
        })
        .handle(JOBS_PURGE, move |_| {
            let db = db2.clone();
            async move {
                let n = crate::jobs::purge_finished(&db, 30)
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(purged = n, "finished jobs purged");
                Ok(())
            }
        })
        .handle(AUDIT_VERIFY, move |_| {
            let db = db3.clone();
            async move {
                let report = crate::audit_log::verify(&db, &Ctx::system("scheduled-verify"))
                    .await
                    .map_err(|e| e.to_string())?;
                if report.is_intact() {
                    tracing::info!(entries = report.entries_checked, "audit chain verified");
                } else {
                    for b in report.breaks.iter().take(20) {
                        tracing::error!(%b, "AUDIT CHAIN BREAK");
                    }
                    metrics::gauge!("pdms_audit_chain_intact").set(0.0);
                    return Ok(());
                }
                metrics::gauge!("pdms_audit_chain_intact").set(1.0);
                Ok(())
            }
        })
        .every(SESSIONS_PURGE, Duration::from_secs(3600))
        .every(JOBS_PURGE, Duration::from_secs(24 * 3600))
        .every(AUDIT_VERIFY, Duration::from_secs(24 * 3600))
        .handle(crate::integrations::DISPATCH_JOB, {
            let core = core.clone();
            move |_| {
                let core = core.clone();
                async move {
                    let r = crate::integrations::dispatch_all(&core)
                        .await
                        .map_err(|e| e.to_string())?;
                    if r.deliveries > 0 {
                        tracing::info!(?r, "integration events dispatched");
                    }
                    Ok(())
                }
            }
        })
        .handle(crate::integrations::DELIVER_JOB, {
            let core = core.clone();
            let worker_id = worker_id2;
            move |job| {
                let core = core.clone();
                let worker_id = worker_id.clone();
                async move {
                    let id = job
                        .payload
                        .get("delivery_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .ok_or("missing delivery_id")?;
                    crate::integrations::webhooks::deliver(&core, id, &worker_id).await
                }
            }
        })
        .handle(crate::integrations::forward::FORWARD_JOB, {
            let core = core.clone();
            move |_| {
                let core = core.clone();
                async move {
                    crate::integrations::forward::forward(&core)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
            }
        })
        .handle(API_LOG_PURGE, {
            let core = core.clone();
            move |_| {
                let core = core.clone();
                async move {
                    let n = crate::api_tokens::purge_request_log(
                        &core.db,
                        core.config.api.request_log_days,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    tracing::info!(deleted = n, "API request log purged");
                    Ok(())
                }
            }
        })
        .handle(crate::ocr::EXTRACT_JOB, {
            let core = core.clone();
            move |job| {
                let core = core.clone();
                async move {
                    let doc = job
                        .payload
                        .get("document_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .ok_or("missing document_id")?;
                    let version = job
                        .payload
                        .get("version_no")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|v| i32::try_from(v).ok())
                        .ok_or("missing version_no")?;
                    let force = job
                        .payload
                        .get("force")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    crate::ocr::extract(&core, doc, version, force)
                        .await
                        .map_err(|e| e.to_string())
                }
            }
        })
        .every(crate::integrations::DISPATCH_JOB, Duration::from_secs(30))
        .every(
            crate::integrations::forward::FORWARD_JOB,
            Duration::from_secs(30),
        )
        .every(API_LOG_PURGE, Duration::from_secs(24 * 3600));
    use crate::directory::entra_sync;
    let worker = worker
        .handle(entra_sync::SYNC_JOB, {
            let core = core.clone();
            move |job| {
                let core = core.clone();
                async move { entra_sync::run_job(&core, &job.payload).await }
            }
        })
        .handle(entra_sync::TICK_JOB, {
            let core = core.clone();
            move |_| {
                let core = core.clone();
                async move {
                    entra_sync::schedule_daily(&core, chrono::Utc::now())
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
            }
        });
    let worker = if core.config.entra_sync.enabled {
        worker.every(entra_sync::TICK_JOB, Duration::from_secs(15 * 60))
    } else {
        worker
    };
    crate::sharepoint::register_jobs(worker, core)
}
