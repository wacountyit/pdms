// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! People and departments from Microsoft Entra ID (spec §7.5, D-086).
//!
//! `/users/delta` gives every member account once, then only changes. Each
//! account is applied idempotently (delta can replay changes):
//! - enabled members are created or updated; their Department value picks one
//!   `idp` department membership (manual memberships are never touched, and a
//!   manual membership in the mapped department satisfies the mapping);
//! - disabled or removed accounts become `departed` (sessions revoked) through
//!   `set_user_status`, which also refuses to remove the last administrator;
//! - a round that lists every account (the first, a full resync, an expired
//!   delta token, a changed attribute or `include_guests`) also departs this
//!   issuer's active accounts it did not list and that existed when the round
//!   started, unless the round looks wrong (no members, or more than
//!   max(5, 20%) of them missing);
//! - accounts with a local password are departed like any other, and reported;
//! - guests (`userType` = `Guest`) are skipped unless `include_guests`;
//! - nobody is reactivated, except under the group rule below, and runs
//!   happen only while `[entra_sync] enabled` is true.
//!
//! With `[entra_sync] group_id` set (GS), only the group's members
//! have accounts; members of groups inside it count. Each run reads the
//! member list before it applies anything, so a list it can't read changes
//! nothing. Then:
//! - removed, disabled and guest accounts are handled as above; these
//!   reasons come first;
//! - an account outside the group is neither created nor updated;
//! - a member this round did not list, with no account, is read with
//!   `/users/{id}` and applied (added to the group with no other change in
//!   Entra);
//! - active accounts outside the group are departed, every run, and marked
//!   `departed_from_group`. The same guards apply: the last administrator
//!   stays, nobody is departed when the group lists no members, and when
//!   more than max(5, 20%) would leave at once they are held back and listed
//!   in the report for an administrator to confirm. An account a complete
//!   round did not list is gone from Entra, not just from the group, and is
//!   left to the sweep above;
//! - someone the sync departed for leaving the group is reactivated once
//!   they are back in it, with an enabled member account: at the next run, or
//!   when they sign in with Microsoft ([`sync_one`]). A departure made by
//!   hand, or because Entra removed or disabled the account, is never undone:
//!   when Entra later reports such a person removed, disabled or a guest, or a
//!   complete round no longer lists them, the departure becomes Entra's.
//!
//! At sign-in with a group set, [`sync_one`] asks Entra whether the person is
//! in it before anything else: someone outside it is refused, without an
//! account, and a Graph error refuses the sign-in too.
//!
//! Accounts are matched by issuer and Entra object ID only, never by email.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::{Capability, role_keys};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::entra_values;
use crate::Core;
use crate::auth::oidc::normalize_issuer;
use crate::config::Config;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::graph::{DeltaPage, GraphClient, GraphError};
use crate::jobs::{self, NewJob};
use crate::model::{MembershipSource, UserStatus};
use crate::settings::{self, Check};

pub const SYNC_JOB: &str = "directory.entra_sync";
/// Runs every 15 minutes; enqueues the daily sync once `run_at` has passed.
pub const TICK_JOB: &str = "directory.entra_sync_tick";
/// Postgres advisory lock: one sync at a time ("PDMSENTR").
pub const LOCK_KEY: i64 = 0x5044_4d53_454e_5452;
pub(crate) const CURSOR: &str = "entra.users";
const BASE_SELECT: &str = "id,displayName,mail,userPrincipalName,accountEnabled,userType";

/// Stored as `sync_cursors.last_summary`; missing fields default, so a
/// summary written before a field was added still reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncReport {
    pub created: u32,
    pub updated: u32,
    pub departed: u32,
    /// People applied this run whose Department value maps to nothing.
    pub unmapped: u32,
    /// Departments that gained or lost an Entra-managed member.
    pub changed_departments: BTreeSet<Uuid>,
    /// Accounts that need a person's attention (not linked, last admin, ...).
    pub errors: Vec<String>,
    /// People the sync departed for leaving the PDMS group and reactivated
    /// because they are back in it.
    pub reactivated: Vec<Uuid>,
    /// Active accounts outside the PDMS group that were not departed because
    /// more would have left at once than the limit allows.
    pub held_back: Vec<Uuid>,
}

#[derive(Clone, Debug, Default)]
pub struct SyncStatus {
    pub last_run: Option<DateTime<Utc>>,
    /// Why the last run failed; cleared by the next run that succeeds.
    pub last_error: Option<String>,
    pub last_summary: Option<SyncReport>,
}

/// Where someone stands with the PDMS group in this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Membership {
    /// No group is set: everyone in the tenant may have an account.
    Everyone,
    /// In the group, directly or through a group inside it.
    Member,
    /// Not in the group.
    Outside,
}

impl Membership {
    /// `members`: the group's member list (lowercase IDs), when a group is set.
    fn of(members: Option<&BTreeSet<String>>, oid: &str) -> Self {
        match members {
            None => Self::Everyone,
            Some(m) if m.contains(&oid.to_ascii_lowercase()) => Self::Member,
            Some(_) => Self::Outside,
        }
    }
}

/// One entry of `/users/delta` or `/users/{id}`.
struct EntraUser {
    id: String,
    removed: bool,
    enabled: bool,
    guest: bool,
    name: Option<String>,
    email: Option<String>,
    department: Option<String>,
}

fn select_list(attribute: &str) -> String {
    if BASE_SELECT.split(',').any(|p| p == attribute) {
        BASE_SELECT.to_owned()
    } else {
        format!("{BASE_SELECT},{attribute}")
    }
}

