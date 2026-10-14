#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::*;
use pdms_core::jobs::{self, JobStatus, NewJob, Worker};
use serde_json::json;

#[tokio::test]
async fn idempotency_key_prevents_duplicates() {
    let (core, _db) = core().await;
    let a = jobs::enqueue(&core.db, NewJob::new("t", json!({})).idempotency_key("k1"))
        .await
        .unwrap();
    let b = jobs::enqueue(&core.db, NewJob::new("t", json!({})).idempotency_key("k1"))
        .await
        .unwrap();
    assert!(a.is_some() && b.is_none());
}

#[tokio::test]
async fn expired_lease_is_reclaimed_by_another_worker() {
    let (core, _db) = core().await;
    jobs::enqueue(&core.db, NewJob::new("t", json!({})))
        .await
        .unwrap();
    let job = jobs::claim(&core.db, "default", "w1", 1)
        .await
        .unwrap()
        .unwrap();
    assert!(
        jobs::claim(&core.db, "default", "w2", 1)
            .await
            .unwrap()
            .is_none(),
        "leased job is not claimable"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let again = jobs::claim(&core.db, "default", "w2", 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.id, job.id);
    assert_eq!(again.attempts, 2);
    assert!(
        !jobs::complete(&core.db, job.id, "w1").await.unwrap(),
        "stale worker cannot complete"
    );
    assert!(jobs::complete(&core.db, job.id, "w2").await.unwrap());
}

#[tokio::test]
async fn failures_back_off_then_die() {
    let (core, _db) = core().await;
    jobs::enqueue(&core.db, NewJob::new("t", json!({})).max_attempts(2))
        .await
        .unwrap();
    let j = jobs::claim(&core.db, "default", "w", 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        jobs::fail(&core.db, &j, "w", "boom").await.unwrap(),
        JobStatus::Pending
    );
    assert!(
        jobs::claim(&core.db, "default", "w", 30)
            .await
            .unwrap()
            .is_none(),
        "backoff delays retry"
    );
    sqlx::query("UPDATE jobs SET run_at = now()")
        .execute(&core.db)
        .await
        .unwrap();
    let j = jobs::claim(&core.db, "default", "w", 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        jobs::fail(&core.db, &j, "w", "boom").await.unwrap(),
        JobStatus::Dead
    );
}

#[tokio::test]
async fn workers_process_each_job_exactly_once() {
    let (core, _db) = core().await;
    for i in 0..50 {
        jobs::enqueue(&core.db, NewJob::new("count", json!({ "i": i })))
            .await
            .unwrap();
    }
    let runs = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let handles: Vec<_> = (0..3)
        .map(|_| {
            let runs = runs.clone();
            let w = Worker::new(core.db.clone(), 4, Duration::from_millis(20), 30).handle(
                "count",
                move |_| {
                    let runs = runs.clone();
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            );
            tokio::spawn(w.run(rx.clone()))
        })
        .collect();
    for _ in 0..100 {
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE status <> 'succeeded'")
            .fetch_one(&core.db)
            .await
            .unwrap();
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tx.send(true).unwrap();
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(runs.load(Ordering::SeqCst), 50);
}

#[tokio::test]
async fn periodic_scheduling_is_idempotent_across_workers() {
    let (core, _db) = core().await;
    let w1 = pdms_core::maintenance::worker(&core);
    let w2 = pdms_core::maintenance::worker(&core);
    w1.schedule_periodic().await.unwrap();
    w2.schedule_periodic().await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(
        n, 8,
        "sessions, jobs, audit, notification planning, event dispatch, audit forwarding, API log purge, \
         Outlook calendar sync"
    );
}
