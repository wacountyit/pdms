// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Records request packages.
//!
//! A records officer (capability `BuildExportPackages`) chooses documents
//! they may *export* (the `export` permission, checked per document by the
//! central engine, so Restricted documents need an explicit grant), the
//! metadata to include, which file versions, and optionally each document's
//! audit history. The package is a ZIP with a manifest, a `SHA256SUMS` file,
//! and a README, tied to the request reference number. Every file's content
//! is re-hashed while packaging and must match the stored hash.
//!
//! Creating a package is audited once for the package and once per document
//! (`document.exported`, so it appears on each document's timeline);
//! downloading it is audited too.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::{Capability, Permission};
use pdms_storage::{BlobHash, ByteStream};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::Core;
use crate::context::Ctx;
use crate::documents::search::{self, DocumentFilter, DocumentSummary};
use crate::documents::{self, access, fields};
use crate::error::{CoreError, CoreResult};

/// Metadata a package can include: (key, label).
pub const FIELDS: &[(&str, &str)] = &[
    ("external_id", "Document ID"),
    ("title", "Title"),
    ("description", "Description"),
    ("type", "Type"),
    ("department", "Department"),
    ("owner", "Primary owner"),
    ("status", "Status"),
    ("execution_date", "Execution date"),
    ("effective_date", "Effective date"),
    ("last_review_date", "Last review"),
    ("next_review_date", "Next review"),
    ("renewal_date", "Renewal date"),
    ("expiration_date", "Expiration date"),
    ("review_interval_months", "Review interval (months)"),
    ("tags", "Tags"),
    ("custom_fields", "Custom fields"),
    ("created_at", "Created"),
    ("updated_at", "Last updated"),
];

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Versions {
    /// The current file only.
    Current,
    /// Every file version.
    All,
    /// Metadata only.
    None,
}

