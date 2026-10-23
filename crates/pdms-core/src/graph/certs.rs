// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Certificates for the "PDMS Service" app registration, made in PDMS (S1,
//! D-113). The private key is stored encrypted in `app_secrets`
//! (`graph.certificate.<id>`); administrators download only the public
//! certificate, to upload to the app registration.
//!
//! The first certificate is `active` at once, and used as soon as the tenant
//! and client IDs are saved. A renewal waits as `pending` until an
//! administrator chooses to use it, so the new certificate can be uploaded to
//! Entra first: no outage. A
//! certificate in the configuration file (`secrets/graph_certificate`) wins
//! over all of these (D-111).

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category, Outcome};
use pdms_authz::Capability;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::cert;
use super::{GraphClient, GraphError};
use crate::Core;
use crate::config::{GraphConfig, Secret};
use crate::context::Ctx;
use crate::crypto::Keys;
use crate::error::{CoreError, CoreResult};
use crate::notifications::actions::Notice;

/// The certificate's lifetime: two years, as Entra recommends at most.
pub const VALID_DAYS: u32 = 730;
/// Administrators are reminded this many days before the active one expires.
pub const REMIND_DAYS: i64 = 30;
pub const EXPIRY_JOB: &str = "graph.certificate_expiry";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CertificateInfo {
    pub id: Uuid,
    pub thumbprint_sha256: String,
    pub thumbprint_sha1: String,
    pub not_after: DateTime<Utc>,
    /// `pending` or `active` (retired ones are not listed).
    pub state: String,
    pub created_at: DateTime<Utc>,
}

fn secret_key(id: Uuid) -> String {
    format!("graph.certificate.{id}")
}

/// The certificates in use or waiting, newest first.
pub async fn list(db: &PgPool) -> CoreResult<Vec<CertificateInfo>> {
    Ok(sqlx::query_as!(
        CertificateInfo,
        "SELECT id, thumbprint_sha256, thumbprint_sha1, not_after, state, created_at
           FROM graph_certificates WHERE state <> 'retired' ORDER BY created_at DESC"
    )
    .fetch_all(db)
    .await?)
}

/// The certificate PDMS signs in with (certificate and key, PEM): the active one.
pub async fn effective_pem(db: &PgPool, keys: &Keys) -> CoreResult<Option<String>> {
    let id =
        sqlx::query_scalar!("SELECT id FROM graph_certificates WHERE state = 'active' LIMIT 1")
            .fetch_optional(db)
            .await?;
    match id {
        Some(id) => load_pem(db, keys, id).await.map(Some),
        None => Ok(None),
    }
}

/// The newest certificate (a pending renewal first): what a connection test
/// tries, so a renewal can be tested before it is used.
async fn newest_pem(db: &PgPool, keys: &Keys) -> CoreResult<Option<String>> {
    let id = sqlx::query_scalar!(
        "SELECT id FROM graph_certificates WHERE state IN ('active', 'pending')
          ORDER BY created_at DESC LIMIT 1"
    )
    .fetch_optional(db)
    .await?;
    match id {
        Some(id) => load_pem(db, keys, id).await.map(Some),
        None => Ok(None),
    }
}

async fn load_pem(db: &PgPool, keys: &Keys, id: Uuid) -> CoreResult<String> {
    let key = secret_key(id);
    let ciphertext = sqlx::query_scalar!("SELECT ciphertext FROM app_secrets WHERE key = $1", key)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| CoreError::internal(format!("certificate {id} has no stored key")))?;
    let plain = keys
        .decrypt_setting(&key, &ciphertext)
        .map_err(|_| CoreError::internal(format!("certificate {id} cannot be decrypted")))?;
    String::from_utf8(plain).map_err(CoreError::internal)
}

fn event(ctx: &Ctx, action: &str, c: &CertificateInfo) -> AuditEvent {
    AuditEvent::new(
        Category::Admin,
        action,
        ctx.actor.clone(),
        "graph_certificate",
    )
    .object(c.id, None)
    .context("thumbprint_sha256", c.thumbprint_sha256.as_str())
    .context("thumbprint_sha1", c.thumbprint_sha1.as_str())
    .context("not_after", c.not_after.to_rfc3339())
}

