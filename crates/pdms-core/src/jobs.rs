// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Postgres-backed job queue and worker.
//!
//! - Claimed with `FOR UPDATE SKIP LOCKED`, so any number of workers can run.
//! - Lease-based: a job whose worker dies becomes claimable when its lease expires.
//! - Failures retry with exponential backoff and jitter, then go to `dead`.
//! - `idempotency_key` makes enqueueing safe to repeat (used by periodic tasks).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::Rng;
use serde_json::Value;
use sqlx::{PgExecutor, PgPool};
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

use crate::error::CoreResult;

#[derive(Copy, Clone, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "job_status", rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Dead,
}

#[derive(Clone, Debug)]
pub struct NewJob {
    pub queue: String,
    pub kind: String,
    pub payload: Value,
    pub run_at: DateTime<Utc>,
    pub priority: i16,
    pub max_attempts: i32,
    pub idempotency_key: Option<String>,
}

impl NewJob {
    pub fn new(kind: &str, payload: Value) -> Self {
        NewJob {
            queue: "default".to_owned(),
            kind: kind.to_owned(),
            payload,
            run_at: Utc::now(),
            priority: 0,
            max_attempts: 8,
            idempotency_key: None,
        }
    }

    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    pub fn run_at(mut self, at: DateTime<Utc>) -> Self {
        self.run_at = at;
        self
    }

    pub fn max_attempts(mut self, n: i32) -> Self {
        self.max_attempts = n;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// Enqueue a job. Returns `None` if a job with the same idempotency key exists.
pub async fn enqueue<'e>(exec: impl PgExecutor<'e>, job: NewJob) -> CoreResult<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO jobs (id, queue, kind, payload, run_at, priority, max_attempts, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (idempotency_key) DO NOTHING
         RETURNING id",
        Uuid::now_v7(),
        job.queue,
        job.kind,
        job.payload,
        job.run_at,
        job.priority,
        job.max_attempts,
        job.idempotency_key,
    )
    .fetch_optional(exec)
    .await?)
}

/// Claim one ready job (or one whose lease expired) for `worker`.
pub async fn claim(
    db: &PgPool,
    queue: &str,
    worker: &str,
    lease_seconds: i64,
) -> CoreResult<Option<Job>> {
    let row = sqlx::query!(
        r#"UPDATE jobs
              SET status = 'running', attempts = attempts + 1, locked_by = $2,
                  lease_expires_at = now() + make_interval(secs => $3), updated_at = now()
            WHERE id = (
                SELECT id FROM jobs
                 WHERE queue = $1
                   AND ((status = 'pending' AND run_at <= now())
                        OR (status = 'running' AND lease_expires_at < now()))
                 ORDER BY priority DESC, run_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED)
            RETURNING id, kind, payload, attempts, max_attempts"#,
        queue,
        worker,
        lease_seconds as f64,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Job {
        id: r.id,
        kind: r.kind,
        payload: r.payload,
        attempts: r.attempts,
        max_attempts: r.max_attempts,
    }))
}

