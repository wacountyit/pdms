// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Demo dataset for evaluation and development. Never run against production.

use pdms_authz::{Permission, Scope, Subject, role_keys};

use crate::context::Ctx;
use crate::error::CoreResult;
use crate::{Core, acl, directory};

pub const DEMO_PASSWORD: &str = "demo-password-change-me";

#[derive(Debug)]
pub struct DemoSummary {
    pub created: bool,
    /// (email, description)
    pub users: Vec<(String, String)>,
}

struct DemoUser {
    name: &'static str,
    email: &'static str,
    dept: &'static str,
    role: Option<(&'static str, bool)>,
    note: &'static str,
}

const DEPARTMENTS: &[(&str, &str)] = &[
    ("IT", "Information Technology"),
    ("OPS", "Operations"),
    ("HR", "Human Resources"),
    ("LEGAL", "Legal Counsel"),
    ("BOARD", "Board Office"),
];

const USERS: &[DemoUser] = &[
    DemoUser {
        name: "Avery Admin",
        email: "admin@example.org",
        dept: "IT",
        role: Some((role_keys::ADMIN, false)),
        note: "administrator",
    },
    DemoUser {
        name: "Alice Ops",
        email: "alice@example.org",
        dept: "OPS",
        role: Some((role_keys::DEPARTMENT_HEAD, true)),
        note: "Operations department head",
    },
    DemoUser {
        name: "Bob Personnel",
        email: "bob@example.org",
        dept: "HR",
        role: None,
        note: "HR staff (read-only default)",
    },
    DemoUser {
        name: "Carol Counsel",
        email: "carol@example.org",
        dept: "LEGAL",
        role: Some((role_keys::LEGAL_COUNSEL, false)),
        note: "legal counsel",
    },
    DemoUser {
        name: "Dana Records",
        email: "dana@example.org",
        dept: "BOARD",
        role: Some((role_keys::RECORDS_OFFICER, false)),
        note: "records officer",
    },
    DemoUser {
        name: "Erin Auditor",
        email: "erin@example.org",
        dept: "IT",
        role: Some((role_keys::AUDITOR, false)),
        note: "auditor",
    },
    DemoUser {
        name: "Jordan Presiding",
        email: "jordan@example.org",
        dept: "BOARD",
        role: Some((role_keys::DEPARTMENT_HEAD, false)),
        note: "presiding officer (approves and signs for every department)",
    },
    DemoUser {
        name: "Gus Engineer",
        email: "gus@example.org",
        dept: "OPS",
        role: None,
        note: "Operations engineer (reviewer)",
    },
    DemoUser {
        name: "Frank Appointments",
        email: "frank@example.org",
        dept: "BOARD",
        role: Some((role_keys::APPOINTMENTS_CLERK, false)),
        note: "appointments clerk (boards and commissions)",
    },
];

/// Seed the demo dataset. Idempotent: does nothing if the IT department exists.
pub async fn seed(core: &Core) -> CoreResult<DemoSummary> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM departments WHERE code = 'IT') AS "e!""#
    )
    .fetch_one(&core.db)
    .await?;
    let users: Vec<(String, String)> = USERS
        .iter()
        .map(|u| (u.email.to_owned(), u.note.to_owned()))
        .collect();
    if exists {
        return Ok(DemoSummary {
            created: false,
            users,
        });
    }

    let ctx = Ctx::system("seed-demo");
    let db = &core.db;
    let mut dept_ids = std::collections::HashMap::new();
    for (code, name) in DEPARTMENTS {
        dept_ids.insert(
            *code,
            directory::create_department(db, &ctx, code, name).await?,
        );
    }
    let dept = |code: &str| {
        dept_ids
            .get(code)
            .copied()
            .ok_or(crate::CoreError::NotFound)
    };

    let mut user_ids = std::collections::HashMap::new();
    for u in USERS {
        let id = directory::create_user(db, &ctx, u.name, u.email, Some(dept(u.dept)?)).await?;
        directory::set_local_credentials(db, &core.keys, &ctx, id, DEMO_PASSWORD, None).await?;
        if let Some((key, scoped)) = u.role {
            let role = directory::role_id_by_key(db, key).await?;
            let scope = if scoped { Some(dept(u.dept)?) } else { None };
            directory::assign_role(db, &ctx, id, role, scope).await?;
        }
        user_ids.insert(u.email, id);
    }

    let committee = directory::create_group(
        db,
        &ctx,
        "Contracts Review Committee",
        "Reviews contracts and partnership agreements before approval.",
    )
    .await?;
    for email in ["alice@example.org", "carol@example.org"] {
        if let Some(id) = user_ids.get(email) {
            directory::add_group_member(db, &ctx, committee, *id).await?;
        }
    }

    // Legal counsel already views non-Restricted documents org-wide, and
    // department heads approve (both seeded by migration 0005).
    // Department heads may create and edit documents in their own department.
    let head = directory::role_id_by_key(db, role_keys::DEPARTMENT_HEAD).await?;
    let ops = dept("OPS")?;
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(
            db,
            &ctx,
            Subject::Role(head),
            Scope::Department(ops),
            p,
            None,
            "demo: department head",
        )
        .await?;
    }

    let docs = seed_documents(core, &ctx, &dept_ids, &user_ids).await?;
    seed_workflow(core, &dept_ids, &user_ids, &docs).await?;
    seed_appointments(core, &ctx, &dept_ids, &user_ids).await?;
    seed_records_requests(core, &user_ids, &docs).await?;
    seed_comments_and_text(core, &user_ids, &docs).await?;

    // Apply date-driven statuses and create reminders now rather than at the
    // worker's next run (as if it were 10:00 locally, inside the send window).
    let today = crate::schedule::local_today(chrono::Utc::now(), core.config.timezone());
    let morning = today
        .and_hms_opt(10, 0, 0)
        .and_then(|t| t.and_local_timezone(core.config.timezone()).earliest())
        .map_or_else(chrono::Utc::now, |t| t.with_timezone(&chrono::Utc));
    crate::notifications::plan(core, morning).await?;

    Ok(DemoSummary {
        created: true,
        users,
    })
}