impl Versions {
    pub fn as_str(self) -> &'static str {
        match self {
            Versions::Current => "current",
            Versions::All => "all",
            Versions::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "current" => Some(Versions::Current),
            "all" => Some(Versions::All),
            "none" => Some(Versions::None),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PackageRequest {
    /// The records request number, e.g. `REQ-2026-014`.
    pub request_reference: String,
    pub document_ids: Vec<Uuid>,
    pub fields: Vec<String>,
    pub versions: Versions,
    pub include_audit: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackageInfo {
    pub id: Uuid,
    pub request_reference: String,
    pub requested_by: Uuid,
    pub requested_by_name: String,
    pub created_at: DateTime<Utc>,
    pub document_count: i64,
    pub include_versions: String,
    pub include_audit: bool,
    pub note: String,
    pub file_count: i32,
    pub size_bytes: i64,
    pub package_sha256: String,
    pub manifest_sha256: String,
}

impl PackageInfo {
    pub fn file_name(&self) -> String {
        let reference: String = self
            .request_reference
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!(
            "records-request-{reference}-{}.zip",
            self.created_at.format("%Y%m%d")
        )
    }
}

/// Documents the caller could put in a package (those they may export).
pub async fn candidates(
    core: &Core,
    ctx: &Ctx,
    filter: &DocumentFilter,
) -> CoreResult<Vec<DocumentSummary>> {
    ctx.require(Capability::BuildExportPackages)?;
    search::all(core, ctx, filter, Permission::Export, 500).await
}

async fn load(
    core: &Core,
    only: Option<Uuid>,
    requester: Option<Uuid>,
) -> CoreResult<Vec<PackageInfo>> {
    Ok(sqlx::query!(
        r#"SELECT p.id, p.request_reference, p.requested_by, u.display_name, p.created_at,
                  cardinality(p.document_ids) AS "document_count!", p.include_versions, p.include_audit, p.note,
                  p.file_count, p.size_bytes, p.package_sha256, p.manifest_sha256
             FROM export_packages p JOIN users u ON u.id = p.requested_by
            WHERE ($1::uuid IS NULL OR p.id = $1) AND ($2::uuid IS NULL OR p.requested_by = $2)
            ORDER BY p.created_at DESC LIMIT 500"#,
        only,
        requester
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| PackageInfo {
        id: r.id,
        request_reference: r.request_reference,
        requested_by: r.requested_by,
        requested_by_name: r.display_name,
        created_at: r.created_at,
        document_count: i64::from(r.document_count),
        include_versions: r.include_versions,
        include_audit: r.include_audit,
        note: r.note,
        file_count: r.file_count,
        size_bytes: r.size_bytes,
        package_sha256: hex::encode(r.package_sha256),
        manifest_sha256: hex::encode(r.manifest_sha256),
    })
    .collect())
}

/// Packages the caller built (auditors see every package).
pub async fn list(core: &Core, ctx: &Ctx) -> CoreResult<Vec<PackageInfo>> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    if ctx.can(Capability::ViewAudit) {
        load(core, None, None).await
    } else {
        ctx.require(Capability::BuildExportPackages)?;
        load(core, None, Some(user)).await
    }
}

/// A safe path segment for file names inside the ZIP.
fn segment(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_owned();
    if trimmed.is_empty() {
        "file".into()
    } else {
        trimmed.chars().take(120).collect()
    }
}

/// Writes ZIP entries and remembers each one's hash and size.
struct Packer {
    zip: ZipWriter<std::fs::File>,
    entries: Vec<(String, String, u64)>,
    total: u64,
    limit: u64,
}

impl Packer {
    fn options() -> SimpleFileOptions {
        SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true)
    }

    fn add_bytes(&mut self, path: &str, bytes: &[u8]) -> CoreResult<()> {
        self.zip
            .start_file(path, Self::options())
            .map_err(CoreError::internal)?;
        self.zip.write_all(bytes).map_err(CoreError::internal)?;
        self.entries.push((
            path.to_owned(),
            hex::encode(Sha256::digest(bytes)),
            bytes.len() as u64,
        ));
        Ok(())
    }

    /// Stream a stored file in, checking it against its recorded hash.
    async fn add_stream(
        &mut self,
        path: &str,
        expected: BlobHash,
        mut stream: ByteStream,
    ) -> CoreResult<u64> {
        self.zip
            .start_file(path, Self::options())
            .map_err(CoreError::internal)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(CoreError::internal)?;
            size += chunk.len() as u64;
            self.total += chunk.len() as u64;
            if self.total > self.limit {
                return Err(CoreError::validation(format!(
                    "the package would exceed the {} MB limit; choose fewer documents or only current versions",
                    self.limit / (1024 * 1024)
                )));
            }
            hasher.update(&chunk);
            self.zip.write_all(&chunk).map_err(CoreError::internal)?;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        if digest != expected.0 {
            return Err(CoreError::internal(format!(
                "stored file {} does not match its recorded hash; run verify-integrity",
                expected.hex()
            )));
        }
        self.entries.push((path.to_owned(), expected.hex(), size));
        Ok(size)
    }
}

fn metadata(d: &documents::Document, fields: &[String], defs: &[fields::FieldDef]) -> Value {
    let date = |v: Option<chrono::NaiveDate>| v.map(|d| d.to_string());
    let mut m = serde_json::Map::new();
    m.insert("id".into(), json!(d.id));
    for f in fields {
        let v = match f.as_str() {
            "external_id" => json!(d.external_id),
            "title" => json!(d.title),
            "description" => json!(d.description),
            "type" => json!(d.type_name),
            "department" => json!(d.department_name),
            "owner" => json!(d.owner_name),
            "status" => json!(d.status.label()),
            "execution_date" => json!(date(d.execution_date)),
            "effective_date" => json!(date(d.effective_date)),
            "last_review_date" => json!(date(d.last_review_date)),
            "next_review_date" => json!(date(d.next_review_date)),
            "renewal_date" => json!(date(d.renewal_date)),
            "expiration_date" => json!(date(d.expiration_date)),
            "review_interval_months" => {
                json!(d.review_interval_months.or(d.type_review_interval_months))
            }
            "tags" => json!(d.tags.iter().map(|t| t.name.clone()).collect::<Vec<_>>()),
            "custom_fields" => {
                let mut c = serde_json::Map::new();
                if let Some(obj) = d.custom_fields.as_object() {
                    for (k, v) in obj {
                        let label = defs
                            .iter()
                            .find(|x| &x.key == k)
                            .map_or_else(|| k.clone(), |x| x.label.clone());
                        c.insert(k.clone(), json!({ "label": label, "value": v }));
                    }
                }
                Value::Object(c)
            }
            "created_at" => json!(d.created_at),
            "updated_at" => json!(d.updated_at),
            _ => continue,
        };
        m.insert(f.clone(), v);
    }
    Value::Object(m)
}

/// A document's audit history as (JSON, CSV).
async fn audit_history(core: &Core, document_id: Uuid) -> CoreResult<(Vec<u8>, Vec<u8>)> {
    let rows = sqlx::query!(
        r#"SELECT seq, occurred_at, category::text AS "category!", action, outcome::text AS "outcome!",
                  actor_display, actor_auth_method::text AS "auth_method!", changes, context, entry_hash
             FROM audit_log WHERE document_id = $1 ORDER BY seq"#,
        document_id
    )
    .fetch_all(&core.db)
    .await?;
    let json_rows: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "seq": r.seq,
                "occurred_at": r.occurred_at,
                "category": r.category,
                "action": r.action,
                "outcome": r.outcome,
                "actor": r.actor_display,
                "auth_method": r.auth_method,
                "changes": r.changes,
                "context": r.context,
                "entry_hash": hex::encode(&r.entry_hash),
            })
        })
        .collect();
    let json_bytes = serde_json::to_vec_pretty(&json_rows).map_err(CoreError::internal)?;
    let mut csv = String::from("seq,occurred_at,category,action,outcome,actor,changes\n");
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    for r in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            r.seq,
            r.occurred_at.to_rfc3339(),
            r.category,
            r.action,
            r.outcome,
            q(r.actor_display.as_deref().unwrap_or("")),
            q(&r.changes
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default()),
        ));
    }
    Ok((json_bytes, csv.into_bytes()))
}

