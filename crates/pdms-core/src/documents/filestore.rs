// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Where document files live: SharePoint (`[storage] backend = "sharepoint"`)
//! or the blob store (local disk or S3). Every read and write of a document
//! file goes through here; export packages always use `core.storage`.

use std::path::Path;

use pdms_authz::Visibility;
use pdms_storage::spool::SpooledFile;
use pdms_storage::{BlobHash, ByteStream};
use sqlx::PgConnection;
use uuid::Uuid;

use super::files::StoredFile;
use crate::Core;
use crate::error::{CoreError, CoreResult};
use crate::sharepoint::{self, layout};

/// Where an accepted file was put, before its version is recorded.
#[derive(Debug)]
pub enum FileLocation {
    /// Content-addressed in `core.storage`; nothing more to record.
    Blob,
    SharePoint(Box<sharepoint::UploadedFile>),
}

/// The document a file belongs to.
pub struct PutTarget<'a> {
    /// `None` while the document is being created.
    pub document_id: Option<Uuid>,
    pub external_id: &'a str,
    pub title: &'a str,
    pub visibility: Visibility,
    pub department_id: Uuid,
}

/// Store an accepted upload (`src` is its spool file).
pub async fn put_version_file(
    core: &Core,
    target: &PutTarget<'_>,
    file: &StoredFile,
    src: &Path,
) -> CoreResult<FileLocation> {
    if core.sharepoint.is_none() {
        core.storage
            .put_file(file.sha256, src, file.size)
            .await
            .map_err(|e| CoreError::internal(format!("storing upload: {e}")))?;
        return Ok(FileLocation::Blob);
    }
    if file.size == 0 {
        return Err(CoreError::validation(
            "This file is empty. Empty files can't be stored.",
        ));
    }
    let up = sharepoint::upload_staged(
        core,
        &sharepoint::UploadTarget {
            document_id: target.document_id,
            external_id: target.external_id,
            title: target.title,
            spot: layout::Spot::of(target.visibility, target.department_id, false),
        },
        src,
        file.size,
        &layout::extension_for(file.media_type, &file.file_name),
    )
    .await?;
    Ok(FileLocation::SharePoint(Box::new(up)))
}

/// Record where the file of `version_no` lives. Runs in the version's transaction.
pub(crate) async fn record_location(
    conn: &mut PgConnection,
    document_id: Uuid,
    version_no: i32,
    loc: &FileLocation,
) -> CoreResult<()> {
    match loc {
        FileLocation::Blob => Ok(()),
        FileLocation::SharePoint(up) => {
            sharepoint::record_upload(conn, document_id, version_no, up).await
        }
    }
}

/// Recorded in `blobs.storage_backend`.
pub fn backend_name(core: &Core) -> &'static str {
    if core.sharepoint.is_some() {
        "sharepoint"
    } else {
        core.storage.backend()
    }
}

/// Open the file current at `version_no`.
pub async fn open(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    hash: BlobHash,
    size: u64,
) -> CoreResult<ByteStream> {
    if core.sharepoint.is_some() {
        return sharepoint::open(core, document_id, version_no, hash, size).await;
    }
    core.storage
        .get(hash)
        .await
        .map_err(|e| CoreError::internal(format!("reading blob: {e}")))
}

/// Open the file of one `document_versions` row.
pub async fn open_version_file(core: &Core, version_id: Uuid) -> CoreResult<ByteStream> {
    let v = sqlx::query!(
        r#"SELECT v.document_id, v.version_no, v.blob_sha256 AS "sha!", b.size_bytes
             FROM document_versions v JOIN blobs b ON b.sha256 = v.blob_sha256
            WHERE v.id = $1"#,
        version_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let hash = BlobHash::from_slice(&v.sha).ok_or_else(|| CoreError::internal("bad blob hash"))?;
    open(
        core,
        v.document_id,
        v.version_no,
        hash,
        u64::try_from(v.size_bytes).unwrap_or(0),
    )
    .await
}

/// A verified local copy of an earlier version's file (SharePoint mode).
pub(crate) async fn copy_for_restore(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    hash: BlobHash,
    size: u64,
) -> CoreResult<SpooledFile> {
    sharepoint::spool_copy(core, document_id, version_no, hash, size).await
}

/// The allowlisted media type matching `s`, as the `&'static str` that
/// [`StoredFile`] holds ("" if not allowlisted).
pub fn known_media_type(s: &str) -> &'static str {
    pdms_storage::sniff::ALLOWED
        .iter()
        .find(|(m, _)| *m == s)
        .map_or("", |(m, _)| *m)
}
