#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 5: comments, text extraction and date suggestions, and the Outlook
//! calendar (Microsoft Graph) provider.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{Duration, NaiveDate, Utc};
use common::*;
use futures_util::future::BoxFuture;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::appointments::{self, NewAppointment, PositionSpec};
use pdms_core::config::Config;
use pdms_core::documents::search::DocumentFilter;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::notifications::inbox;
use pdms_core::ocr::{self, OcrError, OcrProvider};
use pdms_core::{Core, CoreError, acl, comments, directory, graph_calendar, schedule};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.config).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

async fn new_doc(
    core: &Core,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    file: Option<(&str, Vec<u8>)>,
) -> Uuid {
    let t = types::type_by_key(&core.db, "partnership_agreement")
        .await
        .unwrap()
        .id;
    let file = match file {
        Some((name, bytes)) => Some(upload(core, name, &bytes).await),
        None => None,
    };
    documents::create(
        core,
        &system(),
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file,
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

async fn headlines(core: &Core, user: Uuid) -> Vec<String> {
    inbox::list(core, &as_user(core, user).await, true)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.headline)
        .collect()
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

#[tokio::test]
async fn comments_thread_mention_and_keep_every_revision() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "OPS", "owner@x.org").await;
    let gus = directory::create_user(&core.db, &system(), "Gus", "gus@x.org", Some(dept))
        .await
        .unwrap();
    let (outsider, _) = user_in_new_department(&core, "HR", "outsider@x.org").await;
    let admin = directory::create_user(&core.db, &system(), "Admin", "admin@x.org", None)
        .await
        .unwrap();
    let r = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), admin, r, None)
        .await
        .unwrap();
    // Two users share the handle "sam": a bare @sam is ambiguous and ignored.
    for e in ["sam@x.org", "sam@y.org"] {
        directory::create_user(&core.db, &system(), e, e, Some(dept))
            .await
            .unwrap();
    }
    let doc = new_doc(&core, dept, owner, "Equipment agreement", None).await;
    let o = as_user(&core, owner).await;
    let g = as_user(&core, gus).await;

    let c1 = comments::post(
        &core,
        &o,
        doc,
        None,
        "Can @gus check section 4? Also @outsider@x.org and @sam.",
    )
    .await
    .unwrap();
    // Gus (can view) was told; the outsider (cannot view) was not.
    assert!(
        headlines(&core, gus)
            .await
            .iter()
            .any(|h| h.starts_with("test user mentioned you: PAG-"))
    );
    assert!(headlines(&core, outsider).await.is_empty());
    let reply = comments::post(&core, &g, doc, Some(c1), "Section 4 is fine.")
        .await
        .unwrap();
    assert!(
        headlines(&core, owner)
            .await
            .iter()
            .any(|h| h.contains("replied to your comment"))
    );
    assert!(
        comments::post(&core, &g, doc, Some(reply), "nested")
            .await
            .is_err(),
        "threads are one level deep"
    );
    let threads = comments::list(&core, &o, doc).await.unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].replies.len(), 1);
    let mentioned: Vec<&str> = threads[0]
        .comment
        .mentions
        .iter()
        .map(|m| m.1.as_str())
        .collect();
    assert_eq!(mentioned, vec!["Gus", "outsider@x.org"]);

    // Outsiders cannot read or write.
    let x = as_user(&core, outsider).await;
    assert!(matches!(
        comments::list(&core, &x, doc).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        comments::post(&core, &x, doc, None, "hi").await,
        Err(CoreError::Forbidden(_))
    ));

    // Editing keeps the original; only the author may edit.
    assert!(matches!(
        comments::edit(&core, &o, reply, "changed").await,
        Err(CoreError::Forbidden(_))
    ));
    comments::edit(&core, &g, reply, "Section 4 is fine; see also @owner.")
        .await
        .unwrap();
    let h = comments::history(&core, &g, reply).await.unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].body, "Section 4 is fine.");
    assert!(
        headlines(&core, owner)
            .await
            .iter()
            .any(|h| h.contains("mentioned you")),
        "newly mentioned on edit"
    );
    // Others cannot see earlier versions; the document's administrators can.
    let colleague = directory::create_user(&core.db, &system(), "C", "c@x.org", Some(dept))
        .await
        .unwrap();
    assert!(matches!(
        comments::history(&core, &as_user(&core, colleague).await, reply).await,
        Err(CoreError::Forbidden(_))
    ));
    let a = as_user(&core, admin).await;
    assert_eq!(comments::history(&core, &a, reply).await.unwrap().len(), 2);

    // Deleting: authors only; the text stays in the history.
    assert!(matches!(
        comments::delete(&core, &as_user(&core, colleague).await, c1).await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(matches!(
        comments::delete(&core, &a, c1).await,
        Err(CoreError::Forbidden(_))
    ));
    comments::delete(&core, &o, c1).await.unwrap();
    let threads = comments::list(&core, &o, doc).await.unwrap();
    assert!(threads[0].comment.deleted_at.is_some() && threads[0].comment.body.is_empty());
    assert_eq!(threads[0].replies.len(), 1, "replies stay");
    assert!(
        comments::history(&core, &o, c1).await.unwrap()[0]
            .body
            .starts_with("Can @gus")
    );
    // Revisions cannot be altered, even directly through the app's role.
    assert!(
        sqlx::query("UPDATE comment_revisions SET body = 'x'")
            .execute(&core.db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM comment_revisions")
            .execute(&core.db)
            .await
            .is_err()
    );

    let audit = audit_actions(&core).await;
    for a in [
        "comment.created",
        "comment.replied",
        "comment.edited",
        "comment.deleted",
        "comment.edit_denied",
        "comment.delete_denied",
    ] {
        assert!(audit.iter().any(|x| x == a), "missing {a}");
    }
    assert!(pdms_core::integrations::is_published("comment.created"));
    assert!(!pdms_core::integrations::is_published(
        "comment.edit_denied"
    ));
    assert!(!pdms_core::integrations::is_published(
        "comment.post_denied"
    ));
}

async fn make_admin(core: &Core, email: &str) -> Uuid {
    let admin = directory::create_user(&core.db, &system(), email, email, None)
        .await
        .unwrap();
    let r = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), admin, r, None)
        .await
        .unwrap();
    admin
}

