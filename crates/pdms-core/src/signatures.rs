// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Signature tracking: who must sign a document, whether they have been
//! asked, and when they signed. This is status tracking, not an electronic
//! signature: the signed copy is uploaded as a "signed copy" version.
//!
//! [`SignatureProvider`] is the seam for a future e-signature integration.
//! The built-in [`ManualSignatures`] provider records the request and leaves
//! the signing to people; a provider for a signing service would send the
//! document out and return its envelope ID, and a webhook would later record
//! the result through [`record_signed`].

use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use futures_util::future::BoxFuture;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Permission;
use serde::Serialize;
use uuid::Uuid;

use crate::context::Ctx;
use crate::documents::access::{self, DocRef};
use crate::documents::files::Upload;
use crate::documents::{self, VersionKind};
use crate::error::{CoreError, CoreResult};
use crate::notifications::actions::{self, Notice};
use crate::notifications::dispatch::Visibility;
use crate::{Core, schedule};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "signature_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// Listed but not yet asked.
    Pending,
    Requested,
    Signed,
    Declined,
}

impl SignatureStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SignatureStatus::Pending => "pending",
            SignatureStatus::Requested => "requested",
            SignatureStatus::Signed => "signed",
            SignatureStatus::Declined => "declined",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SignatureStatus::Pending => "Not yet requested",
            SignatureStatus::Requested => "Requested",
            SignatureStatus::Signed => "Signed",
            SignatureStatus::Declined => "Declined",
        }
    }
}

/// What a provider needs to send a request.
#[derive(Clone, Debug)]
pub struct SignatureRequest {
    pub signatory_id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub signer_name: String,
    pub signer_email: String,
    pub document_url: String,
}

/// Sends signature requests. Returns the provider's reference for the
/// request (for example an envelope ID), if it has one.
pub trait SignatureProvider: Send + Sync + fmt::Debug {
    /// Stored with each request, e.g. `manual`.
    fn key(&self) -> &'static str;
    fn request<'a>(
        &'a self,
        req: &'a SignatureRequest,
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
}

/// Records requests; people collect signatures and upload the signed copy.
#[derive(Debug, Default)]
pub struct ManualSignatures;

impl SignatureProvider for ManualSignatures {
    fn key(&self) -> &'static str {
        "manual"
    }

    fn request<'a>(
        &'a self,
        _req: &'a SignatureRequest,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Signatory {
    pub id: Uuid,
    pub document_id: Uuid,
    pub sort_order: i32,
    pub name: String,
    pub title: String,
    pub organization: String,
    pub email: String,
    pub user_id: Option<Uuid>,
    pub status: SignatureStatus,
    pub requested_at: Option<DateTime<Utc>>,
    pub requested_by: Option<String>,
    pub signed_on: Option<NaiveDate>,
    pub recorded_by: Option<String>,
    pub recorded_at: Option<DateTime<Utc>>,
    pub signed_version_no: Option<i32>,
    pub provider: String,
    pub provider_ref: Option<String>,
    pub note: String,
}

impl Signatory {
    /// "Jane Doe, Board Chair (Example Organization)" style.
    pub fn describe(&self) -> String {
        let mut s = self.name.clone();
        if !self.title.is_empty() {
            s.push_str(", ");
            s.push_str(&self.title);
        }
        if !self.organization.is_empty() {
            s.push_str(&format!(" ({})", self.organization));
        }
        s
    }
}

#[derive(Clone, Debug, Default)]
pub struct NewSignatory {
    pub name: String,
    pub title: String,
    pub organization: String,
    pub email: String,
    /// When the signer has an account here (they are told when asked to sign).
    pub user_id: Option<Uuid>,
}

async fn load(
    core: &Core,
    document_id: Option<Uuid>,
    only: Option<Uuid>,
) -> CoreResult<Vec<Signatory>> {
    Ok(sqlx::query_as!(
        Signatory,
        r#"SELECT s.id, s.document_id, s.sort_order, s.name, s.title, s.organization, s.email, s.user_id,
                  s.status AS "status: SignatureStatus", s.requested_at, rq.display_name AS "requested_by?",
                  s.signed_on, rc.display_name AS "recorded_by?", s.recorded_at, s.signed_version_no,
                  s.provider, s.provider_ref, s.note
             FROM signatories s
             LEFT JOIN users rq ON rq.id = s.requested_by
             LEFT JOIN users rc ON rc.id = s.recorded_by
            WHERE ($1::uuid IS NULL OR s.document_id = $1) AND ($2::uuid IS NULL OR s.id = $2)
            ORDER BY s.sort_order, s.created_at"#,
        document_id,
        only
    )
    .fetch_all(&core.db)
    .await?)
}

