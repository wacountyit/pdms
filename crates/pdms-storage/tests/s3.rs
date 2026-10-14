#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! S3 backend against a real S3-compatible server (SeaweedFS S3 gateway).

use futures::StreamExt;
use pdms_storage::{BlobStore, S3Config, S3Store, StorageError, sha256};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

/// Create the bucket with a raw HTTP PUT (SeaweedFS accepts unsigned
/// requests when no identities are configured). True once the server answers.
async fn create_bucket(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    let req = "PUT /pdms-test HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    if s.write_all(req.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    buf.starts_with(b"HTTP/1.1 200") || buf.starts_with(b"HTTP/1.1 409")
}

async fn read_all(mut s: pdms_storage::ByteStream) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    out
}

#[tokio::test]
async fn s3_store_round_trip_and_never_overwrites() {
    let minio = GenericImage::new("chrislusf/seaweedfs", "latest")
        .with_exposed_port(8333.tcp())
        .with_wait_for(WaitFor::Nothing)
        .with_cmd([
            "server",
            "-s3",
            "-dir=/data",
            "-s3.port=8333",
            "-master.volumeSizeLimitMB=64",
        ])
        .start()
        .await
        .expect("start seaweedfs");
    let port = minio.get_host_port_ipv4(8333).await.unwrap();
    let store = S3Store::new(&S3Config {
        endpoint: format!("http://127.0.0.1:{port}"),
        bucket: "pdms-test".into(),
        region: "us-east-1".into(),
        access_key_id: "pdmsadmin".into(),
        secret_access_key: "pdmsadmin-secret".into(),
        prefix: "blobs/".into(),
        allow_http: true,
    })
    .unwrap();

    let dir = std::env::temp_dir().join(format!("pdms-s3-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let small = dir.join("small");
    std::fs::write(&small, b"partnership agreement").unwrap();
    let h = sha256(b"partnership agreement");

    // Retry until the server answers, creating the bucket first.
    let mut ok = false;
    for _ in 0..60 {
        if create_bucket(port).await && store.put_file(h, &small, 21).await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(ok, "S3 server did not become ready");
    store.put_file(h, &small, 21).await.unwrap(); // idempotent
    assert_eq!(
        read_all(store.get(h).await.unwrap()).await,
        b"partnership agreement"
    );
    assert_eq!(store.size(h).await.unwrap(), Some(21));
    assert!(store.verify(h).await.unwrap());

    // Content that does not match the claimed hash is refused.
    let wrong = sha256(b"something else");
    assert!(matches!(
        store.put_file(wrong, &small, 21).await,
        Err(StorageError::Corrupt(_))
    ));
    assert_eq!(store.size(wrong).await.unwrap(), None);

    // Large files go through multipart upload.
    let big_bytes: Vec<u8> = (0..20 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let big = dir.join("big");
    std::fs::write(&big, &big_bytes).unwrap();
    let hb = sha256(&big_bytes);
    store
        .put_file(hb, &big, big_bytes.len() as u64)
        .await
        .unwrap();
    assert_eq!(
        read_all(store.get(hb).await.unwrap()).await.len(),
        big_bytes.len()
    );
    assert!(store.verify(hb).await.unwrap());

    assert!(matches!(
        store.get(sha256(b"missing")).await,
        Err(StorageError::NotFound(_))
    ));
    std::fs::remove_dir_all(dir).unwrap();
}
