// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Text extraction, OCR, and date suggestions.
//!
//! When a file version is added, a job extracts its text: embedded text from
//! PDFs, Word documents, and text files in Rust; scanned PDFs and images
//! through an [`OcrProvider`] (the OCRmyPDF/Tesseract sidecar, because Rust
//! has no mature OCR). The text is stored per version and made searchable,
//! and dates found in it become *suggestions* that a person with `edit`
//! accepts or dismisses. Nothing is ever applied automatically. With OCR
//! disabled (the default) everything else works; scanned files simply have
//! no text.

pub mod dates;
pub mod text;

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Permission;
use pdms_storage::BlobHash;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::Core;
use crate::context::Ctx;
use crate::documents::{self, access};
use crate::error::{CoreError, CoreResult};
use crate::notifications::actions::{self, Notice};
use crate::notifications::dispatch::Visibility;

/// Job kind: extract one version's text. Payload `{document_id, version_no, force?}`.
pub const EXTRACT_JOB: &str = "documents.extract_text";

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    /// The OCR service could not be reached or is busy: retry later.
    #[error("OCR service unavailable: {0}")]
    Unavailable(String),
    /// The file could not be read.
    #[error("OCR failed: {0}")]
    Failed(String),
}

/// Reads text from images and scanned PDFs.
pub trait OcrProvider: Send + Sync + fmt::Debug {
    fn key(&self) -> &'static str;
    fn ocr<'a>(
        &'a self,
        path: &'a Path,
        media_type: &'a str,
    ) -> BoxFuture<'a, Result<String, OcrError>>;
}

/// The OCR sidecar: `POST {url}/ocr` with the file as the body and its media
/// type as `Content-Type`; the answer is the text (`text/plain`).
#[derive(Debug)]
pub struct SidecarOcr {
    pub url: String,
    pub timeout: std::time::Duration,
}

impl OcrProvider for SidecarOcr {
    fn key(&self) -> &'static str {
        "sidecar"
    }

    fn ocr<'a>(
        &'a self,
        path: &'a Path,
        media_type: &'a str,
    ) -> BoxFuture<'a, Result<String, OcrError>> {
        Box::pin(async move {
            let body = tokio::fs::read(path)
                .await
                .map_err(|e| OcrError::Failed(e.to_string()))?;
            let client = reqwest::Client::builder()
                .timeout(self.timeout)
                .build()
                .map_err(|e| OcrError::Unavailable(e.to_string()))?;
            let resp = client
                .post(format!("{}/ocr", self.url.trim_end_matches('/')))
                .header("Content-Type", media_type)
                .body(body)
                .send()
                .await
                .map_err(|e| OcrError::Unavailable(e.to_string()))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| OcrError::Unavailable(e.to_string()))?;
            if status.is_success() {
                Ok(text)
            } else if status.is_client_error() {
                Err(OcrError::Failed(format!(
                    "HTTP {status}: {}",
                    text.chars().take(300).collect::<String>()
                )))
            } else {
                Err(OcrError::Unavailable(format!("HTTP {status}")))
            }
        })
    }
}

/// The OCR provider configured for this installation, if any.
pub fn from_config(cfg: &crate::config::OcrConfig) -> Option<Arc<dyn OcrProvider>> {
    (!cfg.sidecar_url.trim().is_empty()).then(|| {
        Arc::new(SidecarOcr {
            url: cfg.sidecar_url.trim().to_owned(),
            timeout: std::time::Duration::from_secs(cfg.sidecar_timeout_seconds.max(1)),
        }) as Arc<dyn OcrProvider>
    })
}

/// Queue text extraction for a version (inside the caller's transaction).
pub(crate) async fn enqueue(
    conn: &mut sqlx::PgConnection,
    document_id: Uuid,
    version_no: i32,
    force: bool,
) -> CoreResult<()> {
    let key = if force {
        format!("extract:{document_id}:{version_no}:{}", Uuid::now_v7())
    } else {
        format!("extract:{document_id}:{version_no}")
    };
    crate::jobs::enqueue(
        conn,
        crate::jobs::NewJob::new(
            EXTRACT_JOB,
            serde_json::json!({ "document_id": document_id, "version_no": version_no, "force": force }),
        )
        .idempotency_key(key)
        .max_attempts(6),
    )
    .await?;
    Ok(())
}