#[tokio::test]
async fn county_wide_comments_are_owner_and_admin_only() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "OPS", "owner@x.org").await;
    let colleague = directory::create_user(&core.db, &system(), "C", "c@x.org", Some(dept))
        .await
        .unwrap();
    let (outsider, _) = user_in_new_department(&core, "HR", "outsider@x.org").await;
    let admin = make_admin(&core, "admin@x.org").await;
    // An admin role scoped to one department is not a PADM administrator.
    let dept_admin = directory::create_user(&core.db, &system(), "DA", "da@x.org", Some(dept))
        .await
        .unwrap();
    let admin_role = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), dept_admin, admin_role, Some(dept))
        .await
        .unwrap();
    let o = as_user(&core, owner).await;
    let a = as_user(&core, admin).await;
    let c = as_user(&core, colleague).await;
    let x = as_user(&core, outsider).await;
    let da = as_user(&core, dept_admin).await;

    // Department document: the colleague comments, as before.
    let doc = new_doc(&core, dept, owner, "Dept doc", None).await;
    assert!(comments::can_comment(&core, &c, doc).await.unwrap());
    let earlier = comments::post(&core, &c, doc, None, "from a colleague")
        .await
        .unwrap();

    // It becomes County-wide.
    documents::set_visibility(&core, &o, doc, pdms_authz::Visibility::CountyWide)
        .await
        .unwrap();
    for (who, allowed) in [
        (&o, true),
        (&a, true),
        (&c, false),
        (&x, false),
        (&da, false),
    ] {
        assert_eq!(
            comments::can_comment(&core, who, doc).await.unwrap(),
            allowed
        );
    }
    let by_owner = comments::post(&core, &o, doc, None, "owner note")
        .await
        .unwrap();
    comments::post(&core, &a, doc, Some(by_owner), "admin reply")
        .await
        .unwrap();
    assert!(matches!(
        comments::post(&core, &da, doc, None, "scoped admin").await,
        Err(CoreError::Forbidden(_))
    ));
    for who in [&c, &x] {
        assert!(matches!(
            comments::post(&core, who, doc, None, "hello").await,
            Err(CoreError::Forbidden(_))
        ));
        assert!(matches!(
            comments::post(&core, who, doc, Some(by_owner), "reply").await,
            Err(CoreError::Forbidden(_))
        ));
        assert_eq!(comments::list(&core, who, doc).await.unwrap().len(), 2);
    }
    let audit = audit_actions(&core).await;
    assert!(audit.iter().any(|x| x == "comment.post_denied"));
    // Refused posts: outcome, reason, no comment_id; refused replies carry parent_id.
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT outcome::text, context FROM audit_log WHERE action = 'comment.post_denied' ORDER BY seq",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5, "colleague x2, outsider x2, scoped admin x1");
    for (outcome, ctxt) in &rows {
        assert_eq!(outcome, "denied");
        assert_eq!(ctxt["reason"], "county_wide_owner_or_admin_only");
        assert!(ctxt.get("comment_id").is_none(), "{ctxt}");
    }
    let replies = rows
        .iter()
        .filter(|(_, c)| c.get("parent_id").is_some())
        .count();
    assert_eq!(replies, 2);
    assert!(
        rows.iter()
            .filter(|(_, c)| c.get("parent_id").is_some())
            .all(|(_, c)| c["parent_id"] == by_owner.to_string())
    );

    // The colleague's earlier comment: can delete, can no longer edit.
    let listed = comments::list(&core, &c, doc).await.unwrap();
    let mine = listed.iter().find(|t| t.comment.id == earlier).unwrap();
    assert!(mine.comment.can_delete && !mine.comment.can_edit);
    assert!(matches!(
        comments::edit(&core, &c, earlier, "changed").await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        audit_actions(&core)
            .await
            .iter()
            .any(|x| x == "comment.edit_denied")
    );
    // The owner can still edit their own.
    let listed = comments::list(&core, &o, doc).await.unwrap();
    assert!(
        listed
            .iter()
            .find(|t| t.comment.id == by_owner)
            .unwrap()
            .comment
            .can_edit
    );
    comments::delete(&core, &c, earlier).await.unwrap();

    // Back to Department: the colleague may comment again.
    documents::set_visibility(&core, &o, doc, pdms_authz::Visibility::Department)
        .await
        .unwrap();
    comments::post(&core, &c, doc, None, "back again")
        .await
        .unwrap();
}