/// What a delta link was started with, kept in `sync_cursors.delta_select`:
/// the `$select`, followed by `;guests` when guests are included, then
/// `;group=<id>` when the sync is limited to a group. A link keeps its
/// original `$select`, and a delta lists only changed accounts, so guests
/// skipped before would not come back; a different key means a full round.
fn cursor_key(select: &str, include_guests: bool, group: Option<&str>) -> String {
    let mut key = select.to_owned();
    if include_guests {
        key.push_str(";guests");
    }
    if let Some(g) = group {
        key.push_str(";group=");
        key.push_str(g);
    }
    key
}

/// What an administrator sees when Graph refuses to list a group's members.
pub const MISSING_PERMISSION: &str = "PDMS can't read the group's members: add the GroupMember.Read.All permission (ADMIN_GUIDE, Entra ID sync)";

/// The user IDs (lowercase) of everyone in the group, including members of
/// groups inside it.
pub(crate) async fn group_members(
    graph: &GraphClient,
    group: &str,
) -> CoreResult<BTreeSet<String>> {
    let rows: Vec<Value> = graph
        .get_all(&format!(
            "/groups/{group}/transitiveMembers/microsoft.graph.user?$select=id&$top=999"
        ))
        .await
        .map_err(group_error)?;
    Ok(rows
        .iter()
        .filter_map(|v| v.get("id").and_then(Value::as_str))
        .map(str::to_lowercase)
        .collect())
}

fn group_error(e: GraphError) -> CoreError {
    match e {
        GraphError::Auth(_) => CoreError::validation(MISSING_PERMISSION),
        other => other.into(),
    }
}

/// Check a group ID without saving it: it is a GUID, Entra has the group, PDMS
/// may read its members. Records `settings.tested` (requires `ManageSettings`).
pub async fn test_group(core: &Core, ctx: &Ctx, group_id: &str) -> CoreResult<Check> {
    ctx.require(Capability::ManageSettings)?;
    let check = probe_group(core, group_id).await;
    settings::record_test(core, ctx, "entra_sync", check.ok).await?;
    Ok(check)
}

async fn probe_group(core: &Core, group_id: &str) -> Check {
    let Ok(id) = Uuid::parse_str(group_id.trim()) else {
        return Check::failed(
            vec![],
            "Enter the group's object ID: Entra admin center → Groups → the group → Object ID.",
        );
    };
    let id = id.hyphenated().to_string();
    let Some(graph) = core.graph() else {
        return Check::failed(vec![], "Set up the Microsoft 365 connection first.");
    };
    let group: Value = match graph
        .get(&format!("/groups/{id}?$select=id,displayName"))
        .await
    {
        Ok(v) => v,
        Err(GraphError::NotFound) => {
            return Check::failed(
                vec![],
                format!("Entra has no group with the object ID {id}."),
            );
        }
        Err(e) => return Check::failed(vec![], group_error(e).to_string()),
    };
    let name = group
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or(&id);
    let found = (true, format!("Group \"{name}\" found."));
    match group_members(&graph, &id).await {
        Ok(members) => Check::passed(vec![
            found,
            (
                true,
                format!(
                    "It has {} people, counting members of groups inside it.",
                    members.len()
                ),
            ),
        ]),
        Err(e) => Check::failed(vec![found], e.to_string()),
    }
}

fn require_enabled(core: &Core) -> CoreResult<()> {
    if core.config().entra_sync.enabled {
        Ok(())
    } else {
        Err(CoreError::validation(
            "Entra sync is turned off ([entra_sync] enabled = false)",
        ))
    }
}

/// Synced people sign in with OIDC; their identity uses the configured issuer.
fn entra_issuer(config: &Config) -> CoreResult<String> {
    let issuer = normalize_issuer(&config.auth.oidc.issuer_url);
    if issuer.is_empty() {
        return Err(CoreError::validation(
            "auth.oidc.issuer_url must be set so synced people can sign in",
        ));
    }
    Ok(issuer.to_owned())
}

fn graph(core: &Core) -> CoreResult<GraphClient> {
    core.graph().ok_or_else(|| {
        CoreError::validation(
            "Microsoft Graph is not configured ([graph] tenant_id, client_id, certificate)",
        )
    })
}

fn valid_email(e: &str) -> bool {
    e.len() <= 320
        && e.matches('@').count() == 1
        && !e.contains(char::is_whitespace)
        && e.split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && !d.is_empty())
}

fn parse_user(v: &Value, attribute: &str) -> Option<EntraUser> {
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some(EntraUser {
        id: v.get("id")?.as_str()?.to_owned(),
        removed: v.get("@removed").is_some(),
        enabled: v
            .get("accountEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        // A missing or null userType is a member (older member accounts).
        guest: v
            .get("userType")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("guest")),
        name: text("displayName").map(|n| n.chars().take(200).collect()),
        email: text("mail")
            .filter(|e| valid_email(e))
            .or_else(|| text("userPrincipalName").filter(|e| valid_email(e))),
        department: text(attribute),
    })
}

/// One account in the sync preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewUser {
    pub name: String,
    pub email: String,
    /// The value of the department attribute in Entra.
    pub department: String,
    /// The PDMS department it maps to.
    pub maps_to: Option<String>,
    /// Why the sync would skip this account (guest, disabled, no email).
    pub skipped: Option<&'static str>,
}