fn readme(org: &str, reference: &str, created: DateTime<Utc>, by: &str) -> String {
    format!(
        "{org}: records request package\r\n\
         Request reference: {reference}\r\n\
         Prepared: {} by {by}\r\n\r\n\
         Contents\r\n\
         - manifest.json: the request, the options chosen, and every document and file in this package.\r\n\
         - documents/<document ID>/metadata.json: the document's details.\r\n\
         - documents/<document ID>/files/: the document's file versions (v<number>_<name>).\r\n\
         - documents/<document ID>/audit-history.json and .csv: the document's audit trail, when included.\r\n\
         - SHA256SUMS: the SHA-256 hash of every file. Check with: sha256sum -c SHA256SUMS\r\n\r\n\
         Every file was checked against the hash recorded when it was stored.\r\n",
        created.to_rfc3339()
    )
}

/// Build a package. Returns its ID.
pub async fn build(core: &Core, ctx: &Ctx, req: PackageRequest) -> CoreResult<Uuid> {
    ctx.require(Capability::BuildExportPackages)?;
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let reference = req.request_reference.trim().to_owned();
    if reference.is_empty()
        || reference.chars().count() > 100
        || reference.chars().any(char::is_control)
    {
        return Err(CoreError::validation(
            "enter the request reference number (up to 100 characters)",
        ));
    }
    let mut ids = req.document_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(CoreError::validation("choose at least one document"));
    }
    if ids.len() > core.config().exports.max_documents {
        return Err(CoreError::validation(format!(
            "a package may hold at most {} documents",
            core.config().exports.max_documents
        )));
    }
    let mut fields: Vec<String> = req
        .fields
        .iter()
        .filter(|f| FIELDS.iter().any(|(k, _)| k == f))
        .cloned()
        .collect();
    fields.dedup();
    for must in ["external_id", "title"] {
        if !fields.iter().any(|f| f == must) {
            fields.insert(0, must.to_owned());
        }
    }
    let note: String = req.note.trim().chars().take(2000).collect();

    // Authorize every document first (denials are audited by `authorize`).
    let mut docs = Vec::new();
    for id in &ids {
        docs.push(access::authorize(core, ctx, *id, Permission::Export).await?);
    }

    let id = Uuid::now_v7();
    let created_at = Utc::now();
    let by = ctx.actor.display.clone().unwrap_or_default();
    let spool = core.config().storage.spool_path.clone();
    std::fs::create_dir_all(&spool).map_err(CoreError::internal)?;
    let path: PathBuf = spool.join(format!("export-{id}.zip"));
    let result = async {
        let file = std::fs::File::create(&path).map_err(CoreError::internal)?;
        let mut p = Packer {
            zip: ZipWriter::new(file),
            entries: Vec::new(),
            total: 0,
            limit: core.config().exports.max_bytes,
        };
        let org = crate::branding::org_name(core).await;
        let defs = fields::list(&core.db).await?;
        let mut manifest_docs = Vec::new();
        let mut file_count = 0i32;
        p.add_bytes(
            "README.txt",
            readme(&org, &reference, created_at, &by).as_bytes(),
        )?;
        for doc_ref in &docs {
            let mut conn = core.db.acquire().await?;
            let d = documents::fetch(&mut conn, doc_ref.id())
                .await?
                .ok_or(CoreError::NotFound)?;
            drop(conn);
            let folder = format!("documents/{}", segment(&d.external_id));
            let meta = metadata(&d, &fields, &defs);
            let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(CoreError::internal)?;
            p.add_bytes(&format!("{folder}/metadata.json"), &meta_bytes)?;
            let mut files = Vec::new();
            if req.versions != Versions::None {
                let versions = documents::versions(core, ctx, d.id).await?;
                let with_files: Vec<_> = versions
                    .iter()
                    .filter(|v| v.file_changed && v.blob_sha256.is_some())
                    .collect();
                let chosen: Vec<_> = match req.versions {
                    Versions::All => with_files,
                    _ => with_files
                        .into_iter()
                        .max_by_key(|v| v.version_no)
                        .into_iter()
                        .collect(),
                };
                for v in chosen {
                    let Some(hash) = v.blob_sha256.as_deref().and_then(BlobHash::from_hex) else {
                        continue;
                    };
                    let name = format!(
                        "{folder}/files/v{}_{}",
                        v.version_no,
                        segment(v.file_name.as_deref().unwrap_or("file"))
                    );
                    let stream = crate::documents::filestore::open(
                        core,
                        d.id,
                        v.version_no,
                        hash,
                        v.size_bytes
                            .and_then(|s| u64::try_from(s).ok())
                            .unwrap_or(0),
                    )
                    .await?;
                    let size = p.add_stream(&name, hash, stream).await?;
                    file_count += 1;
                    files.push(json!({
                        "path": name,
                        "version": v.version_no,
                        "kind": v.kind.as_str(),
                        "file_name": v.file_name,
                        "sha256": hash.hex(),
                        "size": size,
                        "added_at": v.created_at,
                    }));
                }
            }
            let mut audit_files = Vec::new();
            if req.include_audit {
                let (j, c) = audit_history(core, d.id).await?;
                for (name, bytes) in [("audit-history.json", j), ("audit-history.csv", c)] {
                    let path = format!("{folder}/{name}");
                    p.add_bytes(&path, &bytes)?;
                    audit_files.push(path);
                }
            }
            manifest_docs.push(json!({
                "id": d.id,
                "external_id": d.external_id,
                "title": d.title,
                "folder": folder,
                "metadata": format!("{folder}/metadata.json"),
                "files": files,
                "audit_history": audit_files,
            }));
        }
        let entries: Vec<Value> = p
            .entries
            .iter()
            .map(|(path, sha, size)| json!({ "path": path, "sha256": sha, "size": size }))
            .collect();
        let manifest = json!({
            "format": "pdms-records-package/1",
            "package_id": id,
            "organization": org,
            "request_reference": reference,
            "created_at": created_at,
            "created_by": { "user_id": user, "name": by },
            "note": note,
            "options": {
                "fields": fields,
                "versions": req.versions.as_str(),
                "include_audit": req.include_audit,
            },
            "documents": manifest_docs,
            "files": entries,
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(CoreError::internal)?;
        let manifest_sha: [u8; 32] = Sha256::digest(&manifest_bytes).into();
        p.add_bytes("manifest.json", &manifest_bytes)?;
        let sums: String = p
            .entries
            .iter()
            .map(|(path, sha, _)| format!("{sha}  {path}\n"))
            .collect();
        p.zip
            .start_file("SHA256SUMS", Packer::options())
            .map_err(CoreError::internal)?;
        p.zip
            .write_all(sums.as_bytes())
            .map_err(CoreError::internal)?;
        p.zip.finish().map_err(CoreError::internal)?;
        Ok::<_, CoreError>((manifest_sha, file_count))
    }
    .await;
    let (manifest_sha, file_count) = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };

    // Store the package like any other blob (content-addressed, never overwritten).
    let bytes = std::fs::read(&path).map_err(CoreError::internal)?;
    let package_hash = pdms_storage::sha256(&bytes);
    let size = bytes.len() as u64;
    drop(bytes);
    let stored = core.storage.put_file(package_hash, &path, size).await;
    let _ = std::fs::remove_file(&path);
    stored.map_err(CoreError::internal)?;

    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO export_packages (id, request_reference, requested_by, created_at, document_ids, include_versions,
                                      include_audit, fields, note, file_count, size_bytes, package_sha256,
                                      manifest_sha256, storage_backend)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        id,
        reference,
        user,
        created_at,
        &ids,
        req.versions.as_str(),
        req.include_audit,
        &fields,
        note,
        file_count,
        i64::try_from(size).unwrap_or(i64::MAX),
        &package_hash.0[..],
        &manifest_sha[..],
        core.storage.backend()
    )
    .execute(&mut *tx)
    .await?;
    let mut events = vec![
        AuditEvent::new(
            Category::Access,
            "export_package.created",
            ctx.actor.clone(),
            "export_package",
        )
        .object(id, Some(&reference))
        .context("request_reference", reference.clone())
        .context("documents", i64::try_from(ids.len()).unwrap_or(i64::MAX))
        .context(
            "document_ids",
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        )
        .context("versions", req.versions.as_str())
        .context("include_audit", req.include_audit)
        .context("fields", fields.join(" "))
        .context("package_sha256", package_hash.hex())
        .context("manifest_sha256", hex::encode(manifest_sha)),
    ];
    for d in &docs {
        events.push(
            d.event(ctx, Category::Access, "document.exported")
                .context("request_reference", reference.clone())
                .context("package_id", id)
                .context("versions", req.versions.as_str())
                .context("include_audit", req.include_audit),
        );
    }
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(id)
}