#[tokio::test]
async fn only_authors_delete_comments() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "OPS", "owner@x.org").await;
    let author = directory::create_user(&core.db, &system(), "A", "a@x.org", Some(dept))
        .await
        .unwrap();
    let manager = directory::create_user(&core.db, &system(), "M", "m@x.org", Some(dept))
        .await
        .unwrap();
    let admin = make_admin(&core, "admin@x.org").await;
    let doc = new_doc(&core, dept, owner, "Doc", None).await;
    documents::share(
        &core,
        &system(),
        doc,
        Subject::User(manager),
        Permission::ManagePermissions,
        None,
        "t",
    )
    .await
    .unwrap();
    let au = as_user(&core, author).await;
    let c = comments::post(&core, &au, doc, None, "mine").await.unwrap();
    // Prove the grant applies: managers may read earlier versions.
    comments::edit(&core, &au, c, "mine, edited").await.unwrap();
    let mgr = as_user(&core, manager).await;
    assert!(
        comments::history(&core, &mgr, c).await.is_ok(),
        "the manager holds manage_permissions"
    );
    assert!(
        comments::list(&core, &mgr, doc).await.unwrap()[0]
            .comment
            .can_see_history
    );
    for who in [admin, owner, manager] {
        let ctx = as_user(&core, who).await;
        assert!(matches!(
            comments::delete(&core, &ctx, c).await,
            Err(CoreError::Forbidden(_))
        ));
        let t = comments::list(&core, &ctx, doc).await.unwrap();
        assert!(!t[0].comment.can_delete);
    }
    assert!(
        comments::list(&core, &au, doc).await.unwrap()[0]
            .comment
            .can_delete
    );
    assert!(
        audit_actions(&core)
            .await
            .iter()
            .any(|x| x == "comment.delete_denied")
    );
    comments::delete(&core, &au, c).await.unwrap();
}

