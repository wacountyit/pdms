// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Stream an upload to a temporary file while hashing it and enforcing a size limit.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::BlobHash;

/// Bytes kept from the start of the file for content sniffing.
pub const HEAD_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("file is larger than the {limit}-byte limit")]
    TooLarge { limit: u64 },
    #[error("upload spool I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct SpoolWriter {
    file: tokio::fs::File,
    path: PathBuf,
    hasher: Sha256,
    size: u64,
    limit: u64,
    head: Vec<u8>,
}

impl SpoolWriter {
    pub async fn create(dir: &Path, limit: u64) -> Result<Self, SpoolError> {
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!("upload-{}", uuid::Uuid::new_v4()));
        let file = tokio::fs::File::create(&path).await?;
        Ok(SpoolWriter {
            file,
            path,
            hasher: Sha256::new(),
            size: 0,
            limit,
            head: Vec::new(),
        })
    }

    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), SpoolError> {
        self.size += chunk.len() as u64;
        if self.size > self.limit {
            let _ = tokio::fs::remove_file(&self.path).await;
            return Err(SpoolError::TooLarge { limit: self.limit });
        }
        if self.head.len() < HEAD_BYTES {
            let take = (HEAD_BYTES - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
        }
        self.hasher.update(chunk);
        self.file.write_all(chunk).await?;
        Ok(())
    }

    pub async fn finish(mut self) -> Result<SpooledFile, SpoolError> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        Ok(SpooledFile {
            path: std::mem::take(&mut self.path),
            sha256: BlobHash(self.hasher.clone().finalize().into()),
            size: self.size,
            head: std::mem::take(&mut self.head),
        })
    }
}

impl Drop for SpoolWriter {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A fully received upload. The temporary file is deleted when this is dropped.
#[derive(Debug)]
pub struct SpooledFile {
    pub path: PathBuf,
    pub sha256: BlobHash,
    pub size: u64,
    pub head: Vec<u8>,
}

impl Drop for SpooledFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Make a client-supplied file name safe to store and to send back in
/// `Content-Disposition`: no path components or control characters, at most
/// 200 characters, never empty.
pub fn sanitize_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>' | ':' | '|' | '?' | '*'))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let mut out: String = cleaned.chars().take(200).collect();
    if out.is_empty() {
        out = "file".to_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_limits_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("pdms-spool-{}", uuid::Uuid::new_v4()));
        let mut w = SpoolWriter::create(&dir, 10).await.unwrap();
        w.write(b"hello ").await.unwrap();
        w.write(b"you").await.unwrap();
        let f = w.finish().await.unwrap();
        assert_eq!(f.sha256, crate::sha256(b"hello you"));
        assert_eq!(f.size, 9);
        assert_eq!(f.head, b"hello you");
        let path = f.path.clone();
        assert!(path.exists());
        drop(f);
        assert!(!path.exists(), "temp file removed on drop");

        let mut w = SpoolWriter::create(&dir, 4).await.unwrap();
        assert!(matches!(
            w.write(b"too long").await,
            Err(SpoolError::TooLarge { limit: 4 })
        ));
        drop(w);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "no leftovers");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(
            sanitize_file_name("C:\\Users\\x\\Contract \"final\".pdf"),
            "Contract final.pdf"
        );
        assert_eq!(sanitize_file_name("a\nb\r.pdf"), "ab.pdf");
        assert_eq!(sanitize_file_name("..."), "file");
        assert_eq!(sanitize_file_name(".htaccess"), "htaccess");
        assert_eq!(
            sanitize_file_name("Ünïcode résumé.docx"),
            "Ünïcode résumé.docx"
        );
    }
}