/// The first accounts Microsoft Graph returns, with what the sync would do
/// with each, changing nothing (requires `ManageDirectory`).
pub async fn preview(core: &Core, ctx: &Ctx) -> CoreResult<Vec<PreviewUser>> {
    ctx.require(Capability::ManageDirectory)?;
    let graph = graph(core)?;
    let config = core.config();
    let attribute = config.entra_sync.attribute.as_str();
    let page: Value = graph
        .get(&format!(
            "/users?$select={}&$top=10",
            select_list(attribute)
        ))
        .await?;
    let mut out = Vec::new();
    for v in page
        .get("value")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(u) = parse_user(v, attribute) else {
            continue;
        };
        let maps_to = match super::entra_values::lookup(&core.db, u.department.as_deref()).await? {
            Some(id) => {
                sqlx::query_scalar!("SELECT name FROM departments WHERE id = $1", id)
                    .fetch_optional(&core.db)
                    .await?
            }
            None => None,
        };
        let skipped = if u.guest && !config.entra_sync.include_guests {
            Some("guest account")
        } else if !u.enabled {
            Some("disabled")
        } else if u.email.is_none() {
            Some("no email address")
        } else {
            None
        };
        out.push(PreviewUser {
            name: u.name.unwrap_or_default(),
            email: u.email.unwrap_or_default(),
            department: u.department.unwrap_or_default(),
            maps_to,
            skipped,
        });
    }
    Ok(out)
}

/// Incremental sync (falls back to a full one when Graph asks for it).
pub async fn run(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport> {
    run_inner(core, ctx, false).await
}

/// Read every account again, ignoring the saved cursor.
pub async fn run_full(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport> {
    run_inner(core, ctx, true).await
}

async fn run_inner(core: &Core, ctx: &Ctx, full: bool) -> CoreResult<SyncReport> {
    ctx.require(Capability::ManageDirectory)?;
    // Configuration checks the sign-in settings the sync needs only while it
    // is on (subject_claim = "oid"); a run without them could duplicate or
    // depart SSO accounts.
    require_enabled(core)?;
    let graph = graph(core)?;
    let issuer = entra_issuer(&core.config())?;
    let mut lock = core.db.begin().await?;
    let got = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_xact_lock($1) AS "got!""#,
        LOCK_KEY
    )
    .fetch_one(&mut *lock)
    .await?;
    if !got {
        return Err(CoreError::Conflict(
            "a directory sync is already running".into(),
        ));
    }
    let result = sync_all(core, ctx, &graph, &issuer, full).await;
    if let Err(e) = &result {
        record_failure(&core.db, &e.to_string()).await?;
    }
    lock.rollback().await?;
    result
}

async fn sync_all(
    core: &Core,
    ctx: &Ctx,
    graph: &GraphClient,
    issuer: &str,
    full: bool,
) -> CoreResult<SyncReport> {
    // Before the first Graph request: accounts created after this (a first
    // sign-in during the round) may be missing from its listing.
    let started = sqlx::query_scalar!(r#"SELECT now() AS "now!""#)
        .fetch_one(&core.db)
        .await?;
    let config = core.config();
    let attribute = config.entra_sync.attribute.as_str();
    let select = select_list(attribute);
    let group = config.entra_sync.group();
    let key = cursor_key(&select, config.entra_sync.include_guests, group.as_deref());
    // With a group set, its member list comes first: a list that can't be
    // read fails the run before anything changes, and the cursor stays.
    let in_group = match group.as_deref() {
        Some(g) => Some(group_members(graph, g).await?),
        None => None,
    };
    let initial = format!("/users/delta?$select={select}");
    let saved = if full {
        None
    } else {
        saved_link(&core.db, &key).await?
    };
    // A round started from `initial` lists every account; a delta round only changes.
    let (page, complete): (DeltaPage<Value>, bool) = match saved {
        Some(link) => match graph.get_delta(&link).await {
            Err(GraphError::ResyncRequired) => {
                tracing::warn!("Entra delta token no longer valid; reading every account again");
                (graph.get_delta(&initial).await?, true)
            }
            other => (other?, false),
        },
        None => (graph.get_delta(&initial).await?, true),
    };

    let mut report = SyncReport::default();
    let mut seen = BTreeSet::new();
    let mut members = 0usize;
    for v in &page.items {
        match parse_user(v, attribute) {
            Some(u) => {
                seen.insert(u.id.to_ascii_lowercase());
                if !u.removed && !u.guest {
                    members += 1;
                }
                let membership = Membership::of(in_group.as_ref(), &u.id);
                apply(core, ctx, issuer, &u, membership, &mut report).await?;
            }
            None => report
                .errors
                .push("an entry without an id was skipped".into()),
        }
    }
    // A round that listed every account: who it did not list is gone from Entra.
    let listed = complete.then_some(&seen);
    if let Some(in_group) = &in_group {
        add_missing_members(core, ctx, graph, issuer, in_group, &seen, &mut report).await?;
        group_sweep(core, ctx, issuer, started, in_group, listed, &mut report).await?;
    }
    if complete {
        sweep(core, ctx, issuer, started, &seen, members, &mut report).await?;
        if members > 0 {
            unmark_missing_from_entra(core, ctx, issuer, &seen).await?;
        }
    }
    for e in &report.errors {
        tracing::warn!(problem = %e, "Entra sync");
    }
    if !report.held_back.is_empty() {
        tracing::warn!(
            held_back = report.held_back.len(),
            "Entra sync: departures for leaving the PDMS group were held back"
        );
    }

    // Problems with single accounts stay in the summary; `last_error` is
    // only for a run that failed.
    let summary = serde_json::to_value(&report).map_err(CoreError::internal)?;
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "INSERT INTO sync_cursors (name, delta_link, delta_select, last_run, last_error, last_summary)
         VALUES ($1, $2, $3, now(), NULL, $4)
         ON CONFLICT (name) DO UPDATE SET delta_link = EXCLUDED.delta_link,
                delta_select = EXCLUDED.delta_select, last_run = EXCLUDED.last_run,
                last_error = NULL, last_summary = EXCLUDED.last_summary",
        CURSOR,
        page.delta_link,
        key,
        summary
    )
    .execute(&mut *tx)
    .await?;
    let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "directory.synced",
                ctx.actor.clone(),
                "directory",
            )
            .context("full", full)
            .context("created", report.created)
            .context("updated", report.updated)
            .context("departed", report.departed)
            .context("unmapped", report.unmapped)
            .context("departments_changed", n(report.changed_departments.len()))
            .context("problems", n(report.errors.len()))
            .context("reactivated", n(report.reactivated.len()))
            .context("held_back", n(report.held_back.len())),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(report)
}

