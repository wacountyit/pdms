// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Configuration: defaults → TOML file → `PDMS_*` environment → `PDMS_*_FILE` secrets.
//!
//! Nested keys use `__` in environment variables: `PDMS_DATABASE__URL`.
//! Any key can instead be read from a file by appending `_FILE`:
//! `PDMS_SECRET_KEY_FILE=/run/secrets/pdms_secret_key`.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine;
use figment::Figment;
use figment::providers::{Format, Serialized, Toml};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use url::Url;

pub const DEFAULT_ORG_NAME: &str = "Policy and Document Management System";
pub const DEFAULT_CONFIG_PATH: &str = "config/pdms.toml";

/// A string that never appears in logs or `Debug` output.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_empty() {
            "\"\""
        } else {
            "\"[redacted]\""
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub org: OrgConfig,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    /// Base64 master secret (at least 32 bytes decoded). Session CSRF keys,
    /// cookie encryption, and TOTP secret encryption are derived from it.
    pub secret_key: Secret,
    pub log: LogConfig,
    pub worker: WorkerConfig,
    pub storage: StorageConfig,
    pub documents: DocumentsConfig,
    pub notifications: NotificationsConfig,
    pub email: EmailConfig,
    pub workflow: WorkflowConfig,
    pub api: ApiConfig,
    pub integrations: IntegrationsConfig,
    pub audit_forward: AuditForwardConfig,
    pub exports: ExportsConfig,
    pub comments: CommentsConfig,
    pub ocr: OcrConfig,
    /// Microsoft Graph as PADM itself (app-only, certificate). Used by the
    /// Entra user sync and, later, SharePoint storage.
    pub graph: GraphConfig,
    pub entra_sync: EntraSyncConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct OrgConfig {
    /// Shown in the UI title, email subjects, calendar names, and export manifests.
    pub name: String,
    /// IANA time zone used for display and for scheduling notifications.
    pub timezone: String,
    /// Where users can get this installation's source code. PDMS is licensed
    /// under the AGPL-3.0-or-later, whose section 13 requires offering the
    /// source to everyone who uses the system over a network; every page links
    /// to it through `/source`. Point it at your repository (including any
    /// changes you made).
    pub source_url: String,
}

impl Default for OrgConfig {
    fn default() -> Self {
        OrgConfig {
            name: DEFAULT_ORG_NAME.to_owned(),
            timezone: "America/New_York".to_owned(),
            source_url: String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    /// External URL users reach (used for OIDC redirect and Origin checks).
    pub public_url: Url,
    /// Proxies whose `X-Forwarded-For` header is trusted.
    pub trusted_proxies: Vec<IpNet>,
    /// Mark cookies `Secure` and send HSTS. Disable only for local development over HTTP.
    pub secure_cookies: bool,
    pub max_body_bytes: usize,
    /// Serve Prometheus metrics at `/metrics`.
    pub metrics_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: SocketAddr::from(([0, 0, 0, 0], 8080)),
            public_url: Url::parse("http://localhost:8080").unwrap_or_else(|_| unreachable!()),
            trusted_proxies: Vec::new(),
            secure_cookies: true,
            max_body_bytes: 2 * 1024 * 1024,
            metrics_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// Connection URL for the `pdms_app` role.
    pub url: Secret,
    /// Connection URL for the `pdms_owner` role; needed only by `pdms migrate`.
    pub owner_url: Secret,
    pub max_connections: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        DatabaseConfig {
            url: Secret::default(),
            owner_url: Secret::default(),
            max_connections: 16,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    pub session_idle_minutes: i64,
    pub session_absolute_hours: i64,
    /// Login attempts allowed per client IP per minute.
    pub login_attempts_per_minute: u32,
    pub local: LocalAuthConfig,
    pub oidc: OidcConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            session_idle_minutes: 30,
            session_absolute_hours: 12,
            login_attempts_per_minute: 10,
            local: LocalAuthConfig::default(),
            oidc: OidcConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct LocalAuthConfig {
    /// Break-glass local accounts. Keep enabled until OIDC is verified working.
    pub enabled: bool,
    /// Require a TOTP code for local login. Only the demo environment disables this.
    pub require_totp: bool,
    pub max_failed_attempts: i32,
    pub lockout_minutes: i64,
}

impl Default for LocalAuthConfig {
    fn default() -> Self {
        LocalAuthConfig {
            enabled: true,
            require_totp: true,
            max_failed_attempts: 5,
            lockout_minutes: 15,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct OidcConfig {
    pub enabled: bool,
    /// Entra: `https://login.microsoftonline.com/<tenant-id>/v2.0`
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Secret,
    pub scopes: Vec<String>,
    /// Claim holding the stable user identifier. Entra: `oid`. Other IdPs: usually `sub`.
    pub subject_claim: String,
    /// Claim holding group IDs mapped through `idp_group_mappings`.
    pub groups_claim: String,
    /// Create unknown users on first login (they start with no access).
    pub auto_provision: bool,
    /// Label for the sign-in button.
    pub display_name: String,
}

impl Default for OidcConfig {
    fn default() -> Self {
        OidcConfig {
            enabled: false,
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: Secret::default(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            subject_claim: "oid".to_owned(),
            groups_claim: "groups".to_owned(),
            auto_provision: true,
            display_name: "Microsoft".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct LogConfig {
    pub format: LogFormat,
    /// `tracing` env-filter directive, e.g. `info,pdms=debug`.
    pub filter: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            format: LogFormat::Json,
            filter: "info".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    Local,
    S3,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    /// Blob directory for the local backend.
    pub local_path: PathBuf,
    /// Scratch directory for uploads in progress (always local).
    pub spool_path: PathBuf,
    pub max_upload_bytes: u64,
    /// clamd address (`host:port`) to scan uploads; empty disables scanning.
    pub clamav_addr: String,
    /// When scanning is configured and clamd is unreachable, reject uploads (fail closed).
    pub clamav_required: bool,
    pub s3: S3StorageConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            backend: StorageBackend::Local,
            local_path: PathBuf::from("data/blobs"),
            spool_path: PathBuf::from("data/spool"),
            max_upload_bytes: 100 * 1024 * 1024,
            clamav_addr: String::new(),
            clamav_required: true,
            s3: S3StorageConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct S3StorageConfig {
    /// e.g. `http://minio:9000`; leave empty for AWS.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: Secret,
    /// Key prefix inside the bucket.
    pub prefix: String,
    pub allow_http: bool,
}

impl Default for S3StorageConfig {
    fn default() -> Self {
        S3StorageConfig {
            endpoint: String::new(),
            bucket: String::new(),
            region: "us-east-1".to_owned(),
            access_key_id: String::new(),
            secret_access_key: Secret::default(),
            prefix: String::new(),
            allow_http: false,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DocumentsConfig {
    /// Let anyone who can edit a document add new (uncurated) tags. Off by
    /// default: tags are kept by administrators (D-088).
    pub free_form_tags: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Local hour (0–23, in `org.timezone`) after which each day's reminders go out.
    pub send_hour: u32,
    /// An Active document becomes Renewal Due this many days before its renewal date.
    pub renewal_due_days: i64,
    /// Review and renewal tasks appear this many days before their due date.
    pub task_lead_days: i64,
    /// A missed, non-repeating reminder older than this many days is not sent late.
    pub stale_after_days: i64,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        NotificationsConfig {
            send_hour: 7,
            renewal_due_days: 90,
            task_lead_days: 90,
            stale_after_days: 30,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EmailTransport {
    /// Email deliveries are recorded as skipped.
    Disabled,
    Smtp,
    /// Write each message as an `.eml` file (development and testing).
    File,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    /// STARTTLS (Exchange Online: smtp.office365.com:587).
    Starttls,
    /// Implicit TLS (usually port 465).
    Tls,
    /// No encryption: only for a relay on a trusted network.
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct EmailConfig {
    pub transport: EmailTransport,
    /// e.g. `"Records System <pdms@example.org>"`
    pub from: String,
    pub reply_to: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: SmtpSecurity,
    /// Leave empty for an unauthenticated relay (e.g. an Exchange connector).
    pub smtp_username: String,
    pub smtp_password: Secret,
    pub file_dir: PathBuf,
}

impl Default for EmailConfig {
    fn default() -> Self {
        EmailConfig {
            transport: EmailTransport::Disabled,
            from: String::new(),
            reply_to: String::new(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_security: SmtpSecurity::Starttls,
            smtp_username: String::new(),
            smtp_password: Secret::default(),
            file_dir: PathBuf::from("data/mail"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkerConfig {
    pub concurrency: usize,
    pub poll_interval_ms: u64,
    pub lease_seconds: i64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            concurrency: 4,
            poll_interval_ms: 1000,
            lease_seconds: 300,
        }
    }
}

/// The REST API (`/api/v1`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ApiConfig {
    /// Accept API tokens. When off, `/api/v1` answers 404.
    pub enabled: bool,
    /// Longest lifetime a new token may have.
    pub max_token_days: i64,
    /// API request log entries older than this are purged.
    pub request_log_days: i64,
    /// Requests per minute allowed per token (excess requests get 429).
    pub requests_per_minute: u32,
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            enabled: true,
            max_token_days: 365,
            request_log_days: 90,
            requests_per_minute: 600,
        }
    }
}

/// Outbound events: webhooks and integration providers.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct IntegrationsConfig {
    /// Deliver events to configured webhooks.
    pub webhooks_enabled: bool,
    /// Allow plain-HTTP webhook URLs (testing only; use HTTPS in production).
    pub allow_http_webhooks: bool,
    /// Seconds to wait for a webhook endpoint to answer.
    pub webhook_timeout_seconds: u64,
    /// Delivery attempts before giving up (with exponential backoff).
    pub webhook_max_attempts: i32,
    /// Integration providers to enable, by key (built-in: `log`).
    pub providers: Vec<String>,
}

impl Default for IntegrationsConfig {
    fn default() -> Self {
        IntegrationsConfig {
            webhooks_enabled: true,
            allow_http_webhooks: false,
            webhook_timeout_seconds: 10,
            webhook_max_attempts: 8,
            providers: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditForwardTarget {
    None,
    /// Append each entry as a JSON line to `file_path`.
    File,
    /// Send each entry as an RFC 5424 message over UDP to `syslog_addr`.
    Syslog,
}

/// Copy audit entries to an external log system as they are written (the
/// database stays the system of record).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AuditForwardConfig {
    pub target: AuditForwardTarget,
    pub file_path: PathBuf,
    /// `host:port`, e.g. `siem.example.org:514`.
    pub syslog_addr: String,
    pub syslog_app_name: String,
}

impl Default for AuditForwardConfig {
    fn default() -> Self {
        AuditForwardConfig {
            target: AuditForwardTarget::None,
            file_path: PathBuf::from("/var/lib/pdms/audit-forward.jsonl"),
            syslog_addr: String::new(),
            syslog_app_name: "pdms".into(),
        }
    }
}

/// Records request packages.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ExportsConfig {
    pub max_documents: usize,
    /// Largest total size of the files in one package.
    pub max_bytes: u64,
}

impl Default for ExportsConfig {
    fn default() -> Self {
        ExportsConfig {
            max_documents: 500,
            max_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// Document comments.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CommentsConfig {
    /// Anyone who can view a document may comment on it. When false,
    /// commenting needs the `edit` permission.
    pub viewers_can_comment: bool,
}

impl Default for CommentsConfig {
    fn default() -> Self {
        CommentsConfig {
            viewers_can_comment: true,
        }
    }
}

/// Text extraction and OCR of uploaded files, and date suggestions.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct OcrConfig {
    /// Extract embedded text from PDFs, Word (.docx), and text files (built in).
    pub text_extraction: bool,
    /// OCR sidecar for scanned PDFs and images, e.g. `http://ocr:8080`. Empty = no OCR.
    pub sidecar_url: String,
    pub sidecar_timeout_seconds: u64,
    /// Files larger than this are not processed.
    pub max_bytes: u64,
    /// A PDF with fewer embedded characters than this is treated as scanned (OCR'd if available).
    pub min_text_chars: usize,
    /// Look for dates in extracted text and offer them for confirmation.
    pub suggest_dates: bool,
}

impl Default for OcrConfig {
    fn default() -> Self {
        OcrConfig {
            text_extraction: true,
            sidecar_url: String::new(),
            sidecar_timeout_seconds: 300,
            max_bytes: 50 * 1024 * 1024,
            min_text_chars: 100,
            suggest_dates: true,
        }
    }
}

/// Microsoft Graph with an app registration ("PADM Service") and a
/// certificate credential. All three of `tenant_id`, `client_id`, and
/// `certificate` are needed; with none of them set, Graph is off.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct GraphConfig {
    pub tenant_id: String,
    pub client_id: String,
    /// PEM: the certificate followed by its unencrypted RSA private key
    /// (`PRIVATE KEY` or `RSA PRIVATE KEY`). Supply it as a file:
    /// `PDMS_GRAPH__CERTIFICATE_FILE=/run/secrets/graph_certificate`.
    pub certificate: Secret,
    /// Overridable for tests and national clouds.
    pub authority_url: String,
    pub api_url: String,
    pub timeout_seconds: u64,
    /// Retries after HTTP 429/503/504 before giving up.
    pub max_retries: u32,
}

impl Default for GraphConfig {
    fn default() -> Self {
        GraphConfig {
            tenant_id: String::new(),
            client_id: String::new(),
            certificate: Secret::default(),
            authority_url: "https://login.microsoftonline.com".into(),
            api_url: "https://graph.microsoft.com/v1.0".into(),
            timeout_seconds: 60,
            max_retries: 4,
        }
    }
}

impl GraphConfig {
    pub fn is_configured(&self) -> bool {
        !self.tenant_id.is_empty() && !self.client_id.is_empty() && !self.certificate.is_empty()
    }
}

/// People and departments from Entra ID (D-086).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct EntraSyncConfig {
    pub enabled: bool,
    /// Daily run time, `HH:MM` in `[org] timezone`.
    pub run_at: String,
    /// Entra user property that names the department.
    pub attribute: String,
    /// Also sync guest (B2B) accounts. Off: guests cannot sign in.
    pub include_guests: bool,
}

impl Default for EntraSyncConfig {
    fn default() -> Self {
        EntraSyncConfig {
            enabled: false,
            run_at: "02:30".into(),
            attribute: "department".into(),
            include_guests: false,
        }
    }
}

impl EntraSyncConfig {
    pub fn run_at_time(&self) -> Option<chrono::NaiveTime> {
        chrono::NaiveTime::parse_from_str(self.run_at.trim(), "%H:%M").ok()
    }
}

/// `https`, or plain `http` to this machine (tests and local mocks).
fn https_or_loopback(s: &str) -> bool {
    Url::parse(s).is_ok_and(|u| u.scheme() == "https" || (u.scheme() == "http" && is_loopback(&u)))
}

/// The URL names this machine.
pub(crate) fn is_loopback(u: &Url) -> bool {
    matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

/// Review and approval workflow policy.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkflowConfig {
    /// Let a submitter approve (or give the legal sign-off on) their own
    /// submission. Off by default: separation of duties.
    pub allow_self_approval: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration: {0}")]
    Load(Box<figment::Error>),
    #[error("cannot read secret file {path} (from {var}): {source}")]
    SecretFile {
        var: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("configuration: {0}")]
    Invalid(String),
}

impl Config {
    /// Load from `path` (or `PDMS_CONFIG`, or `config/pdms.toml` if present) and the environment.
    pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        Self::load_from(path, &vars)
    }

    /// Like [`Config::load`] with an explicit environment, for tests.
    pub fn load_from(path: Option<&Path>, env: &[(String, String)]) -> Result<Config, ConfigError> {
        let path = path.map(Path::to_path_buf).or_else(|| {
            env.iter()
                .find(|(k, _)| k == "PDMS_CONFIG")
                .map(|(_, v)| PathBuf::from(v))
        });
        let mut fig = Figment::from(Serialized::defaults(Config::default()));
        match &path {
            Some(p) => fig = fig.merge(Toml::file_exact(p)),
            None if Path::new(DEFAULT_CONFIG_PATH).exists() => {
                fig = fig.merge(Toml::file_exact(DEFAULT_CONFIG_PATH));
            }
            None => {}
        }

        for (var, raw) in env {
            let Some(name) = var.strip_prefix("PDMS_") else {
                continue;
            };
            if name == "CONFIG" || name.starts_with("TEST_") {
                continue;
            }
            let (name, value) = match name.strip_suffix("_FILE") {
                Some(name) => {
                    let text =
                        std::fs::read_to_string(raw).map_err(|source| ConfigError::SecretFile {
                            var: var.clone(),
                            path: PathBuf::from(raw),
                            source,
                        })?;
                    let text = text.trim_end_matches(['\r', '\n']).to_owned();
                    (name, figment::value::Value::from(text))
                }
                None => (name, raw.parse().unwrap_or_else(|_| raw.as_str().into())),
            };
            let key = name.to_ascii_lowercase().replace("__", ".");
            fig = fig.merge(Serialized::default(&key, value));
        }

        let config: Config = fig.extract().map_err(|e| ConfigError::Load(Box::new(e)))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        if !(1..=3650).contains(&self.api.max_token_days) || self.api.request_log_days < 1 {
            return bad(
                "api.max_token_days must be 1–3650 and api.request_log_days at least 1".into(),
            );
        }
        if self.audit_forward.target == AuditForwardTarget::Syslog
            && self.audit_forward.syslog_addr.is_empty()
        {
            return bad("audit_forward.syslog_addr is required for the syslog target".into());
        }
        for p in &self.integrations.providers {
            if !crate::integrations::KNOWN_PROVIDERS.contains(&p.as_str()) {
                return bad(format!("unknown integration provider {p:?}"));
            }
        }
        if self.org.name.trim().is_empty() {
            return bad("org.name must not be empty".into());
        }
        if self.org.timezone.parse::<chrono_tz::Tz>().is_err() {
            return bad(format!(
                "org.timezone {:?} is not an IANA time zone",
                self.org.timezone
            ));
        }
        if self.auth.session_idle_minutes <= 0 || self.auth.session_absolute_hours <= 0 {
            return bad("auth session timeouts must be positive".into());
        }
        if self.auth.oidc.enabled
            && (self.auth.oidc.issuer_url.is_empty() || self.auth.oidc.client_id.is_empty())
        {
            return bad(
                "auth.oidc.issuer_url and auth.oidc.client_id are required when OIDC is enabled"
                    .into(),
            );
        }
        if self.storage.backend == StorageBackend::S3 && self.storage.s3.bucket.is_empty() {
            return bad("storage.s3.bucket is required for the s3 backend".into());
        }
        if self.notifications.send_hour > 23 {
            return bad("notifications.send_hour must be 0–23".into());
        }
        if self.email.transport == EmailTransport::Smtp
            && (self.email.smtp_host.is_empty() || self.email.from.is_empty())
        {
            return bad("email.smtp_host and email.from are required for SMTP".into());
        }
        if self.storage.max_upload_bytes == 0 {
            return bad("storage.max_upload_bytes must be positive".into());
        }
        if !self.auth.oidc.enabled && !self.auth.local.enabled {
            return bad("at least one of auth.oidc and auth.local must be enabled".into());
        }
        let g = &self.graph;
        let parts = [
            !g.tenant_id.is_empty(),
            !g.client_id.is_empty(),
            !g.certificate.is_empty(),
        ];
        if parts.iter().any(|p| *p) && !parts.iter().all(|p| *p) {
            return bad("graph needs tenant_id, client_id, and certificate together".into());
        }
        if !https_or_loopback(&g.authority_url) || !https_or_loopback(&g.api_url) {
            return bad("graph.authority_url and graph.api_url must be https URLs".into());
        }
        let e = &self.entra_sync;
        if e.enabled {
            if !g.is_configured() {
                return bad(
                    "entra_sync needs [graph] tenant_id, client_id, and certificate".into(),
                );
            }
            if self.auth.oidc.issuer_url.trim().is_empty() {
                return bad(
                    "entra_sync needs auth.oidc.issuer_url so synced people can sign in".into(),
                );
            }
            if self.auth.oidc.subject_claim != "oid" {
                return bad(
                    "entra_sync needs auth.oidc.subject_claim = \"oid\": synced accounts are keyed by object ID"
                        .into(),
                );
            }
        }
        if e.run_at_time().is_none() {
            return bad(format!("entra_sync.run_at {:?} must be HH:MM", e.run_at));
        }
        let attr_ok = e.attribute.starts_with(|c: char| c.is_ascii_alphabetic())
            && e.attribute
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !attr_ok {
            return bad(format!(
                "entra_sync.attribute {:?} must be one Entra property name",
                e.attribute
            ));
        }
        Ok(())
    }

    /// Decode and check the master secret. Required by `serve` and `worker`.
    pub fn master_key(&self) -> Result<Vec<u8>, ConfigError> {
        if self.secret_key.is_empty() {
            return Err(ConfigError::Invalid(
                "secret_key is not set (PDMS_SECRET_KEY or PDMS_SECRET_KEY_FILE)".into(),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.secret_key.expose().trim())
            .map_err(|_| ConfigError::Invalid("secret_key must be base64".into()))?;
        if bytes.len() < 32 {
            return Err(ConfigError::Invalid(
                "secret_key must decode to at least 32 bytes (use `pdms gen-secret`)".into(),
            ));
        }
        Ok(bytes)
    }

    pub fn timezone(&self) -> chrono_tz::Tz {
        self.org.timezone.parse().unwrap_or(chrono_tz::UTC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn defaults_are_valid() {
        let c = Config::load_from(None, &[]).unwrap();
        assert_eq!(c.org.name, DEFAULT_ORG_NAME);
        assert!(c.auth.local.require_totp);
    }

    #[test]
    fn secret_files_override() {
        let dir = std::env::temp_dir().join(format!("pdms-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("dburl");
        std::fs::write(&f, "postgres://x\n").unwrap();
        let c = Config::load_from(
            None,
            &env(&[("PDMS_DATABASE__URL_FILE", f.to_str().unwrap())]),
        )
        .unwrap();
        assert_eq!(c.database.url.expose(), "postgres://x");
        assert_eq!(format!("{:?}", c.database.url), "\"[redacted]\"");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_bad_timezone_and_short_key() {
        let mut c = Config::default();
        c.org.timezone = "Mars/Olympus".into();
        assert!(c.validate().is_err());
        let c = Config {
            secret_key: Secret::new("c2hvcnQ="),
            ..Config::default()
        };
        assert!(c.master_key().is_err());
    }

    #[test]
    fn toml_file_is_read() {
        let dir = std::env::temp_dir().join(format!("pdms-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pdms.toml");
        std::fs::write(&f, "[org]\nname = \"Example Org PDMS\"\n").unwrap();
        let c = Config::load_from(Some(&f), &[]).unwrap();
        assert_eq!(c.org.name, "Example Org PDMS");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn graph_and_entra_sync_defaults_and_validation() {
        let c = Config::default();
        assert!(!c.graph.is_configured());
        assert_eq!(c.graph.authority_url, "https://login.microsoftonline.com");
        assert_eq!(c.graph.api_url, "https://graph.microsoft.com/v1.0");
        assert!(!c.entra_sync.enabled);
        assert_eq!(c.entra_sync.run_at, "02:30");
        assert_eq!(c.entra_sync.attribute, "department");
        assert!(!c.entra_sync.include_guests);

        let mut c = Config::default();
        c.graph.tenant_id = "t".into();
        assert!(c.validate().is_err(), "partial [graph] is rejected");
        c.graph.client_id = "c".into();
        c.graph.certificate = Secret::new("-----BEGIN CERTIFICATE-----");
        assert!(c.validate().is_ok());
        assert!(c.graph.is_configured());
        c.graph.api_url = "http://graph.example.com/v1.0".into();
        assert!(c.validate().is_err(), "plain http only to loopback");
        c.graph.api_url = "http://127.0.0.1:9999/v1.0".into();
        assert!(c.validate().is_ok());

        let mut c = Config::default();
        c.entra_sync.enabled = true;
        assert!(c.validate().is_err(), "needs [graph] and an OIDC issuer");
        c.graph.tenant_id = "t".into();
        c.graph.client_id = "c".into();
        c.graph.certificate = Secret::new("x");
        c.auth.oidc.issuer_url = "https://login.microsoftonline.com/t/v2.0".into();
        assert!(c.validate().is_ok());
        c.auth.oidc.subject_claim = "sub".into();
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("subject_claim"), "{err}");
        c.auth.oidc.subject_claim = "oid".into();
        assert!(c.validate().is_ok());
        c.entra_sync.run_at = "25:00".into();
        assert!(c.validate().is_err());
        c.entra_sync.run_at = "02:30".into();
        assert_eq!(
            c.entra_sync.run_at_time(),
            chrono::NaiveTime::from_hms_opt(2, 30, 0)
        );
        c.entra_sync.attribute = "department,mail".into();
        assert!(c.validate().is_err(), "the attribute goes into $select");
    }

    #[test]
    fn graph_certificate_is_read_from_a_file() {
        let dir = std::env::temp_dir().join(format!("pdms-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("graph_certificate");
        std::fs::write(
            &f,
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let c = Config::load_from(
            None,
            &env(&[
                ("PDMS_GRAPH__TENANT_ID", "t"),
                ("PDMS_GRAPH__CLIENT_ID", "c"),
                ("PDMS_GRAPH__CERTIFICATE_FILE", f.to_str().unwrap()),
            ]),
        )
        .unwrap();
        assert!(
            c.graph
                .certificate
                .expose()
                .starts_with("-----BEGIN CERTIFICATE-----\nAAAA")
        );
        assert_eq!(format!("{:?}", c.graph.certificate), "\"[redacted]\"");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
