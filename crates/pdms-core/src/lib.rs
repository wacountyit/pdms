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
pub mod settings;
pub mod sharepoint;
pub mod signatures;
pub mod workflow;

use std::sync::{Arc, RwLock};

use pdms_authz::Policy;
use pdms_storage::BlobStore;
use pdms_storage::scan::{ClamdScanner, VirusScanner};
use sqlx::PgPool;

use crate::config::StorageBackend;

pub use config::Config;
pub use context::{ClientInfo, Ctx};
pub use error::{CoreError, CoreResult};

/// Shared application services.
///
/// The settings an administrator can change at run time (S1), and the
/// services built from them, sit in one swappable [`Live`] snapshot: read
/// them through [`Core::config`], [`Core::sharepoint`], and the other
/// accessors, never keep them across an `await` you don't control.
#[derive(Clone)]
pub struct Core {
    pub db: PgPool,
    pub keys: crypto::Keys,
    pub policy: Policy,
    pub storage: Arc<dyn BlobStore>,
    /// Sends signature requests (manual tracking unless an integration is configured).
    pub signatures: Arc<dyn signatures::SignatureProvider>,
    /// The configuration as loaded from the file and environment at start.
    file_config: Arc<Config>,
    live: Arc<RwLock<Arc<Live>>>,
}

/// The effective configuration and the services built from it, swapped as
/// one when an administrator changes a setting.
#[derive(Clone)]
pub struct Live {
    pub config: Arc<Config>,
    /// Microsoft Graph as PDMS itself (`None` unless `[graph]` is configured).
    pub graph: Option<graph::GraphClient>,
    /// Document files in SharePoint (`[storage] backend = "sharepoint"`).
    pub sharepoint: Option<sharepoint::SharePoint>,
    pub mailer: Arc<dyn notifications::mailer::Mailer>,
    /// OCR for scanned files (`None` unless an OCR sidecar is configured).
    pub ocr: Option<Arc<dyn ocr::OcrProvider>>,
    pub scanner: Option<Arc<dyn VirusScanner>>,
    /// The `settings_version` this snapshot was built from (0: file only).
    pub version: i64,
}

impl Live {
    /// Build the services for `config`. Fails without side effects, so a
    /// setting that can't take effect is refused before anything is saved.
    pub fn build(config: Config, version: i64) -> CoreResult<Live> {
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
        Ok(Live {
            graph,
            sharepoint,
            mailer,
            ocr: ocr::from_config(&config.ocr),
            scanner,
            config: Arc::new(config),
            version,
        })
    }
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
        let allow_self_approval = config.workflow.allow_self_approval;
        let live = Live::build(config.clone(), 0)?;
        Ok(Core {
            signatures: Arc::new(signatures::ManualSignatures),
            db,
            keys: crypto::Keys::derive(&master),
            file_config: Arc::new(config),
            live: Arc::new(RwLock::new(Arc::new(live))),
            policy: Policy {
                allow_self_approval,
                ..Policy::default()
            },
            storage,
        })
    }

    pub async fn connect(config: Config) -> CoreResult<Core> {
        let db = db::connect(&config).await?;
        Core::new(db, config)
    }

    /// The current snapshot: one consistent set of settings and services.
    pub fn live(&self) -> Arc<Live> {
        match self.live.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Replace the snapshot (settings changed).
    pub(crate) fn swap_live(&self, next: Live) {
        let next = Arc::new(next);
        match self.live.write() {
            Ok(mut g) => *g = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// Apply the settings stored in the database, when they changed since
    /// this snapshot was built (S1). Returns whether anything changed. When
    /// the new settings can't be built, the current snapshot stays in place.
    pub async fn reload(&self) -> CoreResult<bool> {
        let version = settings::current_version(&self.db).await?;
        if version == self.live().version {
            return Ok(false);
        }
        let overrides = settings::load_overrides(&self.db, &self.keys).await?;
        let config = settings::effective(&self.file_config, &overrides)?;
        let next = Live::build(config, version)?;
        self.swap_live(next);
        tracing::info!(version, "settings applied");
        Ok(true)
    }

    /// The effective configuration.
    pub fn config(&self) -> Arc<Config> {
        self.live().config.clone()
    }

    /// The configuration as loaded from the file and environment at start.
    pub fn file_config(&self) -> Arc<Config> {
        self.file_config.clone()
    }

    pub fn graph(&self) -> Option<graph::GraphClient> {
        self.live().graph.clone()
    }

    pub fn sharepoint(&self) -> Option<sharepoint::SharePoint> {
        self.live().sharepoint.clone()
    }

    pub fn mailer(&self) -> Arc<dyn notifications::mailer::Mailer> {
        self.live().mailer.clone()
    }

    pub fn ocr(&self) -> Option<Arc<dyn ocr::OcrProvider>> {
        self.live().ocr.clone()
    }

    pub fn scanner(&self) -> Option<Arc<dyn VirusScanner>> {
        self.live().scanner.clone()
    }

    /// Use another mailer (tests; the same snapshot otherwise).
    pub fn set_mailer(&self, mailer: Arc<dyn notifications::mailer::Mailer>) {
        let mut next = (*self.live()).clone();
        next.mailer = mailer;
        self.swap_live(next);
    }

    /// Use another OCR provider (tests; the same snapshot otherwise).
    pub fn set_ocr(&self, ocr: Option<Arc<dyn ocr::OcrProvider>>) {
        let mut next = (*self.live()).clone();
        next.ocr = ocr;
        self.swap_live(next);
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
