// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Worker jobs for SharePoint operations (P4): department-folder access and,
//! from Task 4, the change check.

use std::time::Duration;

use super::access::{self, RECONCILE_JOB};
use crate::Core;
use crate::jobs::Worker;

/// Register P4's jobs. The folder-access handler is always registered,
/// because directory changes queue it whether or not SharePoint is on (it
/// is a no-op then); the schedules only with SharePoint storage.
pub fn register_ops_jobs(worker: Worker, core: &Core) -> Worker {
    let c1 = core.clone();
    let worker = worker.handle(RECONCILE_JOB, move |_| {
        let core = c1.clone();
        async move {
            let r = access::reconcile(&core).await.map_err(|e| e.to_string())?;
            if r.granted + r.revoked > 0 || !r.errors.is_empty() {
                tracing::info!(?r, "SharePoint folder access updated");
            }
            Ok(())
        }
    });
    if core.sharepoint.is_none() {
        return worker;
    }
    worker.every(RECONCILE_JOB, Duration::from_secs(24 * 3600))
}