#[tokio::test]
async fn commenting_can_require_edit() {
    let (core, _db) = core_with(|c| c.comments.viewers_can_comment = false).await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let viewer = directory::create_user(&core.db, &system(), "V", "v@x.org", Some(dept))
        .await
        .unwrap();
    let doc = new_doc(&core, dept, owner, "Doc", None).await;
    assert!(matches!(
        comments::post(&core, &as_user(&core, viewer).await, doc, None, "hi").await,
        Err(CoreError::Forbidden(_))
    ));
    assert!(
        comments::list(&core, &as_user(&core, viewer).await, doc)
            .await
            .is_ok(),
        "still readable"
    );
    comments::post(&core, &as_user(&core, owner).await, doc, None, "hi")
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Text extraction and date suggestions
// ---------------------------------------------------------------------------

async fn run_extraction(core: &Core) -> Vec<Result<(), String>> {
    let jobs: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM jobs WHERE kind = 'documents.extract_text' AND status = 'pending' ORDER BY run_at, id",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    let mut out = Vec::new();
    for p in jobs {
        let doc: Uuid = p["document_id"].as_str().unwrap().parse().unwrap();
        let v = i32::try_from(p["version_no"].as_i64().unwrap()).unwrap();
        out.push(
            ocr::extract(core, doc, v, p["force"].as_bool().unwrap_or(false))
                .await
                .map_err(|e| e.to_string()),
        );
    }
    sqlx::query("UPDATE jobs SET status = 'succeeded' WHERE kind = 'documents.extract_text'")
        .execute(&core.db)
        .await
        .unwrap();
    out
}

fn docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opt = zip::write::SimpleFileOptions::default();
        z.start_file("[Content_Types].xml", opt).unwrap();
        z.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
        z.start_file("word/document.xml", opt).unwrap();
        let body: String = paragraphs
            .iter()
            .map(|p| format!("<w:p><w:r><w:t>{p}</w:t></w:r></w:p>"))
            .collect();
        z.write_all(format!(r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#).as_bytes()).unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

#[derive(Debug)]
struct FakeOcr {
    text: String,
    up: bool,
}

impl OcrProvider for FakeOcr {
    fn key(&self) -> &'static str {
        "fake"
    }
    fn ocr<'a>(
        &'a self,
        _path: &'a Path,
        _media: &'a str,
    ) -> BoxFuture<'a, Result<String, OcrError>> {
        Box::pin(async move {
            if self.up {
                Ok(self.text.clone())
            } else {
                Err(OcrError::Unavailable("connection refused".into()))
            }
        })
    }
}