/// The saved delta link, if it was started with the same [`cursor_key`]:
/// after the attribute changes, changed people would come back without it,
/// and after `include_guests` changes, guests would not be read again.
async fn saved_link(db: &PgPool, key: &str) -> CoreResult<Option<String>> {
    let row = sqlx::query!(
        "SELECT delta_link, delta_select FROM sync_cursors WHERE name = $1",
        CURSOR
    )
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some(r) if r.delta_select.as_deref() == Some(key) => r.delta_link,
        Some(r) if r.delta_link.is_some() => {
            tracing::info!(
                "the Entra sync attribute or include_guests changed; reading every account again"
            );
            None
        }
        _ => None,
    })
}

/// After a round that listed every account: depart this issuer's active
/// accounts it did not list (deleted while no delta could report it, or
/// before the sync was on) and that existed when the round started. Held
/// back, and reported, when the round looks wrong: no member accounts, or
/// more departures than max(5, 20%).
async fn sweep(
    core: &Core,
    ctx: &Ctx,
    issuer: &str,
    started: DateTime<Utc>,
    seen: &BTreeSet<String>,
    members: usize,
    report: &mut SyncReport,
) -> CoreResult<()> {
    let linked = linked_accounts(&core.db, issuer, started).await?;
    let missing: Vec<_> = linked
        .iter()
        .filter(|r| !seen.contains(&r.oidc_subject.to_ascii_lowercase()))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let kept = kept_admin(&core.db, &missing).await?;
    let departing: Vec<_> = missing.iter().filter(|r| Some(r.id) != kept).collect();
    let limit = departure_limit(linked.len());
    let held = if departing.is_empty() {
        None
    } else if members == 0 {
        Some(format!(
            "Entra listed no member accounts, so the {} linked PDMS account(s) it did not list \
             were not departed",
            departing.len()
        ))
    } else if departing.len() > limit {
        Some(format!(
            "{} of {} linked PDMS accounts are not in Entra (more than {limit} at once), \
             so none was departed",
            departing.len(),
            linked.len()
        ))
    } else {
        None
    };
    if let Some(held) = held {
        const NAMED: usize = 20;
        let mut names = departing
            .iter()
            .take(NAMED)
            .map(|r| format!("{} <{}>", r.display_name, r.email))
            .collect::<Vec<_>>()
            .join(", ");
        if departing.len() > NAMED {
            names = format!("{names} and {} more", departing.len() - NAMED);
        }
        report.errors.push(format!(
            "{held}: {names}. Check Entra and the Graph permissions; if the departures are \
             real, depart those people by hand on their pages"
        ));
        return Ok(());
    }
    for r in missing {
        depart(
            core,
            ctx,
            &r.who(),
            Leaving::Entra("is not in Entra"),
            report,
        )
        .await?;
    }
    Ok(())
}

/// With a group set, every run: depart this issuer's active accounts outside
/// the group that existed when the round started, marked so they come back
/// if they rejoin. Nobody is departed when the group lists no members (a
/// wrong ID, or a dynamic group still filling), which is reported; nor when
/// more would leave than max(5, 20%) of linked accounts at once (narrowing
/// the group), when they are listed in `held_back` for an administrator to
/// confirm.
///
/// `listed`: every account a complete round listed. An account it did not
/// list is gone from Entra, not just from the group, so it is left to
/// [`sweep`]: that departure is Entra's, which the group rule never undoes.
async fn group_sweep(
    core: &Core,
    ctx: &Ctx,
    issuer: &str,
    started: DateTime<Utc>,
    in_group: &BTreeSet<String>,
    listed: Option<&BTreeSet<String>>,
    report: &mut SyncReport,
) -> CoreResult<()> {
    let linked = linked_accounts(&core.db, issuer, started).await?;
    let outside: Vec<_> = linked
        .iter()
        .filter(|r| {
            let oid = r.oidc_subject.to_ascii_lowercase();
            !in_group.contains(&oid) && listed.is_none_or(|l| l.contains(&oid))
        })
        .collect();
    if outside.is_empty() {
        return Ok(());
    }
    if in_group.is_empty() {
        report.errors.push(format!(
            "The PDMS group lists no members, so the {} linked account(s) outside it were not \
             departed. Check the group's object ID and its members.",
            outside.len()
        ));
        return Ok(());
    }
    let kept = kept_admin(&core.db, &outside).await?;
    let departing: Vec<Uuid> = outside
        .iter()
        .map(|r| r.id)
        .filter(|id| Some(*id) != kept)
        .collect();
    if departing.len() > departure_limit(linked.len()) {
        report.held_back = departing;
        return Ok(());
    }
    for r in outside {
        depart(core, ctx, &r.who(), Leaving::Group, report).await?;
    }
    Ok(())
}

