// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Worker jobs for SharePoint operations (P4): department-folder access and,
//! from Task 4, the change check.

use std::time::Duration;

use super::access::{self, RECONCILE_JOB};
use super::changes::{self, DELTA_JOB};
use crate::Core;
use crate::jobs::Worker;

/// Register P4's jobs. Both handlers are always registered, because
/// directory changes and the status panel queue them whether or not
/// SharePoint is on (they are no-ops then); the schedules only with
/// SharePoint storage.
pub fn register_ops_jobs(worker: Worker, core: &Core) -> Worker {
    let (c1, c2) = (core.clone(), core.clone());
    let worker = worker
        .handle(RECONCILE_JOB, move |_| {
            let core = c1.clone();
            async move {
                let r = access::reconcile(&core).await.map_err(|e| e.to_string())?;
                if r.granted + r.revoked > 0 || !r.errors.is_empty() {
                    tracing::info!(?r, "SharePoint folder access updated");
                }
                Ok(())
            }
        })
        .handle(DELTA_JOB, move |_| {
            let core = c2.clone();
            async move {
                let r = changes::detect_changes(&core)
                    .await
                    .map_err(|e| e.to_string())?;
                if r.modified + r.missing + r.unexpected > 0 || r.resynced {
                    tracing::info!(?r, "SharePoint changes checked");
                }
                Ok(())
            }
        });
    if core.sharepoint.is_none() {
        return worker;
    }
    let every = Duration::from_secs(core.config.sharepoint.delta_interval_minutes.max(1) * 60);
    worker
        .every(RECONCILE_JOB, Duration::from_secs(24 * 3600))
        .every(DELTA_JOB, every)
}