/// Open a package for download (the requester, or an auditor). Audited.
pub async fn open(core: &Core, ctx: &Ctx, id: Uuid) -> CoreResult<(PackageInfo, ByteStream)> {
    let user = ctx.user_id().ok_or(CoreError::Unauthenticated)?;
    let p = load(core, Some(id), None)
        .await?
        .into_iter()
        .next()
        .ok_or(CoreError::NotFound)?;
    if p.requested_by != user && !ctx.can(Capability::ViewAudit) {
        pdms_audit::record(
            &core.db,
            vec![
                AuditEvent::new(
                    Category::Access,
                    "access.denied",
                    ctx.actor.clone(),
                    "export_package",
                )
                .object(id, Some(&p.request_reference))
                .outcome(pdms_audit::Outcome::Denied),
            ],
        )
        .await?;
        return Err(CoreError::Forbidden(None));
    }
    let hash = BlobHash::from_hex(&p.package_sha256)
        .ok_or_else(|| CoreError::internal("bad package hash"))?;
    let stream = core.storage.get(hash).await.map_err(CoreError::internal)?;
    pdms_audit::record(
        &core.db,
        vec![
            AuditEvent::new(
                Category::Access,
                "export_package.downloaded",
                ctx.actor.clone(),
                "export_package",
            )
            .object(id, Some(&p.request_reference))
            .context("request_reference", p.request_reference.clone()),
        ],
    )
    .await?;
    Ok((p, stream))
}

/// The fields offered in the UI, with whether each is on by default.
pub fn default_fields() -> BTreeMap<&'static str, bool> {
    FIELDS
        .iter()
        .map(|(k, _)| (*k, !matches!(*k, "created_at" | "updated_at")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_paths_are_safe() {
        assert_eq!(segment("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(
            segment("Signed copy (final).pdf"),
            "Signed copy _final_.pdf"
        );
        assert_eq!(segment("..."), "file");
        assert_eq!(segment("PAG-2026-000001"), "PAG-2026-000001");
    }
}
