// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Forward audit entries to an external log system: a JSON-lines file
//! (for a log shipper) or syslog over UDP (RFC 5424). The database stays the
//! system of record; forwarding resumes from a cursor, so nothing is skipped
//! after an outage (an entry may be sent twice if the process dies between
//! sending and saving the cursor).

use std::io::Write;

use serde_json::{Value, json};

use super::{advance, cursor};
use crate::Core;
use crate::config::AuditForwardTarget;
use crate::error::{CoreError, CoreResult};

pub const FORWARD_JOB: &str = "audit.forward";

/// One audit entry as forwarded (every column, hashes in hex).
async fn batch(core: &Core, after: i64) -> CoreResult<Vec<(i64, String, bool, Value)>> {
    let rows = sqlx::query!(
        r#"SELECT seq, id, occurred_at, category::text AS "category!", action, outcome::text AS "outcome!",
                  actor_user_id, actor_display, actor_auth_method::text AS "auth_method!", host(actor_ip) AS ip,
                  actor_user_agent, api_token_id, request_id, object_type, object_id, object_label,
                  document_id, department_id, changes, context, prev_hash, entry_hash
             FROM audit_log WHERE seq > $1 ORDER BY seq LIMIT 1000"#,
        after
    )
    .fetch_all(&core.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let denied = r.outcome != "success";
            (
                r.seq,
                r.action.clone(),
                denied,
                json!({
                    "seq": r.seq,
                    "id": r.id,
                    "occurred_at": r.occurred_at,
                    "category": r.category,
                    "action": r.action,
                    "outcome": r.outcome,
                    "actor": {
                        "user_id": r.actor_user_id,
                        "display": r.actor_display,
                        "auth_method": r.auth_method,
                        "ip": r.ip,
                        "user_agent": r.actor_user_agent,
                        "api_token_id": r.api_token_id,
                        "request_id": r.request_id,
                    },
                    "object": { "type": r.object_type, "id": r.object_id, "label": r.object_label },
                    "document_id": r.document_id,
                    "department_id": r.department_id,
                    "changes": r.changes,
                    "context": r.context,
                    "prev_hash": hex::encode(r.prev_hash),
                    "entry_hash": hex::encode(r.entry_hash),
                    "organization": core.config.org.name,
                }),
            )
        })
        .collect())
}

/// RFC 5424 line: facility local0, informational (or warning for denials).
pub fn syslog_line(app: &str, host: &str, action: &str, denied: bool, body: &str) -> String {
    let pri = 16 * 8 + if denied { 4 } else { 6 };
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    format!("<{pri}>1 {ts} {host} {app} - {action} - {body}")
}

/// Forward everything new. Returns how many entries were sent.
pub async fn forward(core: &Core) -> CoreResult<usize> {
    let cfg = &core.config.audit_forward;
    if cfg.target == AuditForwardTarget::None {
        return Ok(0);
    }
    let mut sent = 0;
    loop {
        let mut tx = core.db.begin().await?;
        let last = cursor(&mut tx, "audit_forward").await?;
        let rows = batch(core, last).await?;
        let Some(max) = rows.last().map(|r| r.0) else {
            tx.commit().await?; // keeps a newly created cursor
            return Ok(sent);
        };
        match cfg.target {
            AuditForwardTarget::File => {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&cfg.file_path)
                    .map_err(|e| CoreError::internal(format!("audit forward file: {e}")))?;
                let mut buf = Vec::new();
                for (_, _, _, v) in &rows {
                    serde_json::to_writer(&mut buf, v).map_err(CoreError::internal)?;
                    buf.push(b'\n');
                }
                f.write_all(&buf)
                    .and_then(|()| f.sync_data())
                    .map_err(|e| CoreError::internal(format!("audit forward file: {e}")))?;
            }
            AuditForwardTarget::Syslog => {
                let sock = tokio::net::UdpSocket::bind("0.0.0.0:0")
                    .await
                    .map_err(|e| CoreError::internal(format!("syslog socket: {e}")))?;
                let host = core
                    .config
                    .server
                    .public_url
                    .host_str()
                    .unwrap_or("pdms")
                    .to_owned();
                for (_, action, denied, v) in &rows {
                    let line =
                        syslog_line(&cfg.syslog_app_name, &host, action, *denied, &v.to_string());
                    sock.send_to(line.as_bytes(), &cfg.syslog_addr)
                        .await
                        .map_err(|e| CoreError::internal(format!("syslog send: {e}")))?;
                }
            }
            AuditForwardTarget::None => {}
        }
        sent += rows.len();
        advance(&mut tx, "audit_forward", max).await?;
        tx.commit().await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syslog_priority() {
        assert!(syslog_line("pdms", "h", "document.created", false, "{}").starts_with("<134>1 "));
        assert!(syslog_line("pdms", "h", "access.denied", true, "{}").starts_with("<132>1 "));
        assert!(syslog_line("pdms", "h", "a.b", false, "{}").ends_with(" h pdms - a.b - {}"));
    }
}