/// Members of the group this round did not list: added to the group with no
/// other change in Entra, or back in it. Each one PDMS has no account for,
/// or departed for leaving the group, is read and applied. Anyone else is
/// known: active, or departed for another reason, which the group never undoes.
async fn add_missing_members(
    core: &Core,
    ctx: &Ctx,
    graph: &GraphClient,
    issuer: &str,
    in_group: &BTreeSet<String>,
    seen: &BTreeSet<String>,
    report: &mut SyncReport,
) -> CoreResult<()> {
    let unseen: Vec<String> = in_group.difference(seen).cloned().collect();
    if unseen.is_empty() {
        return Ok(());
    }
    let known: BTreeSet<String> = sqlx::query_scalar!(
        r#"SELECT lower(oidc_subject) AS "subject!" FROM users
            WHERE oidc_issuer = $1 AND lower(oidc_subject) = ANY($2)
              AND (status = 'active' OR NOT departed_from_group)"#,
        issuer,
        &unseen[..]
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .collect();
    let config = core.config();
    let attribute = config.entra_sync.attribute.as_str();
    let select = select_list(attribute);
    for oid in unseen.iter().filter(|o| !known.contains(*o)) {
        // Only an object ID goes into the request path.
        let Ok(oid) = Uuid::parse_str(oid) else {
            report.errors.push(format!(
                "the PDMS group lists a member with an unexpected ID {oid}"
            ));
            continue;
        };
        let path = format!("/users/{}?$select={select}", oid.hyphenated());
        let v: Value = match graph.get(&path).await {
            Ok(v) => v,
            // Deleted since the member list was read.
            Err(GraphError::NotFound) => continue,
            Err(e) => return Err(e.into()),
        };
        match parse_user(&v, attribute) {
            Some(u) => {
                apply(core, ctx, issuer, &u, Membership::Member, report).await?;
            }
            None => report
                .errors
                .push("an entry without an id was skipped".into()),
        }
    }
    Ok(())
}

/// An active account linked to this issuer that existed when the round
/// started: what both sweeps may depart.
struct LinkedAccount {
    id: Uuid,
    display_name: String,
    email: String,
    oidc_subject: String,
    local_login: bool,
}

impl LinkedAccount {
    fn who(&self) -> Linked<'_> {
        Linked {
            id: self.id,
            name: &self.display_name,
            email: &self.email,
            local_login: self.local_login,
        }
    }
}

/// Accounts created after `started` (a first sign-in during the round) may
/// be missing from its listing, so they are left out.
async fn linked_accounts(
    db: &PgPool,
    issuer: &str,
    started: DateTime<Utc>,
) -> CoreResult<Vec<LinkedAccount>> {
    Ok(sqlx::query_as!(
        LinkedAccount,
        r#"SELECT u.id, u.display_name, u.email, u.oidc_subject AS "oidc_subject!",
                  EXISTS (SELECT 1 FROM local_credentials c WHERE c.user_id = u.id) AS "local_login!"
             FROM users u
            WHERE u.oidc_issuer = $1 AND u.status = 'active' AND u.created_at < $2
            ORDER BY u.display_name"#,
        issuer,
        started
    )
    .fetch_all(db)
    .await?)
}

/// The most a sweep departs at once: max(5, 20% of linked accounts).
fn departure_limit(linked: usize) -> usize {
    (linked / 5).max(5)
}

/// Departing keeps the last active administrator: when every one of them is
/// in `leaving`, the last of them in that order stays, so it does not count
/// as a departure (and is reported when its departure is refused).
async fn kept_admin(db: &PgPool, leaving: &[&LinkedAccount]) -> CoreResult<Option<Uuid>> {
    let admins = sqlx::query_scalar!(
        r#"SELECT DISTINCT ra.user_id FROM role_assignments ra
             JOIN roles r ON r.id = ra.role_id JOIN users u ON u.id = ra.user_id
            WHERE r.key = $1 AND ra.department_id IS NULL AND u.status = 'active'"#,
        role_keys::ADMIN
    )
    .fetch_all(db)
    .await?;
    Ok(admins
        .iter()
        .all(|a| leaving.iter().any(|r| r.id == *a))
        .then(|| leaving.iter().rev().find(|r| admins.contains(&r.id)))
        .flatten()
        .map(|r| r.id))
}

/// An active PDMS account linked to an Entra account.
struct Linked<'a> {
    id: Uuid,
    name: &'a str,
    email: &'a str,
    local_login: bool,
}

/// Why the sync departs someone.
#[derive(Clone, Copy)]
enum Leaving {
    /// Entra no longer has them as an enabled member; the text says why.
    Entra(&'static str),
    /// They are no longer in the PDMS group: marked, so they can come back.
    Group,
}

/// Depart someone. The last administrator is reported instead; a departed
/// account that had a local password is reported too, so an administrator
/// knows.
async fn depart(
    core: &Core,
    ctx: &Ctx,
    who: &Linked<'_>,
    why: Leaving,
    report: &mut SyncReport,
) -> CoreResult<()> {
    let (name, email) = (who.name, who.email);
    let (result, why) = match why {
        Leaving::Entra(why) => (
            super::set_user_status(&core.db, ctx, who.id, UserStatus::Departed)
                .await
                .map(|()| true),
            why,
        ),
        Leaving::Group => (
            super::depart_from_group(&core.db, ctx, who.id).await,
            "is no longer in the PDMS group",
        ),
    };
    match result {
        // No longer active by the time it was departed: nothing changed.
        Ok(false) => {}
        Ok(true) => {
            report.departed += 1;
            if who.local_login {
                report.errors.push(format!(
                    "{name} <{email}> {why} and was departed; the account also had a local password"
                ));
            }
        }
        Err(CoreError::Conflict(reason)) => report.errors.push(format!(
            "{name} <{email}> {why} but was not deactivated: {reason}"
        )),
        Err(err) => return Err(err),
    }
    Ok(())
}

/// Keep the saved cursor (no change is lost) and remember why the run failed.
async fn record_failure(db: &PgPool, error: &str) -> CoreResult<()> {
    let error: String = error.chars().take(500).collect();
    sqlx::query!(
        "INSERT INTO sync_cursors (name, last_run, last_error) VALUES ($1, now(), $2)
         ON CONFLICT (name) DO UPDATE SET last_run = now(), last_error = EXCLUDED.last_error",
        CURSOR,
        error
    )
    .execute(db)
    .await?;
    Ok(())
}

/// What Entra says about someone signing in with Microsoft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInLookup {
    /// An enabled member (of the PDMS group, when one is set) with an active
    /// account: created, updated or reactivated just now.
    Account(Uuid),
    /// A PDMS group is set and the person is not in it. Nothing was read or
    /// changed.
    NotInGroup,
    /// Not an enabled member, unknown to Entra, or blocked (see the log).
    NoAccount,
}

