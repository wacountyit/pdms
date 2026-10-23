// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Sending one email delivery (reminder or action notice). Called by the `notifications.email` job.
//!
//! Exactly-once except for one window: if the worker dies after the SMTP
//! server accepted a message but before `sent` is committed, the retry sends
//! it again with the same `Message-ID` (see DECISIONS D-007). A delivery that
//! is already `sent` or `skipped` is never sent again.

use chrono::{NaiveDate, Utc};
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Permission;
use uuid::Uuid;

use super::NotificationKind;
use super::mailer::OutgoingEmail;
#[cfg(test)]
use super::planner::DbTrigger;
use super::planner::DeliveryStatus;
use super::rules::DueEvent;
use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::model::UserStatus;

/// Everything needed to write the message.
#[derive(Clone, Debug)]
pub struct Content {
    pub org_name: String,
    pub document_url: String,
    pub recipient_name: String,
    pub external_id: String,
    pub title: String,
    pub department: String,
    pub event: DueEvent,
    pub due_date: NaiveDate,
    /// The organization's date when the message is written.
    pub today: NaiveDate,
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Headline and sentence for a reminder about `event` due on `due`, as of
/// `today`. Day counts are computed from the dates, not the trigger offset, so
/// a reminder sent late still states the truth.
pub fn describe(event: DueEvent, due: NaiveDate, today: NaiveDate) -> (String, String) {
    let when = due.format("%B %-d, %Y");
    let days = |n: i64| {
        if n == 1 {
            "1 day".to_owned()
        } else {
            format!("{n} days")
        }
    };
    let ahead = (due - today).num_days();
    match (event, ahead) {
        (DueEvent::TermEnd, d) if d > 0 => (
            format!("Term ends in {}", days(d)),
            format!("reaches the end of the term on {when}"),
        ),
        (DueEvent::TermEnd, 0) => (
            "Term ends today".to_owned(),
            format!("reaches the end of the term today, {when}"),
        ),
        (DueEvent::TermEnd, _) => (
            format!("Term ended {when}"),
            format!("reached the end of the term on {when}; no successor has been recorded"),
        ),
        (DueEvent::Reappointment, d) if d > 0 => (
            format!("Reappointment decision due in {}", days(d)),
            format!("needs a reappointment or replacement decision by {when}"),
        ),
        (DueEvent::Reappointment, 0) => (
            "Reappointment decision due today".to_owned(),
            format!("needs a reappointment or replacement decision today, {when}"),
        ),
        (DueEvent::Reappointment, d) => (
            format!("Reappointment decision overdue by {}", days(-d)),
            format!(
                "needed a reappointment or replacement decision by {when}; a replacement must be selected"
            ),
        ),
        (DueEvent::Expiration, d) if d > 0 => (
            format!("Expires in {}", days(d)),
            format!("expires on {when}"),
        ),
        (DueEvent::Expiration, 0) => ("Expires today".to_owned(), format!("expires today, {when}")),
        (DueEvent::Expiration, _) => (format!("Expired {when}"), format!("expired on {when}")),
        (e, d) if d > 0 => (
            format!("{} due in {}", e.label(), days(d)),
            format!("is due for {} on {when}", e.as_str()),
        ),
        (e, 0) => (
            format!("{} due today", e.label()),
            format!("is due for {} today, {when}", e.as_str()),
        ),
        (e, d) if -d <= 60 => (
            format!("{} overdue by {}", e.label(), days(-d)),
            format!(
                "was due for {} on {when} and is {} overdue",
                e.as_str(),
                days(-d)
            ),
        ),
        (e, _) => (
            format!("{} overdue since {when}", e.label()),
            format!(
                "was due for {} on {when} and is still outstanding",
                e.as_str()
            ),
        ),
    }
}

/// The parts every message is built from.
struct Parts<'a> {
    org_name: &'a str,
    recipient_name: &'a str,
    subject: String,
    /// The subject of the first sentence, shown in bold in HTML (may be empty).
    lead: String,
    lead_markup: Option<String>,
    /// The rest of the first paragraph.
    sentence: String,
    action: String,
    url: &'a str,
    link_label: String,
    why: &'a str,
}

