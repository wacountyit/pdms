// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload, WriteMultipart};
use tokio::io::AsyncReadExt;

use crate::{BlobHash, BlobStore, ByteStream, StorageError};

/// Files up to this size are uploaded in one conditional PUT.
const SINGLE_PUT_MAX: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct S3Config {
    /// e.g. `http://minio:9000`; empty for AWS.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Key prefix inside the bucket, e.g. `pdms/`.
    pub prefix: String,
    pub allow_http: bool,
}

/// Blobs in an S3-compatible bucket under `<prefix>sha256/ab/cd/<sha256>`.
///
/// Small blobs use a conditional `PUT` (`If-None-Match: *`), so an existing
/// object is never replaced. Large blobs check for existence first; since keys
/// are content hashes, a concurrent writer can only write identical bytes.
/// Enable bucket versioning or object lock for defence in depth.
#[derive(Debug, Clone)]
pub struct S3Store {
    store: Arc<dyn ObjectStore>,
    prefix: String,
}

fn os_err(e: object_store::Error) -> StorageError {
    StorageError::ObjectStore(e.to_string())
}

impl S3Store {
    pub fn new(cfg: &S3Config) -> Result<Self, StorageError> {
        let mut b = AmazonS3Builder::new()
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.region)
            .with_access_key_id(&cfg.access_key_id)
            .with_secret_access_key(&cfg.secret_access_key)
            .with_allow_http(cfg.allow_http);
        if !cfg.endpoint.is_empty() {
            b = b
                .with_endpoint(&cfg.endpoint)
                .with_virtual_hosted_style_request(false);
        }
        Ok(S3Store {
            store: Arc::new(b.build().map_err(os_err)?),
            prefix: cfg.prefix.clone(),
        })
    }

    fn key(&self, hash: BlobHash) -> ObjectPath {
        ObjectPath::from(format!("{}sha256/{}", self.prefix, hash.relative_path()))
    }
}

#[async_trait]
impl BlobStore for S3Store {
    fn backend(&self) -> &'static str {
        "s3"
    }

    async fn put_file(&self, hash: BlobHash, src: &Path, size: u64) -> Result<(), StorageError> {
        let key = self.key(hash);
        if let Some(existing) = self.size(hash).await? {
            return if existing == size {
                Ok(())
            } else {
                Err(StorageError::Corrupt(hash))
            };
        }
        let mut file = tokio::fs::File::open(src).await?;
        if size <= SINGLE_PUT_MAX {
            let mut buf = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
            file.read_to_end(&mut buf).await?;
            if crate::sha256(&buf) != hash {
                return Err(StorageError::Corrupt(hash));
            }
            let opts = PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            };
            return match self.store.put_opts(&key, PutPayload::from(buf), opts).await {
                Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => Ok(()),
                Err(e) => Err(os_err(e)),
            };
        }
        let upload = self.store.put_multipart(&key).await.map_err(os_err)?;
        let mut writer = WriteMultipart::new(upload);
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        let mut buf = vec![0u8; 8 * 1024 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            sha2::Digest::update(&mut hasher, &buf[..n]);
            writer.wait_for_capacity(4).await.map_err(os_err)?;
            writer.write(&buf[..n]);
        }
        if <[u8; 32]>::from(sha2::Digest::finalize(hasher)) != hash.0 {
            writer.abort().await.map_err(os_err)?;
            return Err(StorageError::Corrupt(hash));
        }
        writer.finish().await.map_err(os_err)?;
        Ok(())
    }

    async fn get(&self, hash: BlobHash) -> Result<ByteStream, StorageError> {
        match self.store.get(&self.key(hash)).await {
            Ok(r) => Ok(Box::pin(
                r.into_stream().map(|c| c.map_err(std::io::Error::other)),
            )),
            Err(object_store::Error::NotFound { .. }) => Err(StorageError::NotFound(hash)),
            Err(e) => Err(os_err(e)),
        }
    }

    async fn size(&self, hash: BlobHash) -> Result<Option<u64>, StorageError> {
        match self.store.head(&self.key(hash)).await {
            Ok(meta) => Ok(Some(meta.size)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(os_err(e)),
        }
    }
}