/// `POST /users/{id}/checkMemberGroups`: the groups of those asked that the
/// user is in, directly or through a group inside them.
#[derive(Deserialize)]
struct MemberGroups {
    value: Vec<String>,
}

/// Look one account up in Entra and apply it, at sign-in. With a PDMS group
/// set, Entra is first asked whether the person is in it: someone outside it
/// is refused without being read, and a member is applied as one (which
/// reactivates an account the sync departed for leaving the group). Any Graph
/// error is returned, so the sign-in is refused, never let through.
pub async fn sync_one(core: &Core, oid: &str) -> CoreResult<SignInLookup> {
    let oid = Uuid::parse_str(oid)
        .map_err(|_| CoreError::validation("not an Entra object ID"))?
        .hyphenated()
        .to_string();
    let graph = graph(core)?;
    let config = core.config();
    let issuer = entra_issuer(&config)?;
    let membership = match config.entra_sync.group() {
        Some(group) => {
            let found: MemberGroups = graph
                .post(
                    &format!("/users/{oid}/checkMemberGroups"),
                    &json!({ "groupIds": [&group] }),
                )
                .await
                .map_err(group_error)?;
            if !found.value.iter().any(|g| g.eq_ignore_ascii_case(&group)) {
                return Ok(SignInLookup::NotInGroup);
            }
            Membership::Member
        }
        None => Membership::Everyone,
    };
    let attribute = config.entra_sync.attribute.as_str();
    let path = format!("/users/{oid}?$select={}", select_list(attribute));
    let v: Value = match graph.get(&path).await {
        Ok(v) => v,
        Err(GraphError::NotFound) => return Ok(SignInLookup::NoAccount),
        Err(e) => return Err(e.into()),
    };
    let user = parse_user(&v, attribute)
        .ok_or_else(|| CoreError::internal("Graph returned a user without an id"))?;
    let mut report = SyncReport::default();
    let id = apply(
        core,
        &Ctx::system("entra-sync"),
        &issuer,
        &user,
        membership,
        &mut report,
    )
    .await?;
    for e in &report.errors {
        tracing::warn!(oid = %oid, problem = %e, "Entra sign-in lookup");
    }
    Ok(id.map_or(SignInLookup::NoAccount, SignInLookup::Account))
}

/// Apply one account. Returns the PDMS user ID when the person is an active member.
async fn apply(
    core: &Core,
    ctx: &Ctx,
    issuer: &str,
    u: &EntraUser,
    membership: Membership,
    report: &mut SyncReport,
) -> CoreResult<Option<Uuid>> {
    let existing = sqlx::query!(
        r#"SELECT u.id, u.display_name, u.email, u.status AS "status: UserStatus",
                  u.source::text AS "source!", u.departed_from_group,
                  EXISTS (SELECT 1 FROM local_credentials c WHERE c.user_id = u.id) AS "local_login!"
             FROM users u WHERE u.oidc_issuer = $1 AND u.oidc_subject = $2"#,
        issuer,
        u.id
    )
    .fetch_optional(&core.db)
    .await?;

    // Entra's reasons come first, so they are never "left the group".
    let leaving = u.removed || !u.enabled || (u.guest && !core.config().entra_sync.include_guests);
    if leaving {
        if let Some(e) = existing.as_ref() {
            let why = if u.removed {
                "was removed from Entra"
            } else if !u.enabled {
                "is disabled in Entra"
            } else {
                "is a guest account in Entra"
            };
            if e.status == UserStatus::Active {
                let who = Linked {
                    id: e.id,
                    name: &e.display_name,
                    email: &e.email,
                    local_login: e.local_login,
                };
                depart(core, ctx, &who, Leaving::Entra(why), report).await?;
            } else if e.departed_from_group {
                entra_takes_over_departure(&core.db, ctx, e.id, &e.display_name, why).await?;
            }
        }
        return Ok(None);
    }
    // Outside the group: neither created nor updated. The group sweep departs
    // an active account, under its guards.
    if membership == Membership::Outside {
        return Ok(None);
    }
    // Back in the group after the sync departed them for leaving it.
    let back = match existing.as_ref() {
        Some(e)
            if membership == Membership::Member
                && e.status != UserStatus::Active
                && e.departed_from_group =>
        {
            let back = super::reactivate_back_in_group(&core.db, ctx, e.id).await?;
            if back {
                report.reactivated.push(e.id);
            }
            back
        }
        _ => false,
    };

    let mut events = Vec::new();
    let mut tx = core.db.begin().await?;
    // First statement: wait for any mapping change in progress, and hold off
    // new ones until this person is written (lock order: LOCK_KEY, then this).
    sqlx::query!(
        "SELECT pg_advisory_xact_lock_shared($1)",
        entra_values::MAPPING_LOCK
    )
    .execute(&mut *tx)
    .await?;
    let want = entra_values::lookup_in(&mut tx, u.department.as_deref()).await?;
    let (id, name, created, changed) = match existing {
        Some(e) if e.status != UserStatus::Active && !back => {
            // Never reactivated automatically, apart from the group rule
            // above: an administrator decides.
            sqlx::query!(
                "UPDATE users SET entra_department = $2, entra_synced_at = now() WHERE id = $1",
                e.id,
                u.department
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(None);
        }
        Some(e) => {
            let name = u.name.clone().unwrap_or_else(|| e.display_name.clone());
            let email = u.email.clone().unwrap_or_else(|| e.email.clone());
            let res = sqlx::query!(
                "UPDATE users SET display_name = $2, email = $3, entra_department = $4,
                        entra_synced_at = now(), source = 'entra', updated_at = now()
                  WHERE id = $1",
                e.id,
                name,
                email,
                u.department
            )
            .execute(&mut *tx)
            .await;
            if let Err(err) = res {
                return email_conflict(err, report, &email);
            }
            let ev = AuditEvent::new(Category::Admin, "user.updated", ctx.actor.clone(), "user")
                .object(e.id, Some(&name))
                .change("display_name", e.display_name.as_str(), name.as_str())
                .change("email", e.email.as_str(), email.as_str())
                .change("source", e.source.as_str(), "entra");
            let changed = ev.changes.is_some();
            if changed {
                events.push(ev);
            }
            (e.id, name, false, changed)
        }
        None => {
            let Some(email) = u.email.clone() else {
                report.errors.push(format!(
                    "Entra account {} has no usable email or user principal name",
                    u.id
                ));
                return Ok(None);
            };
            let name = u.name.clone().unwrap_or_else(|| email.clone());
            let res = sqlx::query_scalar!(
                "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject, source,
                                    entra_department, entra_synced_at)
                 VALUES ($1, $2, $3, $4, $5, 'entra', $6, now())
                 ON CONFLICT ON CONSTRAINT users_oidc_identity_key DO NOTHING
                 RETURNING id",
                Uuid::now_v7(),
                name,
                email,
                issuer,
                u.id,
                u.department
            )
            .fetch_optional(&mut *tx)
            .await;
            let id = match res {
                // Created concurrently (sign-in during a sync): the other writer won.
                Ok(None) => return Ok(None),
                Ok(Some(id)) => id,
                Err(err) => return email_conflict(err, report, &email),
            };
            events.push(
                AuditEvent::new(
                    Category::Admin,
                    "directory.user_created",
                    ctx.actor.clone(),
                    "user",
                )
                .object(id, Some(&name))
                .change("display_name", AuditValue::Null, name.as_str())
                .change("email", AuditValue::Null, email.as_str())
                .change("oidc_subject", AuditValue::Null, u.id.as_str())
                .context("entra_department", u.department.clone()),
            );
            (id, name, true, false)
        }
    };

    let moved =
        match set_idp_department(&mut tx, ctx, id, &name, u.department.as_deref(), want).await? {
            Some((departments, event)) => {
                report.changed_departments.extend(departments);
                events.push(event);
                true
            }
            None => false,
        };
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    if created {
        report.created += 1;
    } else if changed || moved {
        report.updated += 1;
    }
    if want.is_none() {
        report.unmapped += 1;
    }
    Ok(Some(id))
}