async fn info(conn: &mut sqlx::PgConnection, id: Uuid) -> CoreResult<CertificateInfo> {
    sqlx::query_as!(
        CertificateInfo,
        "SELECT id, thumbprint_sha256, thumbprint_sha1, not_after, state, created_at
           FROM graph_certificates WHERE id = $1",
        id
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(CoreError::NotFound)
}

/// Make a new certificate (requires `ManageSettings`). The first one is
/// active at once (used once the tenant and client IDs are saved); later
/// ones wait as pending until [`activate`].
pub async fn generate(core: &Core, ctx: &Ctx) -> CoreResult<CertificateInfo> {
    ctx.require(Capability::ManageSettings)?;
    let g = cert::generate("PDMS Service", VALID_DAYS).map_err(CoreError::internal)?;
    let der = cert::certificate_der(&g.certificate_pem).map_err(CoreError::internal)?;
    let id = Uuid::now_v7();
    let ciphertext = core
        .keys
        .encrypt_setting(&secret_key(id), g.pem.as_bytes())
        .map_err(CoreError::internal)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO graph_certificates (id, thumbprint_sha256, thumbprint_sha1, not_after, state, created_by)
         VALUES ($1, $2, $3, $4,
                 CASE WHEN EXISTS (SELECT 1 FROM graph_certificates WHERE state = 'active')
                      THEN 'pending' ELSE 'active' END, $5)",
        id,
        g.thumbprint,
        cert::sha1_thumbprint(&der),
        g.not_after,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO app_secrets (key, ciphertext, updated_by) VALUES ($1, $2, $3)",
        secret_key(id),
        ciphertext,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    let c = info(&mut tx, id).await?;
    crate::settings::bump_version(&mut tx).await?;
    pdms_audit::append(&mut tx, vec![event(ctx, "graph.certificate_created", &c)]).await?;
    tx.commit().await?;
    reload_quietly(core).await;
    Ok(c)
}

/// Use a pending certificate from now on; the active one is retired
/// (requires `ManageSettings`). Upload it to the app registration first.
pub async fn activate(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let mut tx = core.db.begin().await?;
    let c = info(&mut tx, id).await?;
    if c.state != "pending" {
        return Err(CoreError::Conflict(
            "Only a certificate waiting to be used can be switched to.".into(),
        ));
    }
    let old: Vec<Uuid> = sqlx::query_scalar!(
        "UPDATE graph_certificates SET state = 'retired' WHERE state = 'active' RETURNING id"
    )
    .fetch_all(&mut *tx)
    .await?;
    for o in &old {
        sqlx::query!("DELETE FROM app_secrets WHERE key = $1", secret_key(*o))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query!(
        "UPDATE graph_certificates SET state = 'active' WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    crate::settings::bump_version(&mut tx).await?;
    pdms_audit::append(&mut tx, vec![event(ctx, "graph.certificate_activated", &c)]).await?;
    tx.commit().await?;
    reload_quietly(core).await;
    Ok(())
}

/// Discard a certificate that isn't in use (requires `ManageSettings`). Its
/// private key is deleted.
pub async fn retire(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageSettings)?;
    let mut tx = core.db.begin().await?;
    let c = info(&mut tx, id).await?;
    match c.state.as_str() {
        "pending" => {}
        "active" => {
            return Err(CoreError::Conflict(
                "The certificate in use can't be discarded. Make a new one and switch to it first."
                    .into(),
            ));
        }
        _ => return Err(CoreError::NotFound),
    }
    sqlx::query!(
        "UPDATE graph_certificates SET state = 'retired' WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM app_secrets WHERE key = $1", secret_key(id))
        .execute(&mut *tx)
        .await?;
    crate::settings::bump_version(&mut tx).await?;
    pdms_audit::append(&mut tx, vec![event(ctx, "graph.certificate_retired", &c)]).await?;
    tx.commit().await?;
    reload_quietly(core).await;
    Ok(())
}

/// The public certificate (PEM), to upload to the app registration
/// (requires `ManageSettings`). Never the private key.
pub async fn public_certificate(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<String> {
    ctx.require(Capability::ManageSettings)?;
    let pem = load_pem(&core.db, &core.keys, id).await?;
    cert::public_pem(&pem).ok_or_else(|| CoreError::internal("no certificate in the stored PEM"))
}

/// The certificate change itself is stored; if applying it fails (for
/// example, the IDs aren't saved yet), the running settings stay.
async fn reload_quietly(core: &Core) {
    if let Err(e) = core.reload().await {
        tracing::warn!(error = %e, "certificate saved; settings not applied yet");
    }
}

/// The result of a connection test: what worked, and what didn't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestOutcome {
    pub ok: bool,
    pub lines: Vec<String>,
}

/// Try signing in to Microsoft Graph with `tenant_id` and `client_id` and the
/// newest certificate (a pending renewal first), without saving anything
/// (requires `ManageSettings`). Then read one user, which the Entra sync
/// needs (`User.Read.All`).
pub async fn test_connection(
    core: &Core,
    ctx: &Ctx,
    tenant_id: &str,
    client_id: &str,
) -> CoreResult<TestOutcome> {
    ctx.require(Capability::ManageSettings)?;
    let file = core.file_config();
    let certificate = if file.graph.certificate.is_empty() {
        newest_pem(&core.db, &core.keys).await?
    } else {
        Some(file.graph.certificate.expose().to_owned())
    };
    let outcome = run_test(&file.graph, tenant_id, client_id, certificate).await;
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Admin,
                "settings.tested",
                ctx.actor.clone(),
                "settings",
            )
            .outcome(if outcome.ok {
                Outcome::Success
            } else {
                Outcome::Failure
            })
            .context("area", "microsoft_365"),
        ],
    )
    .await?;
    Ok(outcome)
}

