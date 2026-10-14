// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Virus scanning hook. `ClamdScanner` speaks the clamd INSTREAM protocol over TCP.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    Infected(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("virus scanner unavailable: {0}")]
    Unavailable(String),
    #[error("virus scanner error: {0}")]
    Scanner(String),
}

#[async_trait]
pub trait VirusScanner: Send + Sync + fmt::Debug {
    fn name(&self) -> &'static str;
    async fn scan(&self, path: &Path) -> Result<ScanVerdict, ScanError>;
}

/// clamd over TCP (e.g. `clamav:3310`). Raise clamd's `StreamMaxLength` to at
/// least the upload size limit, or larger files will fail to scan.
#[derive(Debug, Clone)]
pub struct ClamdScanner {
    pub addr: String,
    pub timeout: Duration,
}

#[async_trait]
impl VirusScanner for ClamdScanner {
    fn name(&self) -> &'static str {
        "clamav"
    }

    async fn scan(&self, path: &Path) -> Result<ScanVerdict, ScanError> {
        tokio::time::timeout(self.timeout, self.scan_inner(path))
            .await
            .map_err(|_| ScanError::Unavailable("timed out".into()))?
    }
}

impl ClamdScanner {
    async fn scan_inner(&self, path: &Path) -> Result<ScanVerdict, ScanError> {
        let unavailable = |e: std::io::Error| ScanError::Unavailable(e.to_string());
        let mut conn = tokio::net::TcpStream::connect(&self.addr)
            .await
            .map_err(unavailable)?;
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| ScanError::Scanner(e.to_string()))?;
        conn.write_all(b"zINSTREAM\0").await.map_err(unavailable)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| ScanError::Scanner(e.to_string()))?;
            let len = u32::try_from(n).map_err(|e| ScanError::Scanner(e.to_string()))?;
            conn.write_all(&len.to_be_bytes())
                .await
                .map_err(unavailable)?;
            if n == 0 {
                break;
            }
            conn.write_all(&buf[..n]).await.map_err(unavailable)?;
        }
        let mut reply = Vec::new();
        conn.read_to_end(&mut reply).await.map_err(unavailable)?;
        parse_reply(&reply)
    }
}

fn parse_reply(reply: &[u8]) -> Result<ScanVerdict, ScanError> {
    let text = String::from_utf8_lossy(reply);
    let text = text.trim_end_matches(['\0', '\n']).trim();
    let body = text.strip_prefix("stream:").map(str::trim).unwrap_or(text);
    if body == "OK" {
        Ok(ScanVerdict::Clean)
    } else if let Some(name) = body.strip_suffix("FOUND") {
        Ok(ScanVerdict::Infected(name.trim().to_owned()))
    } else {
        Err(ScanError::Scanner(body.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clamd_replies() {
        assert_eq!(parse_reply(b"stream: OK\0").unwrap(), ScanVerdict::Clean);
        assert_eq!(
            parse_reply(b"stream: Eicar-Test-Signature FOUND\0").unwrap(),
            ScanVerdict::Infected("Eicar-Test-Signature".into())
        );
        assert!(parse_reply(b"INSTREAM size limit exceeded. ERROR\0").is_err());
    }

    /// A fake clamd that checks the framing and flags files containing "EICAR".
    #[tokio::test]
    async fn speaks_instream_protocol() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut cmd = [0u8; 10];
                    s.read_exact(&mut cmd).await.unwrap();
                    assert_eq!(&cmd, b"zINSTREAM\0");
                    let mut data = Vec::new();
                    loop {
                        let mut len = [0u8; 4];
                        s.read_exact(&mut len).await.unwrap();
                        let n = u32::from_be_bytes(len) as usize;
                        if n == 0 {
                            break;
                        }
                        let mut chunk = vec![0u8; n];
                        s.read_exact(&mut chunk).await.unwrap();
                        data.extend(chunk);
                    }
                    let reply: &[u8] = if data.windows(5).any(|w| w == b"EICAR") {
                        b"stream: Eicar-Test-Signature FOUND\0"
                    } else {
                        b"stream: OK\0"
                    };
                    s.write_all(reply).await.unwrap();
                });
            }
        });
        let dir = std::env::temp_dir().join(format!("pdms-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let clean = dir.join("clean");
        let bad = dir.join("bad");
        std::fs::write(&clean, vec![b'a'; 200_000]).unwrap();
        std::fs::write(&bad, b"xxEICARxx").unwrap();
        let s = ClamdScanner {
            addr: addr.clone(),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(s.scan(&clean).await.unwrap(), ScanVerdict::Clean);
        assert!(matches!(
            s.scan(&bad).await.unwrap(),
            ScanVerdict::Infected(_)
        ));
        let down = ClamdScanner {
            addr: "127.0.0.1:1".into(),
            timeout: Duration::from_secs(2),
        };
        assert!(matches!(
            down.scan(&clean).await,
            Err(ScanError::Unavailable(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
