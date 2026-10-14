// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Content-addressed, write-once blob storage.
//!
//! Files are stored under their SHA-256 and never overwritten or deleted
//! through this interface. Two backends: the local filesystem and any
//! S3-compatible service (MinIO, AWS S3). Also: upload spooling with hashing
//! and size limits, content sniffing, and a virus-scan hook.

mod local;
mod s3;
pub mod scan;
pub mod sniff;
pub mod spool;

use std::fmt;
use std::path::Path;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use sha2::{Digest, Sha256};

pub use local::LocalStore;
pub use s3::{S3Config, S3Store};

/// A SHA-256 digest: the identity and storage key of a blob.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_slice(b: &[u8]) -> Option<Self> {
        b.try_into().ok().map(BlobHash)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        hex::decode(s).ok().and_then(|v| Self::from_slice(&v))
    }

    /// Fan-out path `ab/cd/abcd…` so no directory gets too large.
    pub fn relative_path(&self) -> String {
        let h = self.hex();
        format!("{}/{}/{h}", &h[0..2], &h[2..4])
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash({})", self.hex())
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("blob {0} not found")]
    NotFound(BlobHash),
    #[error("stored blob {0} does not match its hash")]
    Corrupt(BlobHash),
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("object store error: {0}")]
    ObjectStore(String),
}

pub type ByteStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

#[async_trait]
pub trait BlobStore: Send + Sync + fmt::Debug {
    /// Backend name recorded with each blob (`local`, `s3`).
    fn backend(&self) -> &'static str;

    /// Store the file at `src` under `hash`. Idempotent: if the blob already
    /// exists nothing is written. Implementations must never overwrite.
    async fn put_file(&self, hash: BlobHash, src: &Path, size: u64) -> Result<(), StorageError>;

    /// Stream a blob's content.
    async fn get(&self, hash: BlobHash) -> Result<ByteStream, StorageError>;

    /// Size of a stored blob, or `None` if absent.
    async fn size(&self, hash: BlobHash) -> Result<Option<u64>, StorageError>;

    /// Re-read the blob and check its content still hashes to `hash`.
    async fn verify(&self, hash: BlobHash) -> Result<bool, StorageError> {
        let mut stream = self.get(hash).await?;
        let mut hasher = Sha256::new();
        while let Some(chunk) = stream.next().await {
            hasher.update(&chunk?);
        }
        Ok(<[u8; 32]>::from(hasher.finalize()) == hash.0)
    }
}

pub fn sha256(data: &[u8]) -> BlobHash {
    BlobHash(Sha256::digest(data).into())
}
