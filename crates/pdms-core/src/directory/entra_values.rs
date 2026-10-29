// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Entra ID "Department" values → PDMS departments (D-086).

use std::collections::{BTreeMap, BTreeSet};

use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::Capability;
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::entra_sync;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};

/// One mapping: the value as an admin typed it, and its department.
#[derive(Clone, Debug, Serialize)]
pub struct EntraValue {
    pub value: String,
    pub normalized: String,
    pub department_id: Uuid,
    pub department_name: String,
}

/// A Department value active Entra people have that maps to nothing.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UnmappedValue {
    /// As found on an account ("" when the field is blank).
    pub value: String,
    pub normalized: String,
    pub users: i64,
}

/// Lowercase, `&` → "and", collapsed whitespace: "Road & Bridge",
/// "road and bridge " and "ROAD  AND BRIDGE" are one value.
pub fn normalize(value: &str) -> String {
    value
        .replace('&', " and ")
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn list(db: &PgPool) -> CoreResult<Vec<EntraValue>> {
    Ok(sqlx::query_as!(
        EntraValue,
        r#"SELECT v.value_display AS value, v.value_normalized AS normalized,
                  v.department_id, d.name AS department_name
             FROM department_entra_values v JOIN departments d ON d.id = v.department_id
            ORDER BY d.name, v.value_display"#
    )
    .fetch_all(db)
    .await?)
}

/// Advisory lock key ("PDMSEMAP") that orders mapping changes against the
/// sync applying a person. A mapping change takes it exclusively, the sync
/// takes it shared, each as the first statement of its transaction, so a
/// person is never applied from a mapping that changes under them.
///
/// Lock order: the sync's own `entra_sync::LOCK_KEY` is taken first (and only
/// by the run), then this one; a mapping change takes only this one. There is
/// no cycle, so no deadlock.
pub const MAPPING_LOCK: i64 = 0x5044_4d53_454d_4150;

/// The department for a raw attribute value, if it is mapped.
pub async fn lookup(db: &PgPool, value: Option<&str>) -> CoreResult<Option<Uuid>> {
    let mut conn = db.acquire().await?;
    lookup_in(&mut conn, value).await
}

/// `lookup` on a given connection (the sync's transaction).
pub(super) async fn lookup_in(
    conn: &mut PgConnection,
    value: Option<&str>,
) -> CoreResult<Option<Uuid>> {
    let Some(n) = value.map(normalize).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    Ok(sqlx::query_scalar!(
        "SELECT department_id FROM department_entra_values WHERE value_normalized = $1",
        n
    )
    .fetch_optional(conn)
    .await?)
}

/// Map a value to a department (replacing any earlier mapping of the same
/// value). People already synced with that value get the department at once.
pub async fn upsert(db: &PgPool, ctx: &Ctx, value: &str, department_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let display = value.trim();
    let n = normalize(display);
    if n.is_empty() || display.chars().count() > 200 {
        return Err(CoreError::validation(
            "enter the Department value as it appears in Entra ID (1–200 characters)",
        ));
    }
    let mut tx = db.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", MAPPING_LOCK)
        .execute(&mut *tx)
        .await?;
    let dept = sqlx::query_scalar!("SELECT name FROM departments WHERE id = $1", department_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| CoreError::validation("choose a department"))?;
    let before = sqlx::query_scalar!(
        "SELECT d.name FROM department_entra_values v JOIN departments d ON d.id = v.department_id
          WHERE v.value_normalized = $1",
        n
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO department_entra_values (value_normalized, value_display, department_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (value_normalized)
         DO UPDATE SET value_display = EXCLUDED.value_display, department_id = EXCLUDED.department_id",
        n,
        display,
        department_id
    )
    .execute(&mut *tx)
    .await?;
    let mut events = vec![
        AuditEvent::new(
            Category::Admin,
            "directory.department_mapping_changed",
            ctx.actor.clone(),
            "department",
        )
        .object(department_id, Some(&dept))
        .department(Some(department_id))
        .change("department", before, dept.clone())
        .context("entra_value", display),
    ];
    events.extend(reapply(&mut tx, ctx, &n, Some(department_id)).await?);
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(())
}

/// Remove a mapping. People synced with that value lose its department at once.
pub async fn remove(db: &PgPool, ctx: &Ctx, value: &str) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let n = normalize(value);
    let mut tx = db.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", MAPPING_LOCK)
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query!(
        "DELETE FROM department_entra_values v USING departments d
          WHERE v.value_normalized = $1 AND d.id = v.department_id
          RETURNING v.value_display, v.department_id, d.name",
        n
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    let mut events = vec![
        AuditEvent::new(
            Category::Admin,
            "directory.department_mapping_changed",
            ctx.actor.clone(),
            "department",
        )
        .object(row.department_id, Some(&row.name))
        .department(Some(row.department_id))
        .change("department", row.name.as_str(), AuditValue::Null)
        .context("entra_value", row.value_display),
    ];
    events.extend(reapply(&mut tx, ctx, &n, None).await?);
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(())
}

/// Apply a changed mapping of `normalized` (now `want`) to the active synced
/// people whose stored Department value is it, as the sync would; a delta
/// round reads only people whose Entra account changed. Returns the audit
/// events for the people whose memberships changed.
async fn reapply(
    conn: &mut PgConnection,
    ctx: &Ctx,
    normalized: &str,
    want: Option<Uuid>,
) -> CoreResult<Vec<AuditEvent>> {
    let people = sqlx::query!(
        r#"SELECT id, display_name, entra_department AS "entra_department!"
             FROM users
            WHERE source = 'entra' AND status = 'active' AND entra_department IS NOT NULL
            ORDER BY id"#
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut events = Vec::new();
    for p in people
        .iter()
        .filter(|p| normalize(&p.entra_department) == normalized)
    {
        let department = Some(p.entra_department.as_str());
        if let Some((_, event)) =
            entra_sync::set_idp_department(conn, ctx, p.id, &p.display_name, department, want)
                .await?
        {
            events.push(event);
        }
    }
    Ok(events)
}

/// Department values of active Entra people that map to nothing, most people first.
pub async fn unmapped(db: &PgPool) -> CoreResult<Vec<UnmappedValue>> {
    let mapped: BTreeSet<String> =
        sqlx::query_scalar!("SELECT value_normalized FROM department_entra_values")
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    let values = sqlx::query_scalar!(
        r#"SELECT coalesce(entra_department, '') AS "value!"
             FROM users WHERE source = 'entra' AND status = 'active'"#
    )
    .fetch_all(db)
    .await?;
    let mut counts: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for raw in values {
        let n = normalize(&raw);
        if mapped.contains(&n) {
            continue;
        }
        counts
            .entry(n)
            .or_insert_with(|| (raw.trim().to_owned(), 0))
            .1 += 1;
    }
    let mut out: Vec<UnmappedValue> = counts
        .into_iter()
        .map(|(normalized, (value, users))| UnmappedValue {
            value,
            normalized,
            users,
        })
        .collect();
    out.sort_by(|a, b| b.users.cmp(&a.users).then_with(|| a.value.cmp(&b.value)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_folds_case_ampersands_and_spaces() {
        assert_eq!(normalize("Road & Bridge"), "road and bridge");
        assert_eq!(normalize("  road AND   bridge "), "road and bridge");
        assert_eq!(normalize("R&B"), "r and b");
        assert_eq!(normalize("   "), "");
    }
}