#[tokio::test]
async fn files_are_read_searchable_and_dates_are_suggested_not_applied() {
    let (core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "OPS", "owner@x.org").await;
    let o = as_user(&core, owner).await;
    acl::grant(
        &core.db,
        &system(),
        Subject::User(owner),
        Scope::Department(dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    let pdf = pdms_core::demo::demo_pdf(&[
        "PARTNERSHIP AGREEMENT FOR WAREHOUSE MAINTENANCE",
        "This Agreement is made and entered into this 12th day of February, 2013.",
        "This Agreement shall be effective March 1, 2013 and shall continue",
        "for a term of five (5) years. The parties shall review it annually.",
    ]);
    let doc = new_doc(
        &core,
        dept,
        owner,
        "Warehouse agreement",
        Some(("agreement.pdf", pdf)),
    )
    .await;
    assert_eq!(run_extraction(&core).await, vec![Ok(())]);
    let t = ocr::latest_text(&core, &o, doc).await.unwrap().unwrap();
    assert_eq!(
        (t.status.as_str(), t.method.as_str()),
        ("done", "pdf_text"),
        "{t:?}"
    );
    assert!(t.preview.contains("WAREHOUSE"));
    // The file's words are searchable.
    let hits = pdms_core::documents::search::search(
        &core,
        &o,
        &DocumentFilter {
            q: Some("warehouse maintenance".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(hits.total, 1);

    // Suggestions only: the document's dates are untouched.
    let d = documents::view(&core, &o, doc).await.unwrap().document;
    assert_eq!(
        (d.execution_date, d.effective_date, d.expiration_date),
        (None, None, None)
    );
    let s = ocr::suggestions(&core, &o, doc).await.unwrap();
    let find = |f: &str| {
        s.iter()
            .find(|x| x.field == f)
            .unwrap_or_else(|| panic!("no {f}: {s:#?}"))
    };
    assert_eq!(find("execution_date").date, "2013-02-12".parse().ok());
    assert_eq!(find("effective_date").date, "2013-03-01".parse().ok());
    assert_eq!(find("expiration_date").date, "2018-02-28".parse().ok());
    assert_eq!(find("review_interval_months").months, Some(12));
    assert!(
        headlines(&core, owner)
            .await
            .iter()
            .any(|h| h.starts_with("Dates found to confirm"))
    );

    // Viewers cannot decide; editors accept or dismiss.
    let viewer = directory::create_user(&core.db, &system(), "V", "v@x.org", Some(dept))
        .await
        .unwrap();
    let eff = find("effective_date").id;
    assert!(matches!(
        ocr::accept(&core, &as_user(&core, viewer).await, eff).await,
        Err(CoreError::Forbidden(_))
    ));
    ocr::accept(&core, &o, eff).await.unwrap();
    assert!(matches!(
        ocr::accept(&core, &o, eff).await,
        Err(CoreError::Conflict(_))
    ));
    ocr::accept(&core, &o, find("review_interval_months").id)
        .await
        .unwrap();
    ocr::dismiss(&core, &o, find("expiration_date").id)
        .await
        .unwrap();
    let d = documents::view(&core, &o, doc).await.unwrap().document;
    assert_eq!(d.effective_date, "2013-03-01".parse().ok());
    assert_eq!(d.review_interval_months, Some(12));
    assert_eq!(d.expiration_date, None, "dismissed");
    let timeline = documents::timeline(&core, &o, doc).await.unwrap();
    let edit = timeline
        .iter()
        .find(|e| {
            e.event.action == "document.metadata_updated"
                && format!("{:?}", e.event.changes).contains("effective_date")
        })
        .unwrap();
    assert_eq!(edit.event.actor.user_id, Some(owner));
    let audit = audit_actions(&core).await;
    for a in [
        "document.text_extracted",
        "date_suggestion.accepted",
        "date_suggestion.dismissed",
    ] {
        assert!(audit.iter().any(|x| x == a), "missing {a}");
    }

    // Word documents; an image without OCR; the same file again is not re-read.
    let w = new_doc(
        &core,
        dept,
        owner,
        "Lease",
        Some((
            "lease.docx",
            docx(&["Lease", "This lease expires on December 31, 2030."]),
        )),
    )
    .await;
    let img = new_doc(
        &core,
        dept,
        owner,
        "Scan",
        Some((
            "scan.png",
            std::fs::read("tests/fixtures/scan.png").unwrap(),
        )),
    )
    .await;
    run_extraction(&core).await;
    let wt = ocr::latest_text(&core, &o, w).await.unwrap().unwrap();
    assert_eq!(wt.method, "docx");
    assert!(
        ocr::suggestions(&core, &o, w)
            .await
            .unwrap()
            .iter()
            .any(|s| s.date == "2030-12-31".parse().ok())
    );
    let it = ocr::latest_text(&core, &o, img).await.unwrap().unwrap();
    assert_eq!(it.status, "unsupported");

    // A scan that was already OCR'd (by a copier, say) has a text layer: no OCR needed.
    let ocred = new_doc(
        &core,
        dept,
        owner,
        "Copier scan",
        Some((
            "ocred.pdf",
            std::fs::read("tests/fixtures/ocred.pdf").unwrap(),
        )),
    )
    .await;
    run_extraction(&core).await;
    let t = ocr::latest_text(&core, &o, ocred).await.unwrap().unwrap();
    assert_eq!((t.status.as_str(), t.method.as_str()), ("done", "pdf_text"));
    assert!(
        ocr::suggestions(&core, &o, ocred)
            .await
            .unwrap()
            .iter()
            .any(|s| s.field == "expiration_date" && s.date == "2018-02-28".parse().ok())
    );
}

#[tokio::test]
async fn scanned_files_use_the_ocr_provider_and_retry_when_it_is_down() {
    let (mut core, _db) = core().await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let o = as_user(&core, owner).await;
    core.ocr = Some(Arc::new(FakeOcr {
        text: String::new(),
        up: false,
    }));
    let scanned = std::fs::read("tests/fixtures/scan.pdf").unwrap();
    let doc = new_doc(
        &core,
        dept,
        owner,
        "Scanned agreement",
        Some(("scan.pdf", scanned)),
    )
    .await;
    let r = run_extraction(&core).await;
    assert!(
        r[0].as_ref().is_err_and(|e| e.contains("unavailable")),
        "{r:?}"
    );
    assert!(
        ocr::latest_text(&core, &o, doc).await.unwrap().is_none(),
        "nothing stored; the job retries"
    );

    core.ocr = Some(Arc::new(FakeOcr {
        text:
            "This Agreement shall be effective March 1, 2013 and shall expire on February 28, 2018."
                .into(),
        up: true,
    }));
    ocr::reprocess(&core, &o, doc).await.unwrap();
    let r = run_extraction(&core).await;
    assert_eq!(r, vec![Ok(())]);
    let t = ocr::latest_text(&core, &o, doc).await.unwrap().unwrap();
    assert_eq!((t.status.as_str(), t.method.as_str()), ("done", "ocr"));
    let s = ocr::suggestions(&core, &o, doc).await.unwrap();
    assert!(
        s.iter()
            .any(|x| x.field == "expiration_date" && x.date == "2018-02-28".parse().ok())
    );

    // Extraction can be turned off entirely.
    let (off, _db2) = core_with(|c| c.ocr.text_extraction = false).await;
    let (u2, d2) = user_in_new_department(&off, "X", "u2@x.org").await;
    let doc2 = new_doc(
        &off,
        d2,
        u2,
        "Doc",
        Some(("a.pdf", pdf("Effective January 1, 2027"))),
    )
    .await;
    run_extraction(&off).await;
    assert!(
        ocr::latest_text(&off, &as_user(&off, u2).await, doc2)
            .await
            .unwrap()
            .is_none()
    );
}

/// The real OCR sidecar. Build it first:
/// `docker build -t pdms-ocr:test deploy/ocr`, then
/// `cargo test -p pdms-core --test phase5 -- --ignored`.
#[tokio::test]
#[ignore = "needs the pdms-ocr:test image"]
async fn real_ocr_sidecar_reads_a_scan() {
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{GenericImage, ImageExt};
    let c = GenericImage::new("pdms-ocr", "test")
        .with_exposed_port(8080.tcp())
        .with_wait_for(WaitFor::seconds(2))
        .with_label("pdms.testkit", "ocr")
        .start()
        .await
        .unwrap();
    let port = c.get_host_port_ipv4(8080).await.unwrap();
    let (core, _db) =
        core_with(|cfg| cfg.ocr.sidecar_url = format!("http://127.0.0.1:{port}")).await;
    let (owner, dept) = user_in_new_department(&core, "IT", "o@x.org").await;
    let doc = new_doc(
        &core,
        dept,
        owner,
        "Scan",
        Some((
            "scan.pdf",
            std::fs::read("tests/fixtures/scan.pdf").unwrap(),
        )),
    )
    .await;
    assert_eq!(run_extraction(&core).await, vec![Ok(())]);
    let o = as_user(&core, owner).await;
    let t = ocr::latest_text(&core, &o, doc).await.unwrap().unwrap();
    assert_eq!(t.method, "ocr");
    assert!(
        t.preview.contains("effective March 1, 2013"),
        "{}",
        t.preview
    );
    let s = ocr::suggestions(&core, &o, doc).await.unwrap();
    assert!(
        s.iter()
            .any(|x| x.field == "expiration_date" && x.date == "2018-02-28".parse().ok())
    );
    assert!(
        s.iter()
            .any(|x| x.field == "review_interval_months" && x.months == Some(12))
    );
}

// ---------------------------------------------------------------------------
// Outlook calendar through Microsoft Graph (against a mock Graph)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Graph {
    tokens: usize,
    next: usize,
    /// event id -> (mailbox, body)
    events: HashMap<String, (String, serde_json::Value)>,
    requests: Vec<String>,
}

async fn mock_graph() -> (String, Arc<Mutex<Graph>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(Mutex::new(Graph::default()));
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let st = st.clone();
            tokio::spawn(async move {
                loop {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let (head_end, len) = loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                            let len = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            break (i + 4, len);
                        }
                    };
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut first = head.lines().next().unwrap_or("").split(' ');
                    let (method, path) = (
                        first.next().unwrap_or("").to_owned(),
                        first.next().unwrap_or("").to_owned(),
                    );
                    let body = buf[head_end..head_end + len].to_vec();
                    let (status, out) = {
                        let mut g = st.lock().unwrap();
                        g.requests.push(format!("{method} {path}"));
                        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
                        match (method.as_str(), parts.as_slice()) {
                            ("POST", [_tenant, "oauth2", "v2.0", "token"]) => {
                                g.tokens += 1;
                                (
                                    200,
                                    r#"{"access_token":"tok","expires_in":3600}"#.to_owned(),
                                )
                            }
                            ("POST", ["users", mailbox, "calendar", "events"]) => {
                                g.next += 1;
                                let id = format!("evt-{}", g.next);
                                let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                                g.events
                                    .insert(id.clone(), ((*mailbox).replace("%40", "@"), v));
                                (201, format!(r#"{{"id":"{id}"}}"#))
                            }
                            ("PATCH", ["users", _, "events", id]) => match g.events.get_mut(*id) {
                                Some(e) => {
                                    e.1 = serde_json::from_slice(&body).unwrap();
                                    (200, "{}".to_owned())
                                }
                                None => (404, "{}".to_owned()),
                            },
                            ("DELETE", ["users", _, "events", id]) => match g.events.remove(*id) {
                                Some(_) => (204, String::new()),
                                None => (404, "{}".to_owned()),
                            },
                            _ => (400, "{}".to_owned()),
                        }
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{out}",
                        out.len()
                    );
                    if sock.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (format!("http://{addr}"), state)
}

fn subjects(g: &Arc<Mutex<Graph>>) -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = g
        .lock()
        .unwrap()
        .events
        .values()
        .map(|(mb, e)| {
            (
                mb.clone(),
                e["subject"].as_str().unwrap().to_owned(),
                e["start"]["dateTime"].as_str().unwrap()[..10].to_owned(),
            )
        })
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn outlook_events_follow_dates_through_graph() {
    let (base, graph) = mock_graph().await;
    let (core, _db) = core_with(|c| {
        let g = &mut c.graph_calendar;
        g.enabled = true;
        g.tenant_id = "tenant".into();
        g.client_id = "client".into();
        g.client_secret = pdms_core::config::Secret::new("secret");
        g.mailbox = "pdms@example.org".into();
        g.department_mailboxes
            .insert("OPS".into(), "ops@example.org".into());
        g.login_base_url = base.clone();
        g.graph_base_url = base.clone();
    })
    .await;
    // Disabled installations do nothing.
    let (off, _db2) = common::core().await;
    assert_eq!(
        graph_calendar::sync(&off).await.unwrap(),
        Default::default()
    );

    let today = schedule::local_today(Utc::now(), core.config.timezone());
    let (owner, ops) = user_in_new_department(&core, "OPS", "o@x.org").await;
    let hr = directory::create_department(&core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    let mk = |dept: Uuid, title: &'static str, renewal: NaiveDate| {
        let core = core.clone();
        async move {
            let t = types::type_by_key(&core.db, "contract").await.unwrap().id;
            documents::create(
                &core,
                &system(),
                NewDocument {
                    visibility: Default::default(),
                    metadata: MetadataInput {
                        title: title.into(),
                        document_type_id: t,
                        department_id: dept,
                        renewal_date: Some(renewal),
                        ..Default::default()
                    },
                    owner_id: owner,
                    status: DocumentStatus::Active,
                    tags: vec![],
                    file: None,
                    note: String::new(),
                },
            )
            .await
            .unwrap()
        }
    };
    let asphalt = mk(ops, "Asphalt", today + Duration::days(40)).await;
    let _copier = mk(hr, "Copier lease", today + Duration::days(50)).await;
    let secret = mk(hr, "Settlement", today + Duration::days(60)).await;
    documents::set_visibility(&core, &system(), secret, pdms_authz::Visibility::Restricted)
        .await
        .unwrap();
    let _far = mk(hr, "Far future", today + Duration::days(5000)).await;
    let pos = appointments::create_position(
        &core,
        &system(),
        PositionSpec {
            name: "Seat 1".into(),
            body_name: "Program Board".into(),
            department_id: Some(hr),
            term_months: Some(24),
            seats: 1,
            reappointment_lead_days: 30,
            is_active: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    appointments::appoint(
        &core,
        &system(),
        pos,
        NewAppointment {
            person_name: "Jane Doe".into(),
            start_date: today - Duration::days(600),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let r = graph_calendar::sync(&core).await.unwrap();
    assert_eq!((r.created, r.updated, r.deleted), (4, 0, 0), "{r:?}");
    let ev = subjects(&graph);
    let asphalt_date = (today + Duration::days(40)).to_string();
    assert!(
        ev.iter().any(|e| e.0 == "ops@example.org"
            && e.1.starts_with("Renewal due: CON-")
            && e.1.ends_with("Asphalt")
            && e.2 == asphalt_date),
        "{ev:?}"
    );
    assert!(
        ev.iter()
            .any(|e| e.0 == "pdms@example.org" && e.1.contains("Copier lease"))
    );
    assert!(ev.iter().any(|e| {
        e.1.starts_with("Term ends: Jane Doe (Seat 1, Program Board)")
    }));
    assert!(
        ev.iter()
            .any(|e| e.1.starts_with("Reappointment decision due: Jane Doe"))
    );
    assert!(
        !ev.iter().any(|e| e.1.contains("Settlement")),
        "Restricted documents are never sent"
    );
    assert!(
        !ev.iter().any(|e| e.1.contains("Far future")),
        "outside the window"
    );
    {
        let g = graph.lock().unwrap();
        let (_, body) = g.events.values().next().unwrap();
        assert_eq!(body["isAllDay"], true);
        assert!(
            body["body"]["content"]
                .as_str()
                .unwrap()
                .contains("Open in PDMS")
        );
    }

    // Nothing changed: nothing sent.
    let before = graph.lock().unwrap().requests.len();
    let r = graph_calendar::sync(&core).await.unwrap();
    assert_eq!((r.created, r.updated, r.deleted, r.unchanged), (0, 0, 0, 4));
    assert_eq!(graph.lock().unwrap().requests.len(), before);
    assert_eq!(graph.lock().unwrap().tokens, 1, "token reused");

    // A changed date updates the event; archiving removes it; an event deleted
    // in Outlook is recreated when its date changes.
    let d = documents::view(&core, &system(), asphalt)
        .await
        .unwrap()
        .document;
    let mut m = MetadataInput {
        title: d.title.clone(),
        document_type_id: d.document_type_id,
        department_id: d.department_id,
        renewal_date: Some(today + Duration::days(45)),
        ..Default::default()
    };
    documents::update(&core, &system(), asphalt, m.clone(), d.row_version)
        .await
        .unwrap();
    documents::change_status(&core, &system(), _copier, DocumentStatus::Archived, "")
        .await
        .unwrap();
    let r = graph_calendar::sync(&core).await.unwrap();
    assert_eq!((r.created, r.updated, r.deleted), (0, 1, 1), "{r:?}");
    assert!(
        subjects(&graph)
            .iter()
            .any(|e| e.1.contains("Asphalt") && e.2 == (today + Duration::days(45)).to_string())
    );
    assert!(!subjects(&graph).iter().any(|e| e.1.contains("Copier")));
    graph
        .lock()
        .unwrap()
        .events
        .retain(|_, (_, e)| !e["subject"].as_str().unwrap().contains("Asphalt"));
    let d = documents::view(&core, &system(), asphalt)
        .await
        .unwrap()
        .document;
    m.renewal_date = Some(today + Duration::days(46));
    documents::update(&core, &system(), asphalt, m, d.row_version)
        .await
        .unwrap();
    let r = graph_calendar::sync(&core).await.unwrap();
    assert_eq!(
        (r.created, r.updated),
        (1, 0),
        "recreated after a 404: {r:?}"
    );
    assert!(
        audit_actions(&core)
            .await
            .contains(&"calendar.graph_synced".to_owned())
    );
}
