// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Accepting an uploaded file: sniff its type and scan it.

use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_storage::BlobHash;
use pdms_storage::scan::{ScanError, ScanVerdict};
use pdms_storage::sniff;
use pdms_storage::spool::SpooledFile;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

/// A received upload awaiting acceptance. The temp file is removed on drop.
#[derive(Debug)]
pub struct Upload {
    pub file: SpooledFile,
    /// Already sanitized with `pdms_storage::spool::sanitize_file_name`.
    pub file_name: String,
}

#[derive(Clone, Debug)]
pub struct StoredFile {
    pub sha256: BlobHash,
    pub size: u64,
    pub media_type: &'static str,
    pub file_name: String,
    /// `clean`, `not_scanned`, or `scanner_unavailable` (only when scanning is optional).
    pub scan: &'static str,
}

async fn reject(
    core: &Core,
    ctx: &Ctx,
    document: Option<Uuid>,
    upload: &Upload,
    reason: &str,
    msg: String,
) -> CoreError {
    let mut e = AuditEvent::new(
        Category::Content,
        "document.upload_rejected",
        ctx.actor.clone(),
        "document",
    )
    .outcome(Outcome::Failure)
    .context("reason", reason)
    .context("detail", msg.clone())
    .context("file_name", upload.file_name.clone())
    .context("sha256", upload.file.sha256.hex())
    .context(
        "size_bytes",
        i64::try_from(upload.file.size).unwrap_or(i64::MAX),
    );
    if let Some(d) = document {
        e = e.document(d).object(d, None);
    }
    if let Err(err) = pdms_audit::record(&core.db, vec![e]).await {
        return err.into();
    }
    CoreError::Validation(msg)
}

/// Validate and scan an upload. Rejections are audited. Storing it is
/// `filestore::put_version_file`.
pub async fn ingest(
    core: &Core,
    ctx: &Ctx,
    document: Option<Uuid>,
    upload: &Upload,
) -> CoreResult<StoredFile> {
    let detected = match sniff::detect(&upload.file.head, &upload.file_name) {
        Ok(d) => d,
        Err(e) => {
            return Err(reject(core, ctx, document, upload, "file_type", e.to_string()).await);
        }
    };
    let scan = match &core.scanner {
        None => "not_scanned",
        Some(s) => match s.scan(&upload.file.path).await {
            Ok(ScanVerdict::Clean) => "clean",
            Ok(ScanVerdict::Infected(name)) => {
                tracing::warn!(file = %upload.file_name, signature = %name, "upload rejected by virus scanner");
                let msg = format!("The virus scanner rejected this file ({name}).");
                return Err(reject(core, ctx, document, upload, "virus", msg).await);
            }
            Err(e @ (ScanError::Unavailable(_) | ScanError::Scanner(_))) => {
                tracing::error!(error = %e, "virus scan failed");
                if core.config.storage.clamav_required {
                    let msg = "Virus scanning is unavailable, so the file was not accepted. Try again later.".to_owned();
                    return Err(
                        reject(core, ctx, document, upload, "scanner_unavailable", msg).await,
                    );
                }
                "scanner_unavailable"
            }
        },
    };
    Ok(StoredFile {
        sha256: upload.file.sha256,
        size: upload.file.size,
        media_type: detected.media_type,
        file_name: upload.file_name.clone(),
        scan,
    })
}

/// Record the blob row (idempotent; content-addressed).
pub(crate) async fn record_blob(
    conn: &mut PgConnection,
    f: &StoredFile,
    backend: &str,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO blobs (sha256, size_bytes, media_type, storage_backend) VALUES ($1, $2, $3, $4)
         ON CONFLICT (sha256) DO NOTHING",
        &f.sha256.0[..],
        i64::try_from(f.size).unwrap_or(i64::MAX),
        f.media_type,
        backend,
    )
    .execute(conn)
    .await?;
    Ok(())
}