/// A minimal valid one-page PDF showing `lines` of text.
pub fn demo_pdf(lines: &[&str]) -> Vec<u8> {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    };
    let mut text = String::from("BT /F1 12 Tf 72 720 Td 16 TL\n");
    for l in lines {
        text.push_str(&format!("({}) Tj T*\n", esc(l)));
    }
    text.push_str("ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for off in offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

async fn spool(
    core: &Core,
    name: &str,
    bytes: &[u8],
) -> CoreResult<crate::documents::files::Upload> {
    let mut w = pdms_storage::spool::SpoolWriter::create(&core.config.storage.spool_path, u64::MAX)
        .await
        .map_err(crate::CoreError::internal)?;
    w.write(bytes).await.map_err(crate::CoreError::internal)?;
    Ok(crate::documents::files::Upload {
        file: w.finish().await.map_err(crate::CoreError::internal)?,
        file_name: name.to_owned(),
    })
}

fn day(s: &str) -> Option<chrono::NaiveDate> {
    s.parse().ok()
}

async fn seed_documents(
    core: &Core,
    ctx: &Ctx,
    depts: &std::collections::HashMap<&str, uuid::Uuid>,
    users: &std::collections::HashMap<&str, uuid::Uuid>,
) -> CoreResult<Vec<uuid::Uuid>> {
    use std::collections::BTreeMap;

    use crate::documents::fields::{self, FieldSpec, FieldType, SelectOption};
    use crate::documents::{
        self, DocumentStatus, MetadataInput, NewDocument, VersionKind, tags, types,
    };

    let db = &core.db;
    let today = crate::schedule::local_today(chrono::Utc::now(), core.config.timezone());
    let rel = |days: i64| Some(today + chrono::Duration::days(days));
    let ty = |key: &'static str| async move { types::type_by_key(db, key).await.map(|t| t.id) };
    let (contract, agreement, policy, other) = (
        ty("contract").await?,
        ty("partnership_agreement").await?,
        ty("policy").await?,
        ty("other").await?,
    );
    let dept = |c: &str| depts.get(c).copied().ok_or(crate::CoreError::NotFound);
    let user = |e: &str| users.get(e).copied().ok_or(crate::CoreError::NotFound);

    fields::create(
        db,
        ctx,
        "contract_amount",
        FieldType::Number,
        Some(contract),
        FieldSpec {
            label: "Contract amount".into(),
            help_text: "Total not-to-exceed amount in dollars.".into(),
            sort_order: 10,
            ..Default::default()
        },
    )
    .await?;
    fields::create(
        db,
        ctx,
        "other_party",
        FieldType::Text,
        None,
        FieldSpec {
            label: "Other party".into(),
            help_text: "Vendor, agency, or other entity that is party to the document.".into(),
            sort_order: 5,
            ..Default::default()
        },
    )
    .await?;
    fields::create(
        db,
        ctx,
        "funding_source",
        FieldType::SingleSelect,
        None,
        FieldSpec {
            label: "Funding source".into(),
            options: vec![
                SelectOption {
                    value: "general".into(),
                    label: "General Fund".into(),
                },
                SelectOption {
                    value: "operations".into(),
                    label: "Operations Fund".into(),
                },
                SelectOption {
                    value: "grant".into(),
                    label: "Grant".into(),
                },
            ],
            sort_order: 20,
            ..Default::default()
        },
    )
    .await?;
    for (name, color) in [
        ("Operations", "#0b4f8a"),
        ("Partnership", "#6f42c1"),
        ("Grant-funded", "#116329"),
        ("Personnel", "#a40e26"),
    ] {
        tags::create_curated(db, ctx, name, color, "").await?;
    }

    let custom = |pairs: &[(&str, &str)]| -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), vec![(*v).to_owned()]))
            .collect()
    };
    struct Spec<'a> {
        meta: MetadataInput,
        owner: &'a str,
        tags: &'a [&'a str],
        file: Option<(&'a str, Vec<&'a str>)>,
    }
    let specs = [
        Spec {
            meta: MetadataInput {
                title: "Partnership Agreement – Facilities Maintenance with Example Partner".into(),
                description: "Shared maintenance of shared facilities. Original agreement 2013; amended 2014. Reviewed every five years, but not reviewed since 2014.".into(),
                document_type_id: agreement,
                department_id: dept("OPS")?,
                execution_date: day("2013-02-12"),
                effective_date: day("2013-03-01"),
                last_review_date: day("2014-06-01"),
                next_review_date: day("2019-06-01"),
                custom: custom(&[("other_party", "Example Partner Inc."), ("funding_source", "operations")]),
                ..Default::default()
            },
            owner: "alice@example.org",
            tags: &["Operations", "Partnership"],
            file: Some(("agreement-facilities-maintenance-2013.pdf", vec!["Partnership Agreement: Facilities Maintenance", "Executed February 12, 2013", "Effective March 1, 2013"])),
        },
        Spec {
            meta: MetadataInput {
                title: "Asphalt Supply Contract FY2026".into(),
                document_type_id: contract,
                department_id: dept("OPS")?,
                execution_date: rel(-390),
                effective_date: rel(-365),
                renewal_date: rel(45),
                expiration_date: rel(120),
                custom: custom(&[("contract_amount", "485000"), ("other_party", "Example Paving Co."), ("funding_source", "operations")]),
                ..Default::default()
            },
            owner: "alice@example.org",
            tags: &["Operations"],
            file: Some(("asphalt-supply-fy2026.pdf", vec!["Asphalt Supply Contract", "Term: October 1, 2025 - September 30, 2026"])),
        },
        Spec {
            meta: MetadataInput {
                title: "Personnel Policy Manual".into(),
                description: "Adopted employee policies. Reviewed annually.".into(),
                document_type_id: policy,
                department_id: dept("HR")?,
                effective_date: day("2024-01-01"),
                last_review_date: rel(-345),
                next_review_date: rel(20),
                ..Default::default()
            },
            owner: "bob@example.org",
            tags: &["Personnel"],
            file: Some(("personnel-policy-manual.pdf", vec!["Personnel Policy Manual", "Revised January 2026"])),
        },
        Spec {
            meta: MetadataInput {
                title: "Workplace Investigation Procedures".into(),
                description: "Restricted: visible only to the owner and people it is shared with.".into(),
                document_type_id: policy,
                department_id: dept("HR")?,
                next_review_date: rel(60),
                ..Default::default()
            },
            owner: "bob@example.org",
            tags: &["Personnel"],
            file: None,
        },
        Spec {
            meta: MetadataInput {
                title: "Workplace Safety Grant Agreement".into(),
                document_type_id: other,
                department_id: dept("OPS")?,
                effective_date: rel(-200),
                expiration_date: rel(800),
                custom: custom(&[("other_party", "Example Foundation"), ("funding_source", "grant")]),
                ..Default::default()
            },
            owner: "alice@example.org",
            tags: &["Operations", "Grant-funded"],
            file: None,
        },
    ];

    let mut ids = Vec::new();
    for s in specs {
        let file = match s.file {
            Some((name, lines)) => Some(spool(core, name, &demo_pdf(&lines)).await?),
            None => None,
        };
        let id = documents::create(
            core,
            ctx,
            NewDocument {
                visibility: Default::default(),
                metadata: s.meta,
                owner_id: user(s.owner)?,
                status: DocumentStatus::Active,
                tags: s.tags.iter().map(|t| (*t).to_owned()).collect(),
                file,
                note: String::new(),
            },
        )
        .await?;
        ids.push(id);
    }

    // The agreement's 2014 amendment as a second version.
    let amendment = spool(
        core,
        "agreement-amendment-1-2014.pdf",
        &demo_pdf(&["Amendment No. 1", "Executed June 1, 2014"]),
    )
    .await?;
    documents::add_version(
        core,
        ctx,
        ids[0],
        amendment,
        VersionKind::Amendment,
        "Amendment No. 1",
        day("2014-06-01"),
    )
    .await?;
    // Restrict the investigation procedures, and share the grant with Legal for review.
    documents::set_visibility(core, ctx, ids[3], pdms_authz::Visibility::Restricted).await?;
    // Everyone in the organization should be able to read the personnel policy.
    documents::set_visibility(core, ctx, ids[2], pdms_authz::Visibility::CountyWide).await?;
    documents::share(
        core,
        ctx,
        ids[4],
        Subject::Department(dept("LEGAL")?),
        Permission::View,
        None,
        "demo: legal review",
    )
    .await?;
    Ok(ids)
}