/// After a round that listed every account (and some member accounts):
/// someone the sync departed for leaving the group whom it did not list was
/// deleted from Entra since, so their departure is Entra's now.
async fn unmark_missing_from_entra(
    core: &Core,
    ctx: &Ctx,
    issuer: &str,
    listed: &BTreeSet<String>,
) -> CoreResult<()> {
    let marked = sqlx::query!(
        r#"SELECT id, display_name, oidc_subject AS "oidc_subject!" FROM users
            WHERE oidc_issuer = $1 AND departed_from_group"#,
        issuer
    )
    .fetch_all(&core.db)
    .await?;
    for r in marked
        .iter()
        .filter(|r| !listed.contains(&r.oidc_subject.to_ascii_lowercase()))
    {
        entra_takes_over_departure(&core.db, ctx, r.id, &r.display_name, "is not in Entra").await?;
    }
    Ok(())
}

/// Entra reports someone the sync departed for leaving the group as removed,
/// disabled or a guest: from now on the departure is Entra's, which being
/// back in the group never undoes, so the mark goes (audited).
async fn entra_takes_over_departure(
    db: &PgPool,
    ctx: &Ctx,
    id: Uuid,
    name: &str,
    why: &str,
) -> CoreResult<()> {
    let mut tx = db.begin().await?;
    let cleared = sqlx::query!(
        "UPDATE users SET departed_from_group = false, updated_at = now()
          WHERE id = $1 AND departed_from_group",
        id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if cleared == 0 {
        return Ok(());
    }
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(Category::Admin, "user.updated", ctx.actor.clone(), "user")
                .object(id, Some(name))
                .change("departed_from_group", true, false)
                .context("reason", why),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// An email already belongs to another account: report it, change nothing.
fn email_conflict(
    err: sqlx::Error,
    report: &mut SyncReport,
    email: &str,
) -> CoreResult<Option<Uuid>> {
    match CoreError::conflict_on_unique(err, "") {
        CoreError::Conflict(_) => {
            report.errors.push(format!(
                "{email} is already used by a PDMS account that is not this Entra account; \
                 rename or remove that account, then run a full sync"
            ));
            Ok(None)
        }
        other => Err(other),
    }
}

/// Make `want` the person's only `idp` department, for the sync and for
/// mapping changes (`entra_values`). A `manual` membership in `want` already
/// satisfies it, so no `idp` row is added then; `manual` rows are never
/// touched. When anything changed, returns the departments that gained or
/// lost the person and the `user.idp_memberships_synced` event, which the
/// caller appends in the same transaction.
pub(super) async fn set_idp_department(
    conn: &mut PgConnection,
    ctx: &Ctx,
    user_id: Uuid,
    name: &str,
    entra_department: Option<&str>,
    want: Option<Uuid>,
) -> CoreResult<Option<(BTreeSet<Uuid>, AuditEvent)>> {
    let rows = sqlx::query!(
        r#"SELECT department_id, source AS "source: MembershipSource"
             FROM department_members WHERE user_id = $1"#,
        user_id
    )
    .fetch_all(&mut *conn)
    .await?;
    let have: BTreeSet<Uuid> = rows
        .iter()
        .filter(|r| r.source == MembershipSource::Idp)
        .map(|r| r.department_id)
        .collect();
    let by_hand = rows
        .iter()
        .any(|r| r.source == MembershipSource::Manual && Some(r.department_id) == want);
    let want: BTreeSet<Uuid> = want.filter(|_| !by_hand).into_iter().collect();
    if have == want {
        return Ok(None);
    }
    // SharePoint folder access follows department membership (P4).
    crate::sharepoint::access::enqueue_reconcile(&mut *conn).await?;
    for d in have.difference(&want) {
        sqlx::query!(
            "DELETE FROM department_members WHERE user_id = $1 AND department_id = $2 AND source = 'idp'",
            user_id,
            d
        )
        .execute(&mut *conn)
        .await?;
    }
    for d in want.difference(&have) {
        sqlx::query!(
            "INSERT INTO department_members (department_id, user_id, is_primary, source)
             VALUES ($1, $2, NOT EXISTS (SELECT 1 FROM department_members WHERE user_id = $2 AND is_primary), 'idp')
             ON CONFLICT (department_id, user_id) DO NOTHING",
            d,
            user_id
        )
        .execute(&mut *conn)
        .await?;
    }
    let event = AuditEvent::new(
        Category::Permission,
        "user.idp_memberships_synced",
        ctx.actor.clone(),
        "user",
    )
    .object(user_id, Some(name))
    .change(
        "departments",
        department_names(conn, &have).await?,
        department_names(conn, &want).await?,
    )
    .context("entra_department", entra_department);
    Ok(Some((have.union(&want).copied().collect(), event)))
}

async fn department_names(
    conn: &mut PgConnection,
    ids: &BTreeSet<Uuid>,
) -> CoreResult<Vec<String>> {
    let ids: Vec<Uuid> = ids.iter().copied().collect();
    Ok(sqlx::query_scalar!(
        "SELECT name FROM departments WHERE id = ANY($1) ORDER BY name",
        &ids[..]
    )
    .fetch_all(conn)
    .await?)
}

/// Last run, last failure, and the last report (for the admin page).
pub async fn status(db: &PgPool) -> CoreResult<SyncStatus> {
    let row = sqlx::query!(
        "SELECT last_run, last_error, last_summary FROM sync_cursors WHERE name = $1",
        CURSOR
    )
    .fetch_optional(db)
    .await?;
    Ok(match row {
        None => SyncStatus::default(),
        Some(r) => SyncStatus {
            last_run: r.last_run,
            last_error: r.last_error,
            last_summary: r.last_summary.and_then(|v| serde_json::from_value(v).ok()),
        },
    })
}

/// "Sync now" / "Full resync" from the admin page: queue a run for the worker.
pub async fn request_sync(core: &Core, ctx: &Ctx, full: bool) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    graph(core)?;
    require_enabled(core)?;
    let mut tx = core.db.begin().await?;
    jobs::enqueue(
        &mut *tx,
        NewJob::new(SYNC_JOB, json!({ "full": full }))
            .idempotency_key(format!("entra_sync:manual:{}", Uuid::new_v4()))
            .max_attempts(3),
    )
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "directory.sync_requested",
                ctx.actor.clone(),
                "directory",
            )
            .context("full", full),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Queue today's run once the local time is past `[entra_sync] run_at`.
/// The idempotency key makes it once per local date, across workers.
pub async fn schedule_daily(core: &Core, now: DateTime<Utc>) -> CoreResult<bool> {
    let cfg = &core.config().entra_sync;
    let Some(at) = cfg.run_at_time() else {
        return Ok(false);
    };
    if !cfg.enabled {
        return Ok(false);
    }
    let local = now.with_timezone(&core.config().timezone());
    if local.time() < at {
        return Ok(false);
    }
    Ok(jobs::enqueue(
        &core.db,
        NewJob::new(SYNC_JOB, json!({ "full": false }))
            .idempotency_key(format!("entra_sync:daily:{}", local.date_naive()))
            .max_attempts(3),
    )
    .await?
    .is_some())
}

/// Worker handler for [`SYNC_JOB`]. A run already in progress is not an
/// error, nor is a run queued before the sync was turned off (not retried).
pub async fn run_job(core: &Core, payload: &Value) -> Result<(), String> {
    if !core.config().entra_sync.enabled {
        tracing::info!("Entra sync is turned off; the queued run was skipped");
        return Ok(());
    }
    let full = payload
        .get("full")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ctx = Ctx::system("entra-sync");
    let result = if full {
        run_full(core, &ctx).await
    } else {
        run(core, &ctx).await
    };
    match result {
        Ok(r) => {
            tracing::info!(
                created = r.created,
                updated = r.updated,
                departed = r.departed,
                reactivated = r.reactivated.len(),
                held_back = r.held_back.len(),
                unmapped = r.unmapped,
                problems = r.errors.len(),
                "Entra directory synchronized"
            );
            Ok(())
        }
        Err(CoreError::Conflict(m)) => {
            tracing::info!("{m}; this run was skipped");
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_summary_without_newer_fields_still_reads() {
        let r: SyncReport = serde_json::from_value(json!({ "created": 2, "departed": 1 })).unwrap();
        assert_eq!((r.created, r.departed, r.updated), (2, 1, 0));
        assert!(r.errors.is_empty() && r.changed_departments.is_empty());
        assert!(r.reactivated.is_empty() && r.held_back.is_empty());
    }

    #[test]
    fn the_cursor_key_covers_guests() {
        assert_eq!(cursor_key("id,department", false, None), "id,department");
        assert_eq!(
            cursor_key("id,department", true, None),
            "id,department;guests"
        );
        assert_eq!(
            cursor_key("id,department", false, Some("g")),
            "id,department;group=g"
        );
        assert_eq!(
            cursor_key("id,department", true, Some("g")),
            "id,department;guests;group=g"
        );
    }
}