fn render(p: Parts<'_>, message_id: String, to_address: String) -> OutgoingEmail {
    let first = if p.lead.is_empty() {
        p.sentence.clone()
    } else {
        format!("{} {}", p.lead, p.sentence)
    };
    let text = format!(
        "Hello {name},\n\n{first}\n\n{action}\n{url}\n\n{why}\nThis is an automated message from {org}.\n",
        name = p.recipient_name,
        action = p.action,
        url = p.url,
        why = p.why,
        org = p.org_name,
    );
    let e = html_escape;
    let first_html = if p.lead.is_empty() {
        e(&p.sentence)
    } else {
        format!("{} {}", p.lead_html(), e(&p.sentence))
    };
    let html = format!(
        "<p>Hello {name},</p><p>{first_html}</p>\
         <p>{action}</p><p><a href=\"{url}\">{label}</a></p>\
         <p style=\"color:#57606a;font-size:small\">{why} This is an automated message from {org}.</p>",
        name = e(p.recipient_name),
        action = e(&p.action),
        url = e(p.url),
        label = e(&p.link_label),
        why = e(p.why),
        org = e(p.org_name),
    );
    OutgoingEmail {
        to_name: p.recipient_name.to_owned(),
        to_address,
        subject: p.subject,
        text,
        html,
        message_id,
    }
}

impl Parts<'_> {
    /// The lead is pre-escaped HTML so callers can mark up part of it.
    fn lead_html(&self) -> String {
        self.lead_markup
            .clone()
            .unwrap_or_else(|| html_escape(&self.lead))
    }
}

/// Subject and body for a document reminder.
pub fn compose(c: &Content, message_id: String, to_address: String) -> OutgoingEmail {
    let (headline, sentence) = describe(c.event, c.due_date, c.today);
    let action = match c.event {
        DueEvent::Review => "Open the document to start or record the review.",
        DueEvent::Renewal => "Open the document to start the renewal and record the new dates.",
        _ => "Open the document to renew it, or archive it if it is no longer needed.",
    };
    let e = html_escape;
    render(
        Parts {
            org_name: &c.org_name,
            recipient_name: &c.recipient_name,
            subject: format!("[{}] {headline}: {} {}", c.org_name, c.external_id, c.title),
            lead: format!("{} \"{}\" ({})", c.external_id, c.title, c.department),
            lead_markup: Some(format!(
                "<strong>{}</strong> &ldquo;{}&rdquo; ({})",
                e(&c.external_id),
                e(&c.title),
                e(&c.department)
            )),
            sentence: format!("{sentence}."),
            action: action.to_owned(),
            url: &c.document_url,
            link_label: format!("Open {}", c.external_id),
            why: "You receive this because of the notification settings for this document.",
        },
        message_id,
        to_address,
    )
}

/// Everything needed to write an appointment reminder.
#[derive(Clone, Debug)]
pub struct AppointmentContent {
    pub org_name: String,
    pub url: String,
    pub recipient_name: String,
    pub person_name: String,
    pub position_name: String,
    pub body_name: String,
    pub event: DueEvent,
    pub due_date: NaiveDate,
    pub today: NaiveDate,
}

/// Subject and body for an appointment reminder.
pub fn compose_appointment(
    c: &AppointmentContent,
    message_id: String,
    to_address: String,
) -> OutgoingEmail {
    let (headline, sentence) = describe(c.event, c.due_date, c.today);
    let seat = format!("{}, {}", c.position_name, c.body_name);
    let e = html_escape;
    render(
        Parts {
            org_name: &c.org_name,
            recipient_name: &c.recipient_name,
            subject: format!("[{}] {headline}: {} ({seat})", c.org_name, c.person_name),
            lead: format!("{} ({seat})", c.person_name),
            lead_markup: Some(format!("<strong>{}</strong> ({})", e(&c.person_name), e(&seat))),
            sentence: format!("{sentence}."),
            action: "Open the position to record a reappointment, a replacement, or the end of the term."
                .to_owned(),
            url: &c.url,
            link_label: format!("Open {}", c.position_name),
            why: "You receive this because of the notification settings for appointments.",
        },
        message_id,
        to_address,
    )
}

/// Subject and body for an "action needed" notice.
pub fn compose_action(
    org_name: &str,
    recipient_name: &str,
    subject: &str,
    body: &str,
    url: &str,
    message_id: String,
    to_address: String,
) -> OutgoingEmail {
    render(
        Parts {
            org_name,
            recipient_name,
            subject: format!("[{org_name}] {subject}"),
            lead: String::new(),
            lead_markup: None,
            sentence: body.to_owned(),
            action: "Open it here:".to_owned(),
            url,
            link_label: "Open in PDMS".to_owned(),
            why: "You receive this because your action or attention is needed.",
        },
        message_id,
        to_address,
    )
}

