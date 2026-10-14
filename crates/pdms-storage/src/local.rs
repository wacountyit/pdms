// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::{BlobHash, BlobStore, ByteStream, StorageError};

/// Blobs as read-only files under `root/ab/cd/<sha256>`.
///
/// Writes go to `root/.tmp` first, are hashed while copying, fsynced, and
/// renamed into place, so a blob path only ever holds complete, verified content.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        std::fs::create_dir_all(root.join(".tmp"))?;
        Ok(LocalStore { root })
    }

    fn path(&self, hash: BlobHash) -> PathBuf {
        self.root.join(hash.relative_path())
    }
}

#[async_trait]
impl BlobStore for LocalStore {
    fn backend(&self) -> &'static str {
        "local"
    }

    async fn put_file(&self, hash: BlobHash, src: &Path, size: u64) -> Result<(), StorageError> {
        let target = self.path(hash);
        if let Ok(meta) = tokio::fs::metadata(&target).await {
            return if meta.len() == size {
                Ok(())
            } else {
                Err(StorageError::Corrupt(hash))
            };
        }
        if let Some(dir) = target.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let tmp = self
            .root
            .join(".tmp")
            .join(uuid::Uuid::new_v4().to_string());
        let result = async {
            let mut input = tokio::fs::File::open(src).await?;
            let mut output = tokio::fs::File::create(&tmp).await?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = input.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                output.write_all(&buf[..n]).await?;
            }
            output.sync_all().await?;
            drop(output);
            if <[u8; 32]>::from(hasher.finalize()) != hash.0 {
                return Err(StorageError::Corrupt(hash));
            }
            set_read_only(&tmp).await?;
            // Another writer may have stored the same content meanwhile; either copy is identical.
            tokio::fs::rename(&tmp, &target).await?;
            if let Some(dir) = target.parent() {
                sync_dir(dir).await;
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        result
    }

    async fn get(&self, hash: BlobHash) -> Result<ByteStream, StorageError> {
        match tokio::fs::File::open(self.path(hash)).await {
            Ok(f) => Ok(Box::pin(ReaderStream::with_capacity(f, 64 * 1024))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StorageError::NotFound(hash)),
            Err(e) => Err(e.into()),
        }
    }

    async fn size(&self, hash: BlobHash) -> Result<Option<u64>, StorageError> {
        match tokio::fs::metadata(self.path(hash)).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(unix)]
async fn set_read_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o440)).await
}

#[cfg(not(unix))]
async fn set_read_only(path: &Path) -> std::io::Result<()> {
    let mut p = tokio::fs::metadata(path).await?.permissions();
    p.set_readonly(true);
    tokio::fs::set_permissions(path, p).await
}

async fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = tokio::fs::File::open(dir).await {
        let _ = d.sync_all().await;
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::sha256;

    async fn collect(mut s: ByteStream) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = s.next().await {
            out.extend_from_slice(&c.unwrap());
        }
        out
    }

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("pdms-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn put_get_is_idempotent_and_verified() {
        let dir = tmpdir();
        let store = LocalStore::open(dir.join("blobs")).unwrap();
        let src = dir.join("in");
        std::fs::write(&src, b"hello world").unwrap();
        let h = sha256(b"hello world");
        store.put_file(h, &src, 11).await.unwrap();
        store.put_file(h, &src, 11).await.unwrap();
        assert_eq!(collect(store.get(h).await.unwrap()).await, b"hello world");
        assert_eq!(store.size(h).await.unwrap(), Some(11));
        assert!(store.verify(h).await.unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wrong_hash_is_rejected_and_nothing_stored() {
        let dir = tmpdir();
        let store = LocalStore::open(dir.join("blobs")).unwrap();
        let src = dir.join("in");
        std::fs::write(&src, b"actual content").unwrap();
        let claimed = sha256(b"other content");
        assert!(matches!(
            store.put_file(claimed, &src, 14).await,
            Err(StorageError::Corrupt(_))
        ));
        assert_eq!(store.size(claimed).await.unwrap(), None);
        assert!(matches!(
            store.get(claimed).await,
            Err(StorageError::NotFound(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn tampered_blob_fails_verification() {
        let dir = tmpdir();
        let store = LocalStore::open(dir.join("blobs")).unwrap();
        let src = dir.join("in");
        std::fs::write(&src, b"original").unwrap();
        let h = sha256(b"original");
        store.put_file(h, &src, 8).await.unwrap();
        let path = dir.join("blobs").join(h.relative_path());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        std::fs::write(&path, b"tampered").unwrap();
        assert!(!store.verify(h).await.unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