/// Act as a demo user (as the web layer would for a signed-in request).
async fn as_demo(
    core: &Core,
    users: &std::collections::HashMap<&str, uuid::Uuid>,
    email: &str,
) -> CoreResult<Ctx> {
    let id = users
        .get(email)
        .copied()
        .ok_or(crate::CoreError::NotFound)?;
    let principal = crate::auth::principal::load(&core.db, id).await?;
    let name = USERS
        .iter()
        .find(|u| u.email == email)
        .map_or(email, |u| u.name);
    Ok(Ctx::user(
        principal,
        name,
        pdms_audit::AuthMethod::System,
        &crate::ClientInfo::default(),
    ))
}

/// A contract waiting for legal review, and signatures on two documents.
async fn seed_workflow(
    core: &Core,
    depts: &std::collections::HashMap<&str, uuid::Uuid>,
    users: &std::collections::HashMap<&str, uuid::Uuid>,
    docs: &[uuid::Uuid],
) -> CoreResult<()> {
    use pdms_authz::Assignee;

    use crate::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
    use crate::signatures::{self, NewSignatory, SignedRecord};
    use crate::workflow::{self, StepDecision, StepKind, StepStatus, Submission};

    let user = |e: &str| users.get(e).copied().ok_or(crate::CoreError::NotFound);
    let ops = depts
        .get("OPS")
        .copied()
        .ok_or(crate::CoreError::NotFound)?;
    let today = crate::schedule::local_today(chrono::Utc::now(), core.config.timezone());
    let alice = as_demo(core, users, "alice@example.org").await?;
    let gus = as_demo(core, users, "gus@example.org").await?;

    let contract = types::type_by_key(&core.db, "contract").await?.id;
    let file = spool(
        core,
        "equipment-inspection-services.pdf",
        &demo_pdf(&[
            "Equipment Inspection Services Contract",
            "DRAFT for review",
            "Services under this contract begin January 4, 2027.",
            "This contract expires on December 31, 2028 unless renewed by the parties.",
        ]),
    )
    .await?;
    let id = documents::create(
        core,
        &alice,
        NewDocument {
            visibility: Default::default(),
            metadata: MetadataInput {
                title: "Equipment Inspection Services Contract".into(),
                description:
                    "Biennial inspection of company equipment. Contracts require legal review."
                        .into(),
                document_type_id: contract,
                department_id: ops,
                effective_date: Some(today + chrono::Duration::days(30)),
                expiration_date: Some(today + chrono::Duration::days(760)),
                ..Default::default()
            },
            owner_id: user("alice@example.org")?,
            status: DocumentStatus::Draft,
            tags: vec!["Operations".into()],
            file: Some(file),
            note: String::new(),
        },
    )
    .await?;
    workflow::submit(
        core,
        &alice,
        id,
        Submission {
            reviewers: vec![Assignee::User(user("gus@example.org")?)],
            approvers: vec![Assignee::User(user("jordan@example.org")?)],
            legal_review: false,
            note: "Scope and fee schedule are in Exhibit A. Needed before the next board meeting."
                .into(),
        },
    )
    .await?;
    let panel = workflow::panel(core, &gus, id).await?;
    if let Some(step) = panel.workflows.first().and_then(|w| {
        w.steps
            .iter()
            .find(|s| s.kind == StepKind::Review && s.status == StepStatus::Pending)
    }) {
        workflow::decide_step(
            core,
            &gus,
            step.id,
            StepDecision::Approve,
            "Inspection scope matches the state requirements.",
        )
        .await?;
    }

    // The agreement: the presiding officer signed (signed copy on file); the partner has been asked.
    let chair = signatures::add(
        core,
        &alice,
        docs[0],
        NewSignatory {
            name: "Jordan Presiding".into(),
            title: "Presiding Officer".into(),
            user_id: Some(user("jordan@example.org")?),
            ..Default::default()
        },
    )
    .await?;
    let partner = signatures::add(
        core,
        &alice,
        docs[0],
        NewSignatory {
            name: "Morgan Partner".into(),
            title: "Managing Director".into(),
            organization: "Example Partner Inc.".into(),
            email: "director@partner.example".into(),
            ..Default::default()
        },
    )
    .await?;
    signatures::request(core, &alice, chair).await?;
    let signed = spool(
        core,
        "agreement-facilities-maintenance-signed.pdf",
        &demo_pdf(&[
            "Partnership Agreement: Facilities Maintenance",
            "Signed copy",
        ]),
    )
    .await?;
    signatures::record_signed(
        core,
        &alice,
        chair,
        SignedRecord {
            signed_on: Some(today - chrono::Duration::days(3)),
            note: "Signed at the board meeting".into(),
            file: Some(signed),
        },
    )
    .await?;
    signatures::request(core, &alice, partner).await?;
    Ok(())
}