/// Send one email delivery. `Err` means "retry later" (the job is retried with backoff).
pub async fn send_email(core: &Core, delivery_id: Uuid, worker: &str) -> Result<(), String> {
    send_inner(core, delivery_id, worker)
        .await
        .map_err(|e| e.to_string())
}

/// The message for a delivery, worked out at send time.
struct Prepared {
    email: OutgoingEmail,
    /// (object type, id, label, document, department) for the audit entry.
    object: (&'static str, Uuid, String, Option<Uuid>, Option<Uuid>),
}

/// Why a delivery should not be sent after all, if it should not.
async fn recheck(
    core: &Core,
    user_id: Uuid,
    display_name: &str,
    document_id: Option<Uuid>,
    appointment_id: Option<Uuid>,
) -> CoreResult<Option<&'static str>> {
    if !core.mailer().enabled() {
        return Ok(Some("email is not configured"));
    }
    let principal = crate::auth::principal::load(&core.db, user_id).await?;
    if let Some(doc) = document_id {
        let ctx = Ctx::user(
            principal.clone(),
            display_name,
            pdms_audit::AuthMethod::System,
            &crate::ClientInfo::default(),
        );
        if !crate::documents::access::can(core, &ctx, doc, Permission::View).await? {
            return Ok(Some(
                "recipient can no longer view the document, or it was deleted",
            ));
        }
    }
    if let Some(a) = appointment_id
        && !crate::appointments::principal_can_view_appointment(core, &principal, a).await?
    {
        return Ok(Some("recipient can no longer view the position"));
    }
    Ok(None)
}

