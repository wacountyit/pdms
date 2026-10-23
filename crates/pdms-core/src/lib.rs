// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! PDMS domain services.
//!
//! Every mutating function takes a [`Ctx`] (who is acting), checks
//! authorization through `pdms-authz`, and writes its audit entries in the
//! same transaction as the change.

pub mod acl;
pub mod activity;
pub mod api_tokens;
pub mod appointments;
pub mod audit_log;
pub mod auth;
pub mod branding;
pub mod calendar;
pub mod comments;
pub mod config;
pub mod context;
pub mod crypto;
pub mod db;
pub mod demo;
pub mod directory;
pub mod documents;
pub mod error;
pub mod exports;
pub mod graph;
pub mod integrations;
pub mod integrity;
pub mod jobs;
pub mod maintenance;
pub mod model;
pub mod notifications;
pub mod ocr;
pub mod reports;
pub mod reviews;
pub mod schedule;
pub mod sharepoint;
pub mod signatures;
pub mod workflow;

use std::sync::Arc;

use pdms_authz::Policy;
use pdms_storage::BlobStore;
use pdms_storage::scan::{ClamdScanner, VirusScanner};
use sqlx::PgPool;

use crate::config::StorageBackend;

pub use config::Config;
pub use context::{ClientInfo, Ctx};
pub use error::{CoreError, CoreResult};

/// Shared application services.
#[derive(Clone)]
pub struct Core {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub keys: crypto::Keys,
    pub policy: Policy,
    pub storage: Arc<dyn BlobStore>,
    pub scanner: Option<Arc<dyn VirusScanner>>,
    pub mailer: Arc<dyn notifications::mailer::Mailer>,
    /// Sends signature requests (manual tracking unless an integration is configured).
    pub signatures: Arc<dyn signatures::SignatureProvider>,
    /// OCR for scanned files (`None` unless an OCR sidecar is configured).
    pub ocr: Option<Arc<dyn ocr::OcrProvider>>,
    /// Document files in SharePoint (`[storage] backend = "sharepoint"`).
    pub sharepoint: Option<sharepoint::SharePoint>,
    /// Microsoft Graph as PDMS itself (`None` unless `[graph]` is configured).
    pub graph: Option<graph::GraphClient>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").finish_non_exhaustive()
    }
}

impl Core {
    pub fn new(db: PgPool, config: Config) -> CoreResult<Core> {
        let master = config
            .master_key()
            .map_err(|e| CoreError::validation(e.to_string()))?;
        let storage = open_storage(&config)?;
        let scanner: Option<Arc<dyn VirusScanner>> =
            (!config.storage.clamav_addr.is_empty()).then(|| {
                Arc::new(ClamdScanner {
                    addr: config.storage.clamav_addr.clone(),
                    timeout: std::time::Duration::from_secs(120),
                }) as Arc<dyn VirusScanner>
            });
        let mailer = notifications::mailer::from_config(&config.email)
            .map_err(|e| CoreError::validation(format!("email: {e}")))?;
        let graph = if config.graph.is_configured() {
            Some(
                graph::GraphClient::new(&config.graph)
                    .map_err(|e| CoreError::validation(format!("graph: {e}")))?,
            )
        } else {
            None
        };
        let sharepoint = match config.storage.backend {
            StorageBackend::SharePoint => {
                let graph = graph.clone().ok_or_else(|| {
                    CoreError::validation(
                        "storage.backend = \"sharepoint\" needs the [graph] section (tenant_id, client_id, certificate)",
                    )
                })?;
                Some(sharepoint::SharePoint::new(graph, &config.sharepoint)?)
            }
            _ => None,
        };
        let allow_self_approval = config.workflow.allow_self_approval;
        Ok(Core {
            graph,
            sharepoint,
            mailer,
            signatures: Arc::new(signatures::ManualSignatures),
            ocr: ocr::from_config(&config.ocr),
            db,
            keys: crypto::Keys::derive(&master),
            config: Arc::new(config),
            policy: Policy {
                allow_self_approval,
                ..Policy::default()
            },
            storage,
            scanner,
        })
    }

    pub async fn connect(config: Config) -> CoreResult<Core> {
        let db = db::connect(&config).await?;
        Core::new(db, config)
    }
}

fn open_storage(config: &Config) -> CoreResult<Arc<dyn BlobStore>> {
    let c = &config.storage;
    Ok(match c.backend {
        StorageBackend::Local | StorageBackend::SharePoint => Arc::new(
            pdms_storage::LocalStore::open(&c.local_path)
                .map_err(|e| CoreError::validation(format!("storage.local_path: {e}")))?,
        ),
        StorageBackend::S3 => Arc::new(
            pdms_storage::S3Store::new(&pdms_storage::S3Config {
                endpoint: c.s3.endpoint.clone(),
                bucket: c.s3.bucket.clone(),
                region: c.s3.region.clone(),
                access_key_id: c.s3.access_key_id.clone(),
                secret_access_key: c.s3.secret_access_key.expose().to_owned(),
                prefix: c.s3.prefix.clone(),
                allow_http: c.s3.allow_http,
            })
            .map_err(|e| CoreError::validation(format!("storage.s3: {e}")))?,
        ),
    })
}