/// Boards and appointments: a term ending soon with the reappointment
/// decision overdue, a holdover, a vacancy, and an open-ended appointment.
async fn seed_appointments(
    core: &Core,
    ctx: &Ctx,
    depts: &std::collections::HashMap<&str, uuid::Uuid>,
    users: &std::collections::HashMap<&str, uuid::Uuid>,
) -> CoreResult<()> {
    use crate::appointments::{self, NewAppointment, PositionSpec};

    let dept = |c: &str| depts.get(c).copied().ok_or(crate::CoreError::NotFound);
    let user = |e: &str| users.get(e).copied().ok_or(crate::CoreError::NotFound);
    let today = crate::schedule::local_today(chrono::Utc::now(), core.config.timezone());
    let rel = |days: i64| today + chrono::Duration::days(days);
    let frank = user("frank@example.org")?;

    let board = appointments::create_position(
        core,
        ctx,
        PositionSpec {
            name: "Seat 1".into(),
            body_name: "Advisory Committee".into(),
            department_id: Some(dept("BOARD")?),
            appointing_authority: "Board of Directors".into(),
            term_months: Some(24),
            seats: 1,
            manager_user_id: Some(frank),
            reappointment_lead_days: 90,
            description: "Two-year staggered term.".into(),
            is_active: true,
        },
    )
    .await?;
    let mut reqs = Vec::new();
    for (label, description) in [
        (
            "Appointment approved by board resolution",
            "Record the order or minutes reference.",
        ),
        ("Acceptance letter on file", ""),
        ("Conflict-of-interest disclosure filed", ""),
    ] {
        reqs.push(appointments::add_requirement(core, ctx, board, label, description).await?);
    }
    let earlier = appointments::appoint(
        core,
        ctx,
        board,
        NewAppointment {
            person_name: "Tom Baker".into(),
            start_date: rel(-1430),
            appointment_reference: "Resolution 2022-31".into(),
            ..Default::default()
        },
    )
    .await?;
    let maria = appointments::replace(
        core,
        ctx,
        earlier,
        NewAppointment {
            person_name: "Maria Lopez".into(),
            contact_email: "maria.lopez@example.net".into(),
            start_date: rel(-700),
            end_date: Some(rel(30)),
            reappointment_due_date: Some(rel(-60)),
            appointment_reference: "Resolution 2024-77".into(),
            ..Default::default()
        },
    )
    .await?;
    for r in &reqs[..2] {
        appointments::set_requirement(core, ctx, maria, *r, true, "").await?;
    }

    let program = appointments::create_position(
        core,
        ctx,
        PositionSpec {
            name: "Trustee".into(),
            body_name: "Program Advisory Board".into(),
            department_id: Some(dept("BOARD")?),
            appointing_authority: "Board of Directors".into(),
            term_months: Some(24),
            seats: 3,
            manager_user_id: Some(frank),
            reappointment_lead_days: 60,
            description: String::new(),
            is_active: true,
        },
    )
    .await?;
    let letter =
        appointments::add_requirement(core, ctx, program, "Acceptance letter on file", "").await?;
    for (name, start, end) in [("Sam Reader", -760, Some(-30)), ("Lee Pages", -200, None)] {
        let id = appointments::appoint(
            core,
            ctx,
            program,
            NewAppointment {
                person_name: name.into(),
                start_date: rel(start),
                end_date: end.map(rel),
                ..Default::default()
            },
        )
        .await?;
        appointments::set_requirement(core, ctx, id, letter, true, "").await?;
    }

    let coordinator = appointments::create_position(
        core,
        ctx,
        PositionSpec {
            name: "Safety Coordinator".into(),
            body_name: "Safety Office".into(),
            department_id: Some(dept("IT")?),
            appointing_authority: "Board Chair".into(),
            term_months: None,
            seats: 1,
            manager_user_id: Some(user("admin@example.org")?),
            reappointment_lead_days: 0,
            description: "Serves at the pleasure of the presiding officer.".into(),
            is_active: true,
        },
    )
    .await?;
    let review = appointments::add_requirement(
        core,
        ctx,
        coordinator,
        "Legal counsel review",
        "Counsel confirms the appointment meets state requirements.",
    )
    .await?;
    let coord = appointments::appoint(
        core,
        ctx,
        coordinator,
        NewAppointment {
            person_name: "Avery Admin".into(),
            user_id: Some(user("admin@example.org")?),
            start_date: rel(-400),
            ..Default::default()
        },
    )
    .await?;
    appointments::set_requirement(core, ctx, coord, review, true, "Reviewed by Carol Counsel")
        .await?;
    Ok(())
}