async fn send_inner(core: &Core, delivery_id: Uuid, worker: &str) -> CoreResult<()> {
    let mut tx = core.db.begin().await?;
    let d = sqlx::query!(
        r#"SELECT dl.status AS "status: DeliveryStatus", dl.attempts, dl.user_id,
                  u.email, u.display_name, u.status AS "user_status: UserStatus",
                  n.kind AS "kind: NotificationKind", n.event AS "event: DueEvent", n.due_date,
                  n.document_id, n.appointment_id, n.subject, n.body, n.link
             FROM notification_deliveries dl
             JOIN notifications n ON n.id = dl.notification_id
             JOIN users u ON u.id = dl.user_id
            WHERE dl.id = $1 AND dl.channel = 'email'
            FOR UPDATE OF dl"#,
        delivery_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    if matches!(d.status, DeliveryStatus::Sent | DeliveryStatus::Skipped) {
        return Ok(()); // already done: never send twice
    }

    // Re-check at send time: the person may have left or lost access since planning.
    let skip_reason = if d.user_status != UserStatus::Active {
        Some("recipient is no longer active")
    } else {
        recheck(
            core,
            d.user_id,
            &d.display_name,
            d.document_id,
            d.appointment_id,
        )
        .await?
    };
    if let Some(reason) = skip_reason {
        sqlx::query!(
            "UPDATE notification_deliveries SET status = 'skipped', skip_reason = $2, updated_at = now() WHERE id = $1",
            delivery_id,
            reason
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }

    let attempt_no = d.attempts + 1;
    let host = core
        .config()
        .server
        .public_url
        .host_str()
        .unwrap_or("pdms.local")
        .to_owned();
    let message_id = format!("<delivery-{delivery_id}@{host}>");
    sqlx::query!(
        "UPDATE notification_deliveries SET status = 'sending', attempts = $2, message_id = $3, updated_at = now()
          WHERE id = $1",
        delivery_id,
        attempt_no,
        message_id
    )
    .execute(&mut *tx)
    .await?;
    let attempt_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO notification_delivery_attempts (id, delivery_id, attempt_no, worker) VALUES ($1, $2, $3, $4)",
        attempt_id,
        delivery_id,
        attempt_no,
        worker
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let today = crate::schedule::local_today(Utc::now(), core.config().timezone());
    let url = |path: &str| {
        core.config()
            .server
            .public_url
            .join(path.trim_start_matches('/'))
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    let org = crate::branding::org_name(core).await;
    let prepared = match (d.kind, d.event, d.due_date) {
        (NotificationKind::Reminder, Some(event), Some(due)) if d.appointment_id.is_some() => {
            let a = sqlx::query!(
                "SELECT a.id, a.person_name, p.id AS position_id, p.name, p.body_name, p.department_id
                   FROM appointments a JOIN positions p ON p.id = a.position_id WHERE a.id = $1",
                d.appointment_id
            )
            .fetch_one(&core.db)
            .await?;
            Prepared {
                email: compose_appointment(
                    &AppointmentContent {
                        org_name: org,
                        url: url(&format!("positions/{}", a.position_id)),
                        recipient_name: d.display_name.clone(),
                        person_name: a.person_name.clone(),
                        position_name: a.name.clone(),
                        body_name: a.body_name.clone(),
                        event,
                        due_date: due,
                        today,
                    },
                    message_id.clone(),
                    d.email.clone(),
                ),
                object: (
                    "appointment",
                    a.id,
                    format!("{} — {}, {}", a.person_name, a.name, a.body_name),
                    None,
                    a.department_id,
                ),
            }
        }
        (NotificationKind::Reminder, Some(event), Some(due)) => {
            let doc = sqlx::query!(
                "SELECT doc.id, doc.external_id, doc.title, doc.department_id, dep.name AS department
                   FROM documents doc JOIN departments dep ON dep.id = doc.department_id WHERE doc.id = $1",
                d.document_id
            )
            .fetch_one(&core.db)
            .await?;
            Prepared {
                email: compose(
                    &Content {
                        org_name: org,
                        document_url: url(&format!("documents/{}", doc.id)),
                        recipient_name: d.display_name.clone(),
                        external_id: doc.external_id.clone(),
                        title: doc.title.clone(),
                        department: doc.department.clone(),
                        event,
                        due_date: due,
                        today,
                    },
                    message_id.clone(),
                    d.email.clone(),
                ),
                object: (
                    "document",
                    doc.id,
                    format!("{} {}", doc.external_id, doc.title),
                    Some(doc.id),
                    Some(doc.department_id),
                ),
            }
        }
        _ => {
            let (object_type, object_id, department) = match (d.document_id, d.appointment_id) {
                (Some(doc), _) => (
                    "document",
                    doc,
                    sqlx::query_scalar!("SELECT department_id FROM documents WHERE id = $1", doc)
                        .fetch_optional(&core.db)
                        .await?,
                ),
                (None, Some(a)) => ("appointment", a, None),
                (None, None) => ("notification", delivery_id, None),
            };
            let subject = d.subject.clone().unwrap_or_default();
            Prepared {
                email: compose_action(
                    &org,
                    &d.display_name,
                    &subject,
                    d.body.as_deref().unwrap_or_default(),
                    &url(d.link.as_deref().unwrap_or("/")),
                    message_id.clone(),
                    d.email.clone(),
                ),
                object: (object_type, object_id, subject, d.document_id, department),
            }
        }
    };
    let result = core.mailer().send(&prepared.email).await;

    let mut tx = core.db.begin().await?;
    match &result {
        Ok(()) => {
            sqlx::query!(
                "UPDATE notification_deliveries SET status = 'sent', sent_at = now(), last_error = NULL, updated_at = now()
                  WHERE id = $1",
                delivery_id
            )
            .execute(&mut *tx)
            .await?;
            let (object_type, object_id, label, document, department) = prepared.object;
            pdms_audit::append(
                &mut tx,
                vec![
                    AuditEvent::new(
                        Category::Notification,
                        "notification.sent",
                        Ctx::system("notifier").actor,
                        object_type,
                    )
                    .object(object_id, Some(&label))
                    .document_opt(document)
                    .department(department)
                    .context("delivery_id", delivery_id)
                    .context("channel", "email")
                    .context("recipient_user_id", d.user_id)
                    .context("message_id", message_id)
                    .context("attempt", attempt_no),
                ],
            )
            .await?;
        }
        Err(e) => {
            sqlx::query!(
                "UPDATE notification_deliveries SET status = 'failed', last_error = $2, updated_at = now() WHERE id = $1",
                delivery_id,
                e.to_string()
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query!(
        "UPDATE notification_delivery_attempts SET finished_at = $2, outcome = $3, error = $4 WHERE id = $1",
        attempt_id,
        Utc::now(),
        if result.is_ok() { "sent" } else { "failed" },
        result.as_ref().err().map(ToString::to_string),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    result.map_err(|e| CoreError::internal(format!("sending email: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(event: DueEvent, trigger: DbTrigger, offset: i32) -> Content {
        let due: NaiveDate = "2026-12-01".parse().unwrap();
        let today = match trigger {
            DbTrigger::Advance => due - chrono::Duration::days(i64::from(offset)),
            DbTrigger::Due => due,
            DbTrigger::Overdue => due + chrono::Duration::days(i64::from(offset)),
        };
        Content {
            today,
            org_name: "Test Org PDMS".into(),
            document_url: "https://pdms.example.org/documents/x".into(),
            recipient_name: "Alice".into(),
            external_id: "PAG-2026-000001".into(),
            title: "Fleet <Maintenance> & Repair".into(),
            department: "Operations".into(),
            event,
            due_date: "2026-12-01".parse().unwrap(),
        }
    }

    #[test]
    fn subjects_read_naturally() {
        let s = |c| compose(&c, "<id@x>".into(), "a@x.org".into()).subject;
        assert_eq!(
            s(content(DueEvent::Review, DbTrigger::Advance, 30)),
            "[Test Org PDMS] Review due in 30 days: PAG-2026-000001 Fleet <Maintenance> & Repair"
        );
        assert!(s(content(DueEvent::Renewal, DbTrigger::Due, 0)).contains("Renewal due today"));
        assert!(
            s(content(DueEvent::Review, DbTrigger::Overdue, 14))
                .contains("Review overdue by 14 days")
        );
        assert!(
            s(content(DueEvent::Expiration, DbTrigger::Advance, 1)).contains("Expires in 1 day")
        );
        assert!(
            s(content(DueEvent::Review, DbTrigger::Overdue, 2688))
                .contains("Review overdue since December 1, 2026")
        );
    }

    #[test]
    fn day_counts_come_from_the_dates_not_the_trigger() {
        // A 90-day reminder caught up 30 days late says 60 days.
        let due: NaiveDate = "2026-12-01".parse().unwrap();
        let today = due - chrono::Duration::days(60);
        assert_eq!(
            describe(DueEvent::Review, due, today).0,
            "Review due in 60 days"
        );
    }

    #[test]
    fn html_is_escaped_and_text_has_link() {
        let m = compose(
            &content(DueEvent::Review, DbTrigger::Due, 0),
            "<id@x>".into(),
            "a@x.org".into(),
        );
        assert!(m.html.contains("Fleet &lt;Maintenance&gt; &amp; Repair"));
        assert!(!m.html.contains("<Maintenance>"));
        assert!(m.text.contains("https://pdms.example.org/documents/x"));
        assert!(m.text.contains("December 1, 2026"));
    }

    #[test]
    fn appointment_reminders_and_action_notices() {
        let due: NaiveDate = "2026-12-31".parse().unwrap();
        let d = |n: i64| due - chrono::Duration::days(n);
        assert_eq!(
            describe(DueEvent::TermEnd, due, d(30)).0,
            "Term ends in 30 days"
        );
        assert_eq!(describe(DueEvent::TermEnd, due, due).0, "Term ends today");
        assert_eq!(
            describe(DueEvent::TermEnd, due, d(-3)).0,
            "Term ended December 31, 2026"
        );
        assert_eq!(
            describe(DueEvent::Reappointment, due, d(-7)).0,
            "Reappointment decision overdue by 7 days"
        );
        let m = compose_appointment(
            &AppointmentContent {
                org_name: "Test Org PDMS".into(),
                url: "https://pdms.example.org/positions/x".into(),
                recipient_name: "Clerk".into(),
                person_name: "Jane <Doe>".into(),
                position_name: "Seat 3".into(),
                body_name: "Advisory Committee".into(),
                event: DueEvent::Reappointment,
                due_date: due,
                today: d(60),
            },
            "<id@x>".into(),
            "c@x.org".into(),
        );
        assert_eq!(
            m.subject,
            "[Test Org PDMS] Reappointment decision due in 60 days: Jane <Doe> (Seat 3, Advisory Committee)"
        );
        assert!(m.html.contains("Jane &lt;Doe&gt;") && !m.html.contains("<Doe>"));
        assert!(m.text.contains("https://pdms.example.org/positions/x"));

        let a = compose_action(
            "Test Org PDMS",
            "Pat",
            "Approval requested: POL-1 <Leave>",
            "Sam submitted it.",
            "https://pdms.example.org/documents/x#workflow",
            "<id@x>".into(),
            "p@x.org".into(),
        );
        assert_eq!(
            a.subject,
            "[Test Org PDMS] Approval requested: POL-1 <Leave>"
        );
        assert!(a.text.contains("Sam submitted it.") && a.text.contains("#workflow"));
        assert!(!a.html.contains("<Leave>"));
    }
}
