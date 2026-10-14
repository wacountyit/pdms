// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Postgres persistence for the audit log.

use futures_util::TryStreamExt;
use ipnet::IpNet;
use sqlx::{Acquire, PgConnection, PgPool, Postgres};
use uuid::Uuid;

use crate::chain::{ChainHead, Hash, SealedEvent, Verifier, VerifyReport, seal};
use crate::event::{
    Actor, AuditEvent, AuthMethod, Category, NotAuditable, Outcome, changes_from_json,
    context_from_json,
};

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("malformed hash in database: expected 32 bytes, found {0}")]
    BadHash(usize),
}

fn to_hash(bytes: &[u8]) -> Result<Hash, AuditError> {
    bytes
        .try_into()
        .map_err(|_| AuditError::BadHash(bytes.len()))
}

/// Append events to the chain inside the caller's transaction.
///
/// Locks the chain head until the transaction ends, so call this as the last
/// step of the business transaction. Audit entries commit or roll back
/// together with the change they describe.
pub async fn append(
    conn: &mut PgConnection,
    events: Vec<AuditEvent>,
) -> Result<Vec<SealedEvent>, AuditError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let row =
        sqlx::query!(r#"SELECT seq AS "seq!", entry_hash AS "entry_hash!" FROM audit_lock_head()"#)
            .fetch_one(&mut *conn)
            .await?;
    let mut head = ChainHead {
        seq: row.seq,
        entry_hash: to_hash(&row.entry_hash)?,
    };

    let mut sealed = Vec::with_capacity(events.len());
    for event in events {
        let s = seal(event, head);
        insert(conn, &s).await?;
        head = ChainHead {
            seq: s.seq,
            entry_hash: s.entry_hash,
        };
        sealed.push(s);
    }
    Ok(sealed)
}

/// Record events in their own transaction (sensitive reads, failed logins,
/// denied requests: anything not tied to a data change).
pub async fn record(pool: &PgPool, events: Vec<AuditEvent>) -> Result<(), AuditError> {
    let mut tx = pool.begin().await?;
    append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(())
}

async fn insert(conn: &mut PgConnection, s: &SealedEvent) -> Result<(), AuditError> {
    let e = &s.event;
    let a = &e.actor;
    sqlx::query!(
        r#"INSERT INTO audit_log (
            seq, id, occurred_at, category, action, outcome,
            actor_user_id, actor_display, actor_auth_method, actor_ip, actor_user_agent,
            api_token_id, request_id, object_type, object_id, object_label,
            document_id, department_id, changes, context,
            hash_version, prev_hash, entry_hash
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23
        )"#,
        s.seq,
        e.id,
        e.occurred_at,
        e.category as Category,
        e.action,
        e.outcome as Outcome,
        a.user_id,
        a.display,
        a.auth_method as AuthMethod,
        a.ip.map(IpNet::from),
        a.user_agent,
        a.api_token_id,
        a.request_id,
        e.object_type,
        e.object_id,
        e.object_label,
        e.document_id,
        e.department_id,
        e.changes_json(),
        e.context_json(),
        s.hash_version,
        &s.prev_hash[..],
        &s.entry_hash[..],
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// One `audit_log` row as stored.
#[derive(Debug, sqlx::FromRow)]
pub struct AuditRow {
    pub seq: i64,
    pub id: Uuid,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub category: Category,
    pub action: String,
    pub outcome: Outcome,
    pub actor_user_id: Option<Uuid>,
    pub actor_display: Option<String>,
    pub actor_auth_method: AuthMethod,
    pub actor_ip: Option<IpNet>,
    pub actor_user_agent: Option<String>,
    pub api_token_id: Option<Uuid>,
    pub request_id: Option<String>,
    pub object_type: String,
    pub object_id: Option<String>,
    pub object_label: Option<String>,
    pub document_id: Option<Uuid>,
    pub department_id: Option<Uuid>,
    pub changes: Option<serde_json::Value>,
    pub context: serde_json::Value,
    pub hash_version: i16,
    pub prev_hash: Vec<u8>,
    pub entry_hash: Vec<u8>,
}

macro_rules! audit_columns {
    () => {
        "seq, id, occurred_at, category, action, outcome, \
        actor_user_id, actor_display, actor_auth_method, actor_ip, actor_user_agent, \
        api_token_id, request_id, object_type, object_id, object_label, document_id, \
        department_id, changes, context, hash_version, prev_hash, entry_hash"
    };
}

/// Every column, for `SELECT {AUDIT_COLUMNS} FROM audit_log ...` into [`AuditRow`].
pub const AUDIT_COLUMNS: &str = audit_columns!();

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Value(#[from] NotAuditable),
    #[error(transparent)]
    Hash(#[from] AuditError),
}

impl AuditRow {
    pub fn into_sealed(self) -> Result<SealedEvent, DecodeError> {
        let changes = self.changes.as_ref().map(changes_from_json).transpose()?;
        let context = context_from_json(&self.context)?;
        Ok(SealedEvent {
            seq: self.seq,
            hash_version: self.hash_version,
            prev_hash: to_hash(&self.prev_hash)?,
            entry_hash: to_hash(&self.entry_hash)?,
            event: AuditEvent {
                id: self.id,
                occurred_at: self.occurred_at,
                category: self.category,
                action: self.action,
                outcome: self.outcome,
                actor: Actor {
                    user_id: self.actor_user_id,
                    display: self.actor_display,
                    auth_method: self.actor_auth_method,
                    ip: self.actor_ip.map(|n| n.addr()),
                    user_agent: self.actor_user_agent,
                    api_token_id: self.api_token_id,
                    request_id: self.request_id,
                },
                object_type: self.object_type,
                object_id: self.object_id,
                object_label: self.object_label,
                document_id: self.document_id,
                department_id: self.department_id,
                changes,
                context,
            },
        })
    }
}

/// Verify the whole chain from a consistent snapshot.
pub async fn verify(pool: &PgPool) -> Result<VerifyReport, AuditError> {
    let mut conn = pool.acquire().await?;
    let mut tx = conn.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let head = sqlx::query!("SELECT seq, entry_hash FROM audit_chain_head")
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| -> Result<ChainHead, AuditError> {
            Ok(ChainHead {
                seq: r.seq,
                entry_hash: to_hash(&r.entry_hash)?,
            })
        })
        .transpose()?;

    let mut verifier = Verifier::new();
    {
        const SQL: &str = concat!("SELECT ", audit_columns!(), " FROM audit_log ORDER BY seq");
        let mut rows = sqlx::query_as::<Postgres, AuditRow>(SQL).fetch(&mut *tx);
        while let Some(row) = rows.try_next().await? {
            let (seq, stored) = (row.seq, to_hash(&row.entry_hash).ok());
            match row.into_sealed() {
                Ok(sealed) => verifier.push(&sealed),
                Err(e) => verifier.push_undecodable(seq, stored, e.to_string()),
            }
        }
    }
    tx.commit().await?;
    Ok(verifier.finish(head))
}
