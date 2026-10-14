#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Real clamd (official image, bundled signatures). Slow to start, so ignored
//! by default: `cargo test -p pdms-storage --test clamav -- --ignored`.

use std::time::Duration;

use pdms_storage::scan::{ClamdScanner, ScanVerdict, VirusScanner};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

/// The standard antivirus test string (harmless by design).
const EICAR: &str = r"X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";

#[tokio::test]
#[ignore = "starts a ClamAV container (about a minute)"]
async fn real_clamd_detects_eicar_and_passes_clean_files() {
    let c = GenericImage::new("clamav/clamav", "stable")
        .with_exposed_port(3310.tcp())
        .with_wait_for(WaitFor::Nothing)
        .with_env_var("CLAMAV_NO_FRESHCLAMD", "true")
        .with_env_var("CLAMAV_NO_MILTERD", "true")
        .with_startup_timeout(Duration::from_secs(300))
        .start()
        .await
        .expect("start clamav");
    let port = c.get_host_port_ipv4(3310).await.unwrap();
    let scanner = ClamdScanner {
        addr: format!("127.0.0.1:{port}"),
        timeout: Duration::from_secs(60),
    };

    let dir = std::env::temp_dir().join(format!("pdms-clam-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let clean = dir.join("clean.txt");
    let bad = dir.join("eicar.txt");
    std::fs::write(&clean, "An ordinary partnership agreement.\n").unwrap();
    std::fs::write(&bad, EICAR).unwrap();

    // clamd takes a while to load signatures.
    let mut verdict = None;
    for _ in 0..120 {
        if let Ok(v) = scanner.scan(&clean).await {
            verdict = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        verdict,
        Some(ScanVerdict::Clean),
        "clamd did not become ready"
    );
    match scanner.scan(&bad).await.unwrap() {
        ScanVerdict::Infected(name) => assert!(name.to_lowercase().contains("eicar"), "{name}"),
        other => panic!("EICAR not detected: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}
