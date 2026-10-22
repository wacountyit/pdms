// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! `verify-integrity`: after a restore (or at any time), check that the
//! database and the file store agree and that nothing was altered.
//!
//! - every stored file referenced by the database exists, has the recorded
//!   size, and still hashes to its SHA-256;
//! - every export package is present and intact;
//! - every document version points at a recorded file;
//! - the audit hash chain is unbroken.
//!
//! The run itself is recorded in the audit log.

use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_storage::BlobHash;
use serde::Serialize;

use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;

#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    /// `file`, `export_package`, `version`, or `audit`.
    pub kind: &'static str,
    pub subject: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct IntegrityReport {
    pub files_checked: u64,
    pub bytes_checked: u64,
    pub packages_checked: u64,
    pub versions_checked: u64,
    pub audit_entries_checked: u64,
    pub audit_last_seq: i64,
    pub problems: Vec<Problem>,
    /// File contents were not re-hashed (`--skip-files`); presence and size were checked.
    pub contents_skipped: bool,
    /// Files stored in SharePoint. They are checked against SharePoint by the
    /// change check (P4), not re-read here, and not counted in `files_checked`.
    pub sharepoint_files_skipped: u64,
}

impl IntegrityReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Check one stored object: present, right size, right hash.
async fn check_blob(
    core: &Core,
    kind: &'static str,
    hash: BlobHash,
    size: i64,
    rehash: bool,
    out: &mut Vec<Problem>,
) -> CoreResult<()> {
    let problem = |detail: String| Problem {
        kind,
        subject: hash.hex(),
        detail,
    };
    match core.storage.size(hash).await {
        Ok(None) => {
            out.push(problem("missing from the file store".into()));
            return Ok(());
        }
        Ok(Some(actual)) if i64::try_from(actual).ok() != Some(size) => {
            out.push(problem(format!("size is {actual} bytes, expected {size}")));
            return Ok(());
        }
        Ok(Some(_)) => {}
        Err(e) => {
            out.push(problem(format!("could not be read: {e}")));
            return Ok(());
        }
    }
    if rehash {
        match core.storage.verify(hash).await {
            Ok(true) => {}
            Ok(false) => out.push(problem("content does not match its SHA-256".into())),
            Err(e) => out.push(problem(format!("could not be read: {e}"))),
        }
    }
    Ok(())
}

/// Run every check. `rehash = false` checks presence and size only (fast).
pub async fn verify(core: &Core, rehash: bool) -> CoreResult<IntegrityReport> {
    let mut problems = Vec::new();
    let blobs =
        sqlx::query!("SELECT sha256, size_bytes, storage_backend FROM blobs ORDER BY created_at")
            .fetch_all(&core.db)
            .await?;
    let mut bytes = 0u64;
    let mut sharepoint_files_skipped = 0u64;
    for b in &blobs {
        if b.storage_backend == "sharepoint" {
            sharepoint_files_skipped += 1;
            continue;
        }
        let Some(hash) = BlobHash::from_slice(&b.sha256) else {
            problems.push(Problem {
                kind: "file",
                subject: hex::encode(&b.sha256),
                detail: "malformed hash in the database".into(),
            });
            continue;
        };
        check_blob(core, "file", hash, b.size_bytes, rehash, &mut problems).await?;
        bytes += u64::try_from(b.size_bytes).unwrap_or(0);
    }
    let packages = sqlx::query!(
        "SELECT id, request_reference, package_sha256, size_bytes FROM export_packages"
    )
    .fetch_all(&core.db)
    .await?;
    for p in &packages {
        match BlobHash::from_slice(&p.package_sha256) {
            Some(hash) => {
                let before = problems.len();
                check_blob(
                    core,
                    "export_package",
                    hash,
                    p.size_bytes,
                    rehash,
                    &mut problems,
                )
                .await?;
                for pr in problems.iter_mut().skip(before) {
                    pr.subject = format!("{} ({})", p.request_reference, p.id);
                }
            }
            None => problems.push(Problem {
                kind: "export_package",
                subject: p.id.to_string(),
                detail: "malformed hash in the database".into(),
            }),
        }
    }
    let versions = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM document_versions"#)
        .fetch_one(&core.db)
        .await?;
    let dangling = sqlx::query!(
        r#"SELECT v.document_id, v.version_no FROM document_versions v
            WHERE v.blob_sha256 IS NOT NULL
              AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.sha256 = v.blob_sha256)"#
    )
    .fetch_all(&core.db)
    .await?;
    for d in dangling {
        problems.push(Problem {
            kind: "version",
            subject: format!("{} v{}", d.document_id, d.version_no),
            detail: "points at a file the database does not record".into(),
        });
    }
    let audit = pdms_audit::verify(&core.db).await?;
    for b in audit.breaks.iter().take(100) {
        problems.push(Problem {
            kind: "audit",
            subject: "audit_log".into(),
            detail: b.to_string(),
        });
    }
    if audit.truncated {
        problems.push(Problem {
            kind: "audit",
            subject: "audit_log".into(),
            detail: "more breaks than shown".into(),
        });
    }
    let report = IntegrityReport {
        files_checked: blobs.len() as u64 - sharepoint_files_skipped,
        bytes_checked: bytes,
        packages_checked: packages.len() as u64,
        versions_checked: u64::try_from(versions).unwrap_or(0),
        audit_entries_checked: audit.entries_checked,
        audit_last_seq: audit.last.seq,
        problems,
        contents_skipped: !rehash,
        sharepoint_files_skipped,
    };
    let ctx = Ctx::system("verify-integrity");
    let mut e = AuditEvent::new(
        Category::System,
        "integrity.verified",
        ctx.actor.clone(),
        "system",
    )
    .context("ok", report.is_ok())
    .context(
        "files_checked",
        i64::try_from(report.files_checked).unwrap_or(i64::MAX),
    )
    .context(
        "packages_checked",
        i64::try_from(report.packages_checked).unwrap_or(i64::MAX),
    )
    .context(
        "audit_entries_checked",
        i64::try_from(report.audit_entries_checked).unwrap_or(i64::MAX),
    )
    .context(
        "sharepoint_files_skipped",
        i64::try_from(report.sharepoint_files_skipped).unwrap_or(i64::MAX),
    )
    .context("contents_rehashed", rehash)
    .context(
        "problems",
        i64::try_from(report.problems.len()).unwrap_or(i64::MAX),
    );
    if !report.is_ok() {
        e = e.outcome(Outcome::Failure);
    }
    pdms_audit::record(&core.db, vec![e]).await?;
    Ok(report)
}