async fn get(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
    perm: Permission,
) -> CoreResult<(DocRef, Signatory)> {
    let s = load(core, None, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(CoreError::NotFound)?;
    let doc = access::authorize(core, ctx, s.document_id, perm).await?;
    Ok((doc, s))
}

fn event(ctx: &Ctx, doc: &DocRef, action: &str, s: &Signatory) -> AuditEvent {
    doc.event(ctx, Category::Workflow, action)
        .context("signatory_id", s.id)
        .context("signatory", s.describe())
}

/// A document's signatories, in order (requires `view`).
pub async fn list(core: &Core, ctx: &Ctx, document_id: Uuid) -> CoreResult<Vec<Signatory>> {
    access::authorize(core, ctx, document_id, Permission::View).await?;
    load(core, Some(document_id), None).await
}

fn field(s: &str, max: usize, what: &str) -> CoreResult<String> {
    let t = s.trim();
    if t.chars().count() > max {
        return Err(CoreError::validation(format!(
            "{what} must be at most {max} characters"
        )));
    }
    Ok(t.to_owned())
}

/// Add someone who must sign (requires `edit`).
pub async fn add(core: &Core, ctx: &Ctx, document_id: Uuid, n: NewSignatory) -> CoreResult<Uuid> {
    let doc = access::authorize(core, ctx, document_id, Permission::Edit).await?;
    let name = field(&n.name, 200, "the name")?;
    if name.is_empty() {
        return Err(CoreError::validation("enter the signer's name"));
    }
    let title = field(&n.title, 200, "the title")?;
    let organization = field(&n.organization, 200, "the organization")?;
    let email = field(&n.email, 320, "the email address")?;
    if !email.is_empty() && !email.contains('@') {
        return Err(CoreError::validation("enter a valid email address"));
    }
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO signatories (id, document_id, sort_order, name, title, organization, email, user_id)
         VALUES ($1, $2, (SELECT coalesce(max(sort_order), 0) + 10 FROM signatories WHERE document_id = $2),
                 $3, $4, $5, $6, $7)",
        id,
        document_id,
        name,
        title,
        organization,
        email,
        n.user_id
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
            CoreError::validation("the chosen user no longer exists")
        }
        _ => CoreError::Db(e),
    })?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc.event(ctx, Category::Workflow, "signatory.added")
                .context("signatory_id", id)
                .change("name", None::<String>, name)
                .change("title", None::<String>, title)
                .change("organization", None::<String>, organization)
                .change("user_id", None::<Uuid>, n.user_id),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Remove a signatory who has not been asked yet (requires `edit`).
pub async fn remove(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let (doc, s) = get(core, ctx, id, Permission::Edit).await?;
    if s.status != SignatureStatus::Pending {
        return Err(CoreError::validation(
            "only signers who have not been asked yet can be removed; record a decline instead",
        ));
    }
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "DELETE FROM signatories WHERE id = $1 AND status = 'pending'",
        id
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(&mut tx, vec![event(ctx, &doc, "signatory.removed", &s)]).await?;
    tx.commit().await?;
    Ok(())
}