/// The records officer answers a records request for the agreement.
async fn seed_records_requests(
    core: &Core,
    users: &std::collections::HashMap<&str, uuid::Uuid>,
    docs: &[uuid::Uuid],
) -> CoreResult<()> {
    use crate::exports::{self, PackageRequest, Versions};

    let dana = as_demo(core, users, "dana@example.org").await?;
    exports::build(
        core,
        &dana,
        PackageRequest {
            request_reference: "REQ-2026-014".into(),
            document_ids: vec![docs[0]],
            fields: exports::FIELDS
                .iter()
                .map(|(k, _)| (*k).to_owned())
                .collect(),
            versions: Versions::All,
            include_audit: true,
            note: "Request from the Example Gazette for the facilities maintenance agreement and its history."
                .into(),
        },
    )
    .await?;
    Ok(())
}

/// A comment thread on the agreement, and the uploaded files read (so their text is
/// searchable and the dates found in them wait for confirmation).
async fn seed_comments_and_text(
    core: &Core,
    users: &std::collections::HashMap<&str, uuid::Uuid>,
    docs: &[uuid::Uuid],
) -> CoreResult<()> {
    let alice = as_demo(core, users, "alice@example.org").await?;
    let carol = as_demo(core, users, "carol@example.org").await?;
    let first = crate::comments::post(
        core,
        &alice,
        docs[0],
        None,
        "This agreement has not been reviewed since the 2014 amendment. @carol can you confirm \
         whether the partner still maintains the shared facilities under section 3?",
    )
    .await?;
    crate::comments::post(
        core,
        &carol,
        docs[0],
        Some(first),
        "Section 3 still applies. Start the five-year review and route it to me for legal review.",
    )
    .await?;
    let versions = sqlx::query!(
        "SELECT document_id, version_no FROM document_versions WHERE file_changed ORDER BY document_id, version_no"
    )
    .fetch_all(&core.db)
    .await?;
    for v in versions {
        crate::ocr::extract(core, v.document_id, v.version_no, false).await?;
    }
    Ok(())
}