async fn run_test(
    base: &GraphConfig,
    tenant_id: &str,
    client_id: &str,
    certificate: Option<String>,
) -> TestOutcome {
    let fail = |line: String| TestOutcome {
        ok: false,
        lines: vec![line],
    };
    let (tenant_id, client_id) = (tenant_id.trim(), client_id.trim());
    if tenant_id.is_empty() || client_id.is_empty() {
        return fail("Enter the directory (tenant) ID and the application (client) ID.".into());
    }
    let Some(certificate) = certificate else {
        return fail("Generate a certificate first, and upload it to the app registration.".into());
    };
    let config = GraphConfig {
        tenant_id: tenant_id.to_owned(),
        client_id: client_id.to_owned(),
        certificate: Secret::new(certificate),
        ..base.clone()
    };
    let client = match GraphClient::new(&config) {
        Ok(c) => c,
        Err(e) => return fail(format!("The settings can't be used: {e}")),
    };
    if let Err(e) = client.sign_in().await {
        return fail(format!(
            "Microsoft refused the sign-in: {e}. Check the tenant and client IDs, and that this certificate is uploaded to the app registration."
        ));
    }
    let mut lines = vec!["Signed in to Microsoft Graph as the PDMS Service app.".to_owned()];
    match client
        .get::<serde_json::Value>("/users?$top=1&$select=id")
        .await
    {
        Ok(_) => lines.push("It can read users (needed for the Entra ID user sync).".into()),
        Err(GraphError::Auth(_)) => lines.push(
            "It can't read users yet: the Entra ID user sync needs the User.Read.All permission with admin consent. SharePoint storage doesn't."
                .into(),
        ),
        Err(e) => lines.push(format!("Reading users failed: {e}")),
    }
    TestOutcome { ok: true, lines }
}

/// Remind administrators when the active certificate expires within
/// [`REMIND_DAYS`] (once per certificate).
pub async fn remind_expiry(core: &Core) -> CoreResult<()> {
    let soon = sqlx::query!(
        "SELECT id, not_after FROM graph_certificates
          WHERE state = 'active' AND not_after < now() + make_interval(days => $1)",
        i32::try_from(REMIND_DAYS).unwrap_or(30)
    )
    .fetch_optional(&core.db)
    .await?;
    let Some(c) = soon else { return Ok(()) };
    let mut tx = core.db.begin().await?;
    crate::sharepoint::status::notify_admins(
        &mut tx,
        core,
        Notice {
            subject: "The PDMS Service certificate expires soon".into(),
            body: format!(
                "The certificate PDMS signs in to Microsoft 365 with expires on {}. Make a new one under Administration → Microsoft 365 → Connection, upload it to the app registration, and switch to it.",
                c.not_after.format("%Y-%m-%d")
            ),
            link: "/admin/microsoft-365".into(),
            document_id: None,
            appointment_id: None,
            dedupe_key: format!("graph-certificate-expiry:{}", c.id),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
