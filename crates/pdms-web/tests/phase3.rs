#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Phase 3 pages: the workflow and signature panels, positions and
//! appointments, and their place on the dashboard, calendar, and inbox.

mod support;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::{acl, directory, schedule};
use support::*;
use uuid::Uuid;

fn re(pattern: &str, body: &str) -> String {
    regex::Regex::new(pattern)
        .unwrap()
        .captures(body)
        .unwrap_or_else(|| panic!("{pattern} not found"))[1]
        .to_owned()
}

#[tokio::test]
async fn documents_are_submitted_approved_and_signed_through_the_page() {
    let app = app().await;
    let dept =
        directory::create_department(&app.core.db, &system(), "IT", "Information Technology")
            .await
            .unwrap();
    let (owner, owner_secret) = user(&app.core, "owner@x.org", Some(dept), None).await;
    let (head, head_secret) = user(&app.core, "head@x.org", Some(dept), None).await;
    let (lawyer, lawyer_secret) =
        user(&app.core, "lawyer@x.org", None, Some("legal_counsel")).await;
    let role = directory::role_id_by_key(&app.core.db, "department_head")
        .await
        .unwrap();
    directory::assign_role(&app.core.db, &system(), head, role, Some(dept))
        .await
        .unwrap();
    acl::grant(
        &app.core.db,
        &system(),
        Subject::User(owner),
        Scope::Department(dept),
        Permission::Edit,
        None,
        "t",
    )
    .await
    .unwrap();
    let t = types::type_by_key(&app.core.db, "contract")
        .await
        .unwrap()
        .id;
    let doc = documents::create(
        &app.core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: "Copier lease".into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Draft,
            tags: vec![],
            file: None,
            note: String::new(),
        },
    )
    .await
    .unwrap();

    let mut o = Browser::new(&app);
    o.login("owner@x.org", &owner_secret).await;
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("Submit for review and approval"));
    assert!(
        page.body.contains("requires a legal counsel review"),
        "contracts require legal review"
    );
    assert!(
        !page.body.contains("Move to In Review"),
        "review starts by submitting, not by a status button"
    );
    let csrf = page.csrf();
    let head_opt = format!("user:{head}");
    let r = o
        .post(
            &format!("/documents/{doc}/workflow"),
            &[
                ("_csrf", csrf.as_str()),
                ("approver", head_opt.as_str()),
                ("note", "Ready for the board"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("Submitted for review."), "flash");
    assert!(page.body.contains("In Review"));
    assert!(page.body.contains("Legal review") && page.body.contains("Any Legal Counsel"));
    assert!(!page.body.contains("Submit for review and approval"));

    // The lawyer sees it on the dashboard and approves from the page.
    let mut l = Browser::new(&app);
    l.login("lawyer@x.org", &lawyer_secret).await;
    let dash = l.get("/").await;
    assert!(dash.body.contains("Awaiting my review or approval"));
    assert!(dash.body.contains("Copier lease"));
    let page = l.get(&format!("/documents/{doc}")).await;
    let step = re(r"/workflow/steps/([0-9a-f-]{36})", &page.body);
    // A rejection needs a comment.
    let r = l
        .post(
            &format!("/documents/{doc}/workflow/steps/{step}"),
            &[("_csrf", page.csrf().as_str()), ("decision", "reject")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        l.get(&format!("/documents/{doc}"))
            .await
            .body
            .contains("explain what is wrong")
    );
    l.post(
        &format!("/documents/{doc}/workflow/steps/{step}"),
        &[
            ("_csrf", page.csrf().as_str()),
            ("decision", "approve"),
            ("comment", "Approved as to form"),
        ],
    )
    .await;
    assert!(
        l.get(&format!("/documents/{doc}"))
            .await
            .body
            .contains("Approved as to form")
    );

    // The owner cannot approve (no permission, and not the assignee); the head can.
    let mut h = Browser::new(&app);
    h.login("head@x.org", &head_secret).await;
    let inbox = h.get("/notifications").await;
    assert!(
        inbox.body.contains("Approval requested: CON-"),
        "{}",
        inbox.body
    );
    let page = h.get(&format!("/documents/{doc}")).await;
    let step = re(r"/workflow/steps/([0-9a-f-]{36})", &page.body);
    let r = o
        .post(
            &format!("/documents/{doc}/workflow/steps/{step}"),
            &[("_csrf", csrf.as_str()), ("decision", "approve")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    h.post(
        &format!("/documents/{doc}/workflow/steps/{step}"),
        &[("_csrf", page.csrf().as_str()), ("decision", "approve")],
    )
    .await;
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("Approved by everyone; now Active"));
    assert_eq!(
        documents::view(&app.core, &system(), doc)
            .await
            .unwrap()
            .document
            .status,
        DocumentStatus::Active
    );
    assert!(
        o.get("/notifications")
            .await
            .body
            .contains("Approved: CON-")
    );

    // Signatures: add the head (a user) and record a signed copy by upload.
    let head_id = head.to_string();
    o.post(
        &format!("/documents/{doc}/signatories"),
        &[
            ("_csrf", csrf.as_str()),
            ("user", head_id.as_str()),
            ("name", ""),
            ("title", "Department Head"),
        ],
    )
    .await;
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(
        page.body.contains("head@x.org, Department Head"),
        "name from the user"
    );
    let sig = re(r"/signatories/([0-9a-f-]{36})/request", &page.body);
    o.post(
        &format!("/documents/{doc}/signatories/{sig}/request"),
        &[("_csrf", csrf.as_str())],
    )
    .await;
    assert!(
        h.get("/notifications")
            .await
            .body
            .contains("Signature requested")
    );
    let today = schedule::local_today(Utc::now(), app.core.config.timezone()).to_string();
    let r = o
        .multipart(
            &format!("/documents/{doc}/signatories/{sig}/signed"),
            &[
                ("_csrf", csrf.as_str()),
                ("signed_on", today.as_str()),
                ("note", "Wet ink"),
            ],
            Some(("signed.pdf", b"%PDF-1.4\n% signed\n%%EOF\n")),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = o.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("Signature recorded."));
    assert!(page.body.contains("1 of 1 signed"));
    assert!(page.body.contains("Signed copy (version"));
    // The multipart route checks its token first.
    let r = o
        .multipart(
            &format!("/documents/{doc}/signatories/{sig}/signed"),
            &[("signed_on", today.as_str())],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let _ = lawyer;
}

#[tokio::test]
async fn positions_and_appointments_through_the_pages() {
    let app = app().await;
    let dept = directory::create_department(&app.core.db, &system(), "CC", "Board of Directors")
        .await
        .unwrap();
    let (_, clerk_secret) = user(&app.core, "clerk@x.org", None, Some("appointments_clerk")).await;
    let (manager, manager_secret) = user(&app.core, "manager@x.org", Some(dept), None).await;
    let (_, outsider_secret) = user(&app.core, "outsider@x.org", None, None).await;

    let mut c = Browser::new(&app);
    c.login("clerk@x.org", &clerk_secret).await;
    let list = c.get("/positions").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains("Add a position"));
    let csrf = list.csrf();
    let dept_s = dept.to_string();
    let manager_s = manager.to_string();
    let r = c
        .post(
            "/positions",
            &[
                ("_csrf", csrf.as_str()),
                ("body_name", "Program Board"),
                ("name", "Trustee 2"),
                ("appointing_authority", "Board of Directors"),
                ("term_months", "24"),
                ("seats", "1"),
                ("reappointment_lead_days", "60"),
                ("department_id", dept_s.as_str()),
                ("manager_user_id", manager_s.as_str()),
                ("description", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let pos = r.location().trim_start_matches("/positions/").to_owned();
    c.post(
        &format!("/positions/{pos}/requirements"),
        &[
            ("_csrf", csrf.as_str()),
            ("label", "Acceptance letter on file"),
        ],
    )
    .await;
    let start = (schedule::local_today(Utc::now(), app.core.config.timezone())
        - Duration::days(700))
    .to_string();
    let r = c
        .post(
            &format!("/positions/{pos}/appointments"),
            &[
                ("_csrf", csrf.as_str()),
                ("person_name", "Robin Reader"),
                ("start_date", start.as_str()),
                ("appointment_reference", "Resolution 12"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = c.get(&format!("/positions/{pos}")).await;
    assert!(page.body.contains("Appointment recorded."), "{}", page.body);
    assert!(page.body.contains("Robin Reader"));
    assert!(page.body.contains("Reappointment decision due"));
    assert!(page.body.contains("0 of 1 satisfied"));
    assert!(
        !page.body.contains("Appoint someone"),
        "the only seat is filled"
    );

    // The manager ticks the checklist; they cannot change the setup.
    let mut m = Browser::new(&app);
    m.login("manager@x.org", &manager_secret).await;
    let page = m.get(&format!("/positions/{pos}")).await;
    assert!(page.body.contains("Mark done"));
    assert!(!page.body.contains("Edit position"));
    let check = re(
        r#"action="(/positions/[^"]+/requirements/[0-9a-f-]{36})""#,
        &page.body,
    );
    let r = m
        .post(
            &check,
            &[
                ("_csrf", page.csrf().as_str()),
                ("satisfied", "true"),
                ("note", "Filed 3/1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = m.get(&format!("/positions/{pos}")).await;
    assert!(page.body.contains("1 of 1 satisfied"));
    assert!(page.body.contains("by manager@x.org on"));
    let r = m
        .post(
            &format!("/positions/{pos}/requirements"),
            &[("_csrf", page.csrf().as_str()), ("label", "Sneaky")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // The term ends within 60 days: on the manager's calendar agenda.
    let agenda = m.get("/calendar?view=agenda&days=90").await;
    assert!(
        agenda.body.contains("Robin Reader — Trustee 2"),
        "{}",
        agenda.body
    );
    assert!(agenda.body.contains(&format!("/positions/{pos}")));
    let dash = m.get("/?days=90").await;
    assert!(dash.body.contains("Robin Reader — Trustee 2"));

    // Reappoint through the form; the history keeps the first term.
    let clerk_page = c.get(&format!("/positions/{pos}")).await;
    let appt = re(r"/appointments/([0-9a-f-]{36})/reappoint", &clerk_page.body);
    c.post(
        &format!("/positions/{pos}/appointments/{appt}/reappoint"),
        &[
            ("_csrf", csrf.as_str()),
            ("appointment_reference", "Resolution 99"),
        ],
    )
    .await;
    let page = c.get(&format!("/positions/{pos}")).await;
    assert!(
        page.body.contains("Reappointed for a new term."),
        "{}",
        page.body
    );
    assert!(page.body.contains("(reappointed)"));
    assert!(page.body.contains("Resolution 99"));

    // Outsiders see nothing.
    let mut x = Browser::new(&app);
    x.login("outsider@x.org", &outsider_secret).await;
    assert!(
        x.get("/positions")
            .await
            .body
            .contains("no positions you can see")
    );
    assert_eq!(
        x.get(&format!("/positions/{pos}")).await.status,
        StatusCode::FORBIDDEN
    );
    let _: Uuid = pos.parse().unwrap();
}