/// Mark succeeded. Only the current lease holder can complete a job.
pub async fn complete(db: &PgPool, id: Uuid, worker: &str) -> CoreResult<bool> {
    let r = sqlx::query!(
        "UPDATE jobs SET status = 'succeeded', finished_at = now(), updated_at = now(),
                         locked_by = NULL, lease_expires_at = NULL, last_error = NULL
          WHERE id = $1 AND status = 'running' AND locked_by = $2",
        id,
        worker
    )
    .execute(db)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Exponential backoff from 30 s, capped at 6 h, with ±20 % jitter.
pub fn backoff(attempts: i32) -> chrono::Duration {
    let exp = u32::try_from(attempts.saturating_sub(1).clamp(0, 16)).unwrap_or(16);
    let base = (30_i64 * 2_i64.pow(exp)).min(6 * 3600);
    let jitter = rand::thread_rng().gen_range(0.8..1.2);
    chrono::Duration::milliseconds((base as f64 * 1000.0 * jitter) as i64)
}

/// Record a failure: retry later, or move to `dead` after `max_attempts`.
pub async fn fail(db: &PgPool, job: &Job, worker: &str, error: &str) -> CoreResult<JobStatus> {
    let dead = job.attempts >= job.max_attempts;
    let status = if dead {
        JobStatus::Dead
    } else {
        JobStatus::Pending
    };
    let run_at = Utc::now() + backoff(job.attempts);
    sqlx::query!(
        "UPDATE jobs SET status = $3, last_error = $4, run_at = $5, updated_at = now(),
                         locked_by = NULL, lease_expires_at = NULL,
                         finished_at = CASE WHEN $3 = 'dead'::job_status THEN now() END
          WHERE id = $1 AND locked_by = $2",
        job.id,
        worker,
        status as JobStatus,
        error.chars().take(4000).collect::<String>(),
        run_at,
    )
    .execute(db)
    .await?;
    Ok(status)
}

pub type JobFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
pub type Handler = Arc<dyn Fn(Job) -> JobFuture + Send + Sync>;

/// A task enqueued once per `every` interval, deduplicated by idempotency key.
#[derive(Clone)]
pub struct Periodic {
    pub kind: &'static str,
    pub every: Duration,
}

pub struct Worker {
    db: PgPool,
    id: String,
    queue: String,
    concurrency: usize,
    poll: Duration,
    lease_seconds: i64,
    handlers: HashMap<String, Handler>,
    periodic: Vec<Periodic>,
}

impl Worker {
    pub fn new(db: PgPool, concurrency: usize, poll: Duration, lease_seconds: i64) -> Self {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "worker".into());
        Worker {
            db,
            id: format!("{host}:{}", Uuid::new_v4().simple()),
            queue: "default".to_owned(),
            concurrency: concurrency.max(1),
            poll,
            lease_seconds,
            handlers: HashMap::new(),
            periodic: Vec::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn handle<F, Fut>(mut self, kind: &str, f: F) -> Self
    where
        F: Fn(Job) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.handlers
            .insert(kind.to_owned(), Arc::new(move |job| Box::pin(f(job))));
        self
    }

    pub fn every(mut self, kind: &'static str, every: Duration) -> Self {
        self.periodic.push(Periodic { kind, every });
        self
    }

    /// Enqueue due periodic tasks; the bucketed key makes this idempotent across workers and restarts.
    pub async fn schedule_periodic(&self) -> CoreResult<()> {
        let now = Utc::now().timestamp();
        for p in &self.periodic {
            let secs = i64::try_from(p.every.as_secs().max(1)).unwrap_or(i64::MAX);
            let bucket = now / secs;
            enqueue(
                &self.db,
                NewJob::new(p.kind, Value::Null)
                    .idempotency_key(format!("periodic:{}:{bucket}", p.kind))
                    .max_attempts(3),
            )
            .await?;
        }
        Ok(())
    }

    /// Run until `shutdown` becomes true; in-flight jobs finish first.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let slots = Arc::new(Semaphore::new(self.concurrency));
        let me = Arc::new(self);
        tracing::info!(worker = %me.id, concurrency = me.concurrency, "worker started");
        let mut last_schedule = std::time::Instant::now() - Duration::from_secs(3600);
        loop {
            if *shutdown.borrow() {
                break;
            }
            if last_schedule.elapsed() >= Duration::from_secs(30) {
                if let Err(e) = me.schedule_periodic().await {
                    tracing::error!(error = %e, "scheduling periodic jobs failed");
                }
                last_schedule = std::time::Instant::now();
            }
            let Ok(permit) = slots.clone().acquire_owned().await else {
                break;
            };
            match claim(&me.db, &me.queue, &me.id, me.lease_seconds).await {
                Ok(Some(job)) => {
                    let me = me.clone();
                    tokio::spawn(async move {
                        me.execute(job).await;
                        drop(permit);
                    });
                }
                Ok(None) => {
                    drop(permit);
                    tokio::select! {
                        _ = tokio::time::sleep(me.poll) => {}
                        _ = shutdown.changed() => {}
                    }
                }
                Err(e) => {
                    drop(permit);
                    tracing::error!(error = %e, "claiming job failed");
                    tokio::time::sleep(me.poll).await;
                }
            }
        }
        // Wait for in-flight jobs.
        let _ = slots
            .acquire_many(u32::try_from(me.concurrency).unwrap_or(u32::MAX))
            .await;
        tracing::info!(worker = %me.id, "worker stopped");
    }

    async fn execute(&self, job: Job) {
        let started = std::time::Instant::now();
        let kind = job.kind.clone();
        let result = match self.handlers.get(&job.kind) {
            None => Err(format!("no handler registered for job kind {:?}", job.kind)),
            Some(_) if job.attempts > job.max_attempts => {
                Err("attempts exhausted (lease expired)".into())
            }
            Some(h) => h(job.clone()).await,
        };
        let outcome = match result {
            Ok(()) => match complete(&self.db, job.id, &self.id).await {
                Ok(true) => "succeeded",
                Ok(false) => "lost_lease",
                Err(e) => {
                    tracing::error!(job_id = %job.id, error = %e, "recording job success failed");
                    "error"
                }
            },
            Err(err) => {
                tracing::warn!(job_id = %job.id, kind = %kind, attempts = job.attempts, error = %err, "job failed");
                match fail(&self.db, &job, &self.id, &err).await {
                    Ok(JobStatus::Dead) => "dead",
                    Ok(_) => "retry",
                    Err(e) => {
                        tracing::error!(job_id = %job.id, error = %e, "recording job failure failed");
                        "error"
                    }
                }
            }
        };
        metrics::counter!("pdms_jobs_total", "kind" => kind.clone(), "outcome" => outcome)
            .increment(1);
        metrics::histogram!("pdms_job_duration_seconds", "kind" => kind)
            .record(started.elapsed().as_secs_f64());
    }
}

/// Remove finished jobs older than `keep_days`.
pub async fn purge_finished(db: &PgPool, keep_days: i32) -> CoreResult<u64> {
    let r = sqlx::query!(
        "DELETE FROM jobs WHERE status IN ('succeeded', 'dead') AND finished_at < now() - make_interval(days => $1)",
        keep_days
    )
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let s = |n| backoff(n).num_seconds();
        assert!((24..=36).contains(&s(1)));
        assert!((48..=72).contains(&s(2)));
        assert!(s(30) <= 6 * 3600 * 12 / 10);
    }
}