/// Ask for a signature through the configured provider (requires `edit`).
/// A signer with an account here is told in the app and by email.
pub async fn request(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let (doc, s) = get(core, ctx, id, Permission::Edit).await?;
    if !matches!(
        s.status,
        SignatureStatus::Pending | SignatureStatus::Requested
    ) {
        return Err(CoreError::validation(format!(
            "this signature is already {}",
            s.status.label().to_lowercase()
        )));
    }
    let provider = &core.signatures;
    let url = core
        .config
        .server
        .public_url
        .join(&format!("documents/{}", doc.id()))
        .map(|u| u.to_string())
        .unwrap_or_default();
    let reference = provider
        .request(&SignatureRequest {
            signatory_id: s.id,
            document_id: doc.id(),
            external_id: doc.external_id.clone(),
            title: doc.title.clone(),
            signer_name: s.name.clone(),
            signer_email: s.email.clone(),
            document_url: url,
        })
        .await
        .map_err(|e| {
            CoreError::validation(format!("the signature request could not be sent: {e}"))
        })?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE signatories SET status = 'requested', requested_at = now(), requested_by = $2, provider = $3,
                provider_ref = $4, updated_at = now()
          WHERE id = $1",
        id,
        ctx.user_id(),
        provider.key(),
        reference
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, &doc, "signature.requested", &s)
                .context("provider", provider.key())
                .context("provider_ref", reference)
                .change("status", s.status.as_str(), "requested"),
        ],
    )
    .await?;
    if let Some(user) = s.user_id {
        let visibility = Visibility::document(core, doc.attrs.clone()).await?;
        actions::send(
            &mut tx,
            core,
            ctx,
            Notice {
                subject: format!("Signature requested: {}", doc.label()),
                body: format!(
                    "Your signature is needed on {} \"{}\".",
                    doc.external_id, doc.title
                ),
                link: format!("/documents/{}#signatures", doc.id()),
                document_id: Some(doc.id()),
                appointment_id: None,
                // Each request is a deliberate action (asking again is a
                // reminder), so every one is delivered.
                dedupe_key: format!("signature:{id}:{}", Uuid::now_v7()),
            },
            [user].into_iter().collect(),
            &visibility,
            event(ctx, &doc, "notification.created", &s),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// How a signature was recorded.
#[derive(Debug, Default)]
pub struct SignedRecord {
    /// Defaults to today.
    pub signed_on: Option<NaiveDate>,
    pub note: String,
    /// The signed copy, stored as a new "signed copy" version.
    pub file: Option<Upload>,
}

/// Record that someone signed (requires `edit`), optionally with the signed copy.
pub async fn record_signed(core: &Core, ctx: &Ctx, id: Uuid, r: SignedRecord) -> CoreResult<()> {
    let (doc, s) = get(core, ctx, id, Permission::Edit).await?;
    if s.status == SignatureStatus::Signed {
        return Err(CoreError::validation("this signature is already recorded"));
    }
    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let signed_on = r.signed_on.unwrap_or(today);
    if signed_on > today {
        return Err(CoreError::validation(
            "the signing date cannot be in the future",
        ));
    }
    let note = field(&r.note, 1000, "the note")?;
    // The file is stored first (in its own transaction); if recording the
    // signature then fails, the signed copy is still kept as a version.
    let version = match r.file {
        Some(upload) => Some(
            documents::add_version(
                core,
                ctx,
                doc.id(),
                upload,
                VersionKind::SignedCopy,
                &format!("Signed by {}", s.describe()),
                Some(signed_on),
            )
            .await?,
        ),
        None => None,
    };
    let mut tx = core.db.begin().await?;
    let updated = sqlx::query!(
        "UPDATE signatories SET status = 'signed', signed_on = $2, recorded_by = $3, recorded_at = now(),
                signed_version_no = coalesce($4, signed_version_no), note = $5, updated_at = now()
          WHERE id = $1 AND status <> 'signed'",
        id,
        signed_on,
        ctx.user_id(),
        version,
        note
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(CoreError::Conflict(
            "Someone else recorded this signature first.".into(),
        ));
    }
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, &doc, "signature.recorded", &s)
                .change("status", s.status.as_str(), "signed")
                .context("signed_on", signed_on)
                .context("signed_version", version)
                .context("note", note),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Record that someone declined to sign (requires `edit`).
pub async fn record_declined(core: &Core, ctx: &Ctx, id: Uuid, note: &str) -> CoreResult<()> {
    let (doc, s) = get(core, ctx, id, Permission::Edit).await?;
    if s.status == SignatureStatus::Signed {
        return Err(CoreError::validation("this signature is already recorded"));
    }
    let note = field(note, 1000, "the note")?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE signatories SET status = 'declined', recorded_by = $2, recorded_at = now(), note = $3,
                updated_at = now()
          WHERE id = $1",
        id,
        ctx.user_id(),
        note
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, &doc, "signature.declined", &s)
                .change("status", s.status.as_str(), "declined")
                .context("note", note),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