struct Extracted {
    status: &'static str,
    method: &'static str,
    text: String,
    error: Option<String>,
}

impl Extracted {
    fn failed(method: &'static str, e: String) -> Self {
        Extracted {
            status: "failed",
            method,
            text: String::new(),
            error: Some(e),
        }
    }

    fn unsupported(why: &str) -> Self {
        Extracted {
            status: "unsupported",
            method: "none",
            text: String::new(),
            error: Some(why.to_owned()),
        }
    }

    fn text(method: &'static str, text: String) -> Self {
        let status = if text::meaningful_chars(&text) == 0 {
            "empty"
        } else {
            "done"
        };
        let text: String = text.chars().take(2_000_000).collect();
        Extracted {
            status,
            method,
            text,
            error: None,
        }
    }
}

async fn read_text(core: &Core, path: &Path, media_type: &str) -> CoreResult<Extracted> {
    let ocr = core.ocr.clone();
    let run_ocr = |ocr: Arc<dyn OcrProvider>| async move {
        match ocr.ocr(path, media_type).await {
            Ok(t) => Ok(Extracted::text("ocr", t)),
            Err(OcrError::Failed(e)) => Ok(Extracted::failed("ocr", e)),
            Err(e @ OcrError::Unavailable(_)) => Err(CoreError::internal(e)),
        }
    };
    Ok(match text::kind(media_type) {
        text::Kind::Pdf => match text::pdf(path).await {
            Ok(t) if text::meaningful_chars(&t) >= core.config.ocr.min_text_chars => {
                Extracted::text("pdf_text", t)
            }
            Ok(t) => match ocr {
                Some(o) => run_ocr(o).await?,
                None => Extracted::text("pdf_text", t),
            },
            Err(e) => match ocr {
                Some(o) => run_ocr(o).await?,
                None => Extracted::failed("pdf_text", e),
            },
        },
        text::Kind::Docx => match text::docx(path).await {
            Ok(t) => Extracted::text("docx", t),
            Err(e) => Extracted::failed("docx", e),
        },
        text::Kind::Plain => match text::plain(path).await {
            Ok(t) => Extracted::text("plain", t),
            Err(e) => Extracted::failed("plain", e),
        },
        text::Kind::Image => match ocr {
            Some(o) => run_ocr(o).await?,
            None => Extracted::unsupported("images need OCR, which is not configured"),
        },
        text::Kind::Unsupported => Extracted::unsupported("no text extraction for this file type"),
    })
}

/// Extract one version's text, store it, make it searchable, and record date
/// suggestions. `Err` means "retry later" (the OCR service was unavailable).
pub async fn extract(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    force: bool,
) -> CoreResult<()> {
    if !core.config.ocr.text_extraction {
        return Ok(());
    }
    if !force {
        let done = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM document_texts WHERE document_id = $1 AND version_no = $2) AS "e!""#,
            document_id,
            version_no
        )
        .fetch_one(&core.db)
        .await?;
        if done {
            return Ok(());
        }
    }
    let v = sqlx::query!(
        r#"SELECT v.blob_sha256 AS "sha!", b.media_type, b.size_bytes, d.external_id, d.title, d.department_id,
                  d.owner_id, d.visibility::text AS "visibility!"
             FROM document_versions v
             JOIN blobs b ON b.sha256 = v.blob_sha256
             JOIN documents d ON d.id = v.document_id
            WHERE v.document_id = $1 AND v.version_no = $2 AND d.deleted_at IS NULL"#,
        document_id,
        version_no
    )
    .fetch_optional(&core.db)
    .await?;
    let Some(v) = v else { return Ok(()) };
    let hash = BlobHash::from_slice(&v.sha).ok_or_else(|| CoreError::internal("bad blob hash"))?;

    // The same file was already read for another version (e.g. a restore).
    let earlier = sqlx::query!(
        r#"SELECT status::text AS "status!", method, text, error FROM document_texts
            WHERE blob_sha256 = $1 AND NOT (document_id = $2 AND version_no = $3)
            ORDER BY extracted_at DESC LIMIT 1"#,
        &v.sha,
        document_id,
        version_no
    )
    .fetch_optional(&core.db)
    .await?;
    let (result, reused) = match earlier {
        Some(e) if !force => (
            Extracted {
                status: match e.status.as_str() {
                    "done" => "done",
                    "empty" => "empty",
                    "unsupported" => "unsupported",
                    _ => "failed",
                },
                method: match e.method.as_str() {
                    "pdf_text" => "pdf_text",
                    "docx" => "docx",
                    "plain" => "plain",
                    "ocr" => "ocr",
                    _ => "none",
                },
                text: e.text,
                error: e.error,
            },
            true,
        ),
        _ if u64::try_from(v.size_bytes).unwrap_or(u64::MAX) > core.config.ocr.max_bytes => (
            Extracted::unsupported("the file is larger than the text extraction limit"),
            false,
        ),
        _ => {
            let dir = core.config.storage.spool_path.clone();
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(CoreError::internal)?;
            let path = dir.join(format!("extract-{}", Uuid::now_v7()));
            let r = async {
                let mut f = tokio::fs::File::create(&path)
                    .await
                    .map_err(CoreError::internal)?;
                let mut s = crate::documents::filestore::open(
                    core,
                    document_id,
                    version_no,
                    hash,
                    u64::try_from(v.size_bytes).unwrap_or(0),
                )
                .await?;
                while let Some(chunk) = s.next().await {
                    f.write_all(&chunk.map_err(CoreError::internal)?)
                        .await
                        .map_err(CoreError::internal)?;
                }
                f.flush().await.map_err(CoreError::internal)?;
                read_text(core, &path, &v.media_type).await
            }
            .await;
            let _ = tokio::fs::remove_file(&path).await;
            (r?, false)
        }
    };

    let mut suggestions = Vec::new();
    if result.status == "done" && core.config.ocr.suggest_dates && !reused {
        let current = sqlx::query!(
            "SELECT execution_date, effective_date, renewal_date, expiration_date, next_review_date,
                    review_interval_months FROM documents WHERE id = $1",
            document_id
        )
        .fetch_one(&core.db)
        .await?;
        suggestions = dates::suggest(&result.text)
            .into_iter()
            .filter(|s| {
                use dates::Field::*;
                let same = match s.field {
                    Execution => current.execution_date == s.date,
                    Effective => current.effective_date == s.date,
                    Renewal => current.renewal_date == s.date,
                    Expiration => current.expiration_date == s.date,
                    NextReview => current.next_review_date == s.date,
                    ReviewInterval => current.review_interval_months == s.months,
                };
                !same
            })
            .collect();
    }

    let mut tx = core.db.begin().await?;
    let chars = i32::try_from(result.text.chars().count()).unwrap_or(i32::MAX);
    sqlx::query!(
        "INSERT INTO document_texts (document_id, version_no, blob_sha256, status, method, text, char_count, error)
         VALUES ($1, $2, $3, $4::text::extraction_status, $5, $6, $7, $8)
         ON CONFLICT (document_id, version_no) DO UPDATE
            SET status = EXCLUDED.status, method = EXCLUDED.method, text = EXCLUDED.text,
                char_count = EXCLUDED.char_count, error = EXCLUDED.error, extracted_at = now()",
        document_id,
        version_no,
        &v.sha,
        result.status,
        result.method,
        result.text,
        chars,
        result.error
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("SELECT refresh_document_search($1)")
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
    let mut added = 0i64;
    for s in &suggestions {
        let r = sqlx::query!(
            "INSERT INTO date_suggestions (id, document_id, version_no, field, suggested_date, suggested_months,
                                           snippet, confidence)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
            Uuid::now_v7(),
            document_id,
            version_no,
            s.field.as_str(),
            s.date,
            s.months,
            s.snippet.chars().take(1000).collect::<String>(),
            i16::from(s.confidence)
        )
        .execute(&mut *tx)
        .await?;
        added += i64::try_from(r.rows_affected()).unwrap_or(0);
    }
    let ctx = Ctx::system("text-extraction");
    let label = format!("{} {}", v.external_id, v.title);
    // A fresh event (with its own ID) each time it is needed.
    let about = || {
        AuditEvent::new(
            Category::Content,
            "document.text_extracted",
            ctx.actor.clone(),
            "document",
        )
        .object(document_id, Some(&label))
        .document(document_id)
        .department(Some(v.department_id))
    };
    let mut e = about()
        .context("version", version_no)
        .context("status", result.status)
        .context("method", result.method)
        .context("characters", i64::from(chars))
        .context("date_suggestions", added)
        .context("reused", reused);
    if let Some(err) = &result.error {
        e = e.context("error", err.clone());
    }
    pdms_audit::append(&mut tx, vec![e]).await?;
    if added > 0 {
        let attrs = pdms_authz::DocumentAttrs {
            id: document_id,
            department_id: v.department_id,
            owner_id: v.owner_id,
            visibility: v.visibility.parse().map_err(CoreError::internal)?,
        };
        let visibility = Visibility::document(core, attrs).await?;
        actions::send(
            &mut tx,
            core,
            &ctx,
            Notice {
                subject: format!("Dates found to confirm: {label}"),
                body: format!(
                    "{added} possible date(s) were found in the file for {} \"{}\". Nothing has been changed: \
                     confirm or dismiss each one on the document page.",
                    v.external_id, v.title
                ),
                link: format!("/documents/{document_id}#suggestions"),
                document_id: Some(document_id),
                appointment_id: None,
                dedupe_key: format!("suggestions:{document_id}:{version_no}"),
            },
            [v.owner_id].into_iter().collect(),
            &visibility,
            about(),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Text extracted from a document's latest file.
#[derive(Clone, Debug, Serialize)]
pub struct ExtractedText {
    pub version_no: i32,
    pub status: String,
    pub method: String,
    pub char_count: i32,
    pub error: Option<String>,
    pub extracted_at: DateTime<Utc>,
    /// The first part of the text.
    pub preview: String,
}

/// The latest extraction result for a document (requires `view`).
pub async fn latest_text(
    core: &Core,
    ctx: &Ctx,
    document_id: Uuid,
) -> CoreResult<Option<ExtractedText>> {
    access::authorize(core, ctx, document_id, Permission::View).await?;
    Ok(sqlx::query!(
        r#"SELECT version_no, status::text AS "status!", method, char_count, error, extracted_at,
                  left(text, 2000) AS "preview!"
             FROM document_texts WHERE document_id = $1 ORDER BY version_no DESC LIMIT 1"#,
        document_id
    )
    .fetch_optional(&core.db)
    .await?
    .map(|r| ExtractedText {
        version_no: r.version_no,
        status: r.status,
        method: r.method,
        char_count: r.char_count,
        error: r.error,
        extracted_at: r.extracted_at,
        preview: r.preview,
    }))
}

/// Run extraction again for the current file (requires `edit`).
pub async fn reprocess(core: &Core, ctx: &Ctx, document_id: Uuid) -> CoreResult<()> {
    let doc = access::authorize(core, ctx, document_id, Permission::Edit).await?;
    let version = sqlx::query_scalar!(
        "SELECT max(version_no) FROM document_versions WHERE document_id = $1 AND file_changed",
        document_id
    )
    .fetch_one(&core.db)
    .await?
    .ok_or_else(|| CoreError::validation("this document has no file"))?;
    let mut tx = core.db.begin().await?;
    enqueue(&mut tx, document_id, version, true).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc.event(ctx, Category::Content, "document.text_extraction_requested")
                .context("version", version),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct DateSuggestion {
    pub id: Uuid,
    pub version_no: i32,
    pub field: String,
    pub field_label: &'static str,
    pub date: Option<NaiveDate>,
    pub months: Option<i32>,
    pub snippet: String,
    pub confidence: i16,
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl DateSuggestion {
    pub fn value_label(&self) -> String {
        match (self.date, self.months) {
            (Some(d), _) => d.format("%B %-d, %Y").to_string(),
            (None, Some(m)) if m % 12 == 0 => {
                let y = m / 12;
                if y == 1 {
                    "every year".into()
                } else {
                    format!("every {y} years")
                }
            }
            (None, Some(m)) => format!("every {m} months"),
            (None, None) => String::new(),
        }
    }
}

/// A document's date suggestions, pending first (requires `view`).
pub async fn suggestions(
    core: &Core,
    ctx: &Ctx,
    document_id: Uuid,
) -> CoreResult<Vec<DateSuggestion>> {
    access::authorize(core, ctx, document_id, Permission::View).await?;
    load(core, Some(document_id), None).await
}

async fn load(
    core: &Core,
    document_id: Option<Uuid>,
    only: Option<Uuid>,
) -> CoreResult<Vec<DateSuggestion>> {
    Ok(sqlx::query!(
        r#"SELECT s.id, s.document_id, s.version_no, s.field, s.suggested_date, s.suggested_months, s.snippet,
                  s.confidence, s.status::text AS "status!", u.display_name AS "decided_by?", s.decided_at
             FROM date_suggestions s LEFT JOIN users u ON u.id = s.decided_by
            WHERE ($1::uuid IS NULL OR s.document_id = $1) AND ($2::uuid IS NULL OR s.id = $2)
            ORDER BY (s.status = 'pending') DESC, s.field, s.confidence DESC, s.created_at"#,
        document_id,
        only
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| DateSuggestion {
        field_label: dates::Field::parse(&r.field).map_or("?", dates::Field::label),
        id: r.id,
        version_no: r.version_no,
        field: r.field,
        date: r.suggested_date,
        months: r.suggested_months,
        snippet: r.snippet,
        confidence: r.confidence,
        status: r.status,
        decided_by: r.decided_by,
        decided_at: r.decided_at,
    })
    .collect())
}

async fn pending(
    core: &Core,
    ctx: &Ctx,
    id: Uuid,
) -> CoreResult<(access::DocRef, DateSuggestion, Uuid)> {
    let doc_id = sqlx::query_scalar!("SELECT document_id FROM date_suggestions WHERE id = $1", id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(CoreError::NotFound)?;
    let doc = access::authorize(core, ctx, doc_id, Permission::Edit).await?;
    let s = load(core, None, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(CoreError::NotFound)?;
    if s.status != "pending" {
        return Err(CoreError::Conflict(
            "This suggestion was already decided.".into(),
        ));
    }
    Ok((doc, s, doc_id))
}

fn suggestion_event(
    ctx: &Ctx,
    doc: &access::DocRef,
    action: &str,
    s: &DateSuggestion,
) -> AuditEvent {
    doc.event(ctx, Category::Metadata, action)
        .context("suggestion_id", s.id)
        .context("field", s.field.clone())
        .context("value", s.value_label())
        .context("confidence", i64::from(s.confidence))
        .context("snippet", s.snippet.clone())
}

/// Confirm a suggestion: the document's field is set to it (an ordinary,
/// audited metadata change), and other pending suggestions for the same
/// field are set aside. Requires `edit`.
pub async fn accept(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let (doc, s, doc_id) = pending(core, ctx, id).await?;
    let field =
        dates::Field::parse(&s.field).ok_or_else(|| CoreError::internal("unknown field"))?;
    let defs = documents::fields::list(&core.db).await?;
    let mut tx = core.db.begin().await?;
    let change = documents::apply_date_field(
        &mut tx,
        ctx,
        &doc,
        field,
        s.date,
        s.months,
        "confirmed from a date found in the file",
        &defs,
    )
    .await?;
    sqlx::query!(
        "UPDATE date_suggestions SET status = CASE WHEN id = $1 THEN 'accepted'::suggestion_status
                                                   ELSE 'dismissed'::suggestion_status END,
                decided_by = $3, decided_at = now()
          WHERE document_id = $2 AND field = $4 AND status = 'pending'",
        id,
        doc_id,
        ctx.user_id(),
        s.field
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("SELECT refresh_document_search($1)")
        .bind(doc_id)
        .execute(&mut *tx)
        .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            change,
            suggestion_event(ctx, &doc, "date_suggestion.accepted", &s),
        ],
    )
    .await?;
    tx.commit().await?;
    crate::notifications::refresh_document(core, doc_id, Utc::now()).await?;
    Ok(())
}

/// Set a suggestion aside without changing the document. Requires `edit`.
pub async fn dismiss(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    let (doc, s, _) = pending(core, ctx, id).await?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE date_suggestions SET status = 'dismissed', decided_by = $2, decided_at = now()
          WHERE id = $1 AND status = 'pending'",
        id,
        ctx.user_id()
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![suggestion_event(ctx, &doc, "date_suggestion.dismissed", &s)],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
