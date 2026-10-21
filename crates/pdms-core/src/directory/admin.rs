// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use pdms_audit::{AuditEvent, AuditValue, Category};
use pdms_authz::{Capability, role_keys};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::{password, session};
use crate::context::Ctx;
use crate::crypto::Keys;
use crate::error::{CoreError, CoreResult};
use crate::model::{MembershipSource, UserStatus};

fn event(ctx: &Ctx, category: Category, action: &str, object_type: &str) -> AuditEvent {
    AuditEvent::new(category, action, ctx.actor.clone(), object_type)
}

fn clean_name(s: &str, what: &str) -> CoreResult<String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 200 {
        return Err(CoreError::validation(format!(
            "{what} must be 1–200 characters"
        )));
    }
    Ok(s.to_owned())
}

fn clean_email(s: &str) -> CoreResult<String> {
    let s = s.trim();
    let valid = s.len() <= 320
        && s.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && d.contains('.') && !s.contains(char::is_whitespace)
        });
    if !valid {
        return Err(CoreError::validation("enter a valid email address"));
    }
    Ok(s.to_owned())
}

async fn user_label(conn: &mut PgConnection, id: Uuid) -> CoreResult<String> {
    sqlx::query_scalar!("SELECT display_name FROM users WHERE id = $1", id)
        .fetch_optional(conn)
        .await?
        .ok_or(CoreError::NotFound)
}

async fn department_label(conn: &mut PgConnection, id: Uuid) -> CoreResult<String> {
    sqlx::query_scalar!("SELECT name FROM departments WHERE id = $1", id)
        .fetch_optional(conn)
        .await?
        .ok_or(CoreError::NotFound)
}

// ---------------------------------------------------------------------------
// Departments
// ---------------------------------------------------------------------------

/// Create a department and its default grant: members may view its documents.
pub async fn create_department(db: &PgPool, ctx: &Ctx, code: &str, name: &str) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDirectory)?;
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty()
        || code.len() > 32
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CoreError::validation(
            "code must be 1–32 characters: letters, digits, '-' or '_'",
        ));
    }
    let name = clean_name(name, "name")?;
    let id = Uuid::now_v7();
    let grant_id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO departments (id, code, name) VALUES ($1, $2, $3)",
        id,
        code,
        name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a department with that code already exists"))?;
    sqlx::query!(
        "INSERT INTO acl_entries (id, subject_department_id, scope, scope_department_id, permission, granted_by, reason)
         VALUES ($1, $2, 'department', $2, 'view', $3, 'default: department members may view')",
        grant_id,
        id,
        ctx.user_id(),
    )
    .execute(&mut *tx)
    .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Admin, "department.created", "department")
                .object(id, Some(&name))
                .department(Some(id))
                .change("code", AuditValue::Null, code.as_str())
                .change("name", AuditValue::Null, name.as_str()),
            event(ctx, Category::Permission, "acl.granted", "acl_entry")
                .object(grant_id, Some("Department members: view"))
                .department(Some(id))
                .change("subject", AuditValue::Null, format!("department:{id}"))
                .change("scope", AuditValue::Null, format!("department:{id}"))
                .change("permission", AuditValue::Null, "view"),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update_department(
    db: &PgPool,
    ctx: &Ctx,
    id: Uuid,
    name: &str,
    is_active: bool,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let name = clean_name(name, "name")?;
    let mut tx = db.begin().await?;
    let before = sqlx::query!(
        "SELECT name, is_active FROM departments WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    sqlx::query!(
        "UPDATE departments SET name = $2, is_active = $3, updated_at = now() WHERE id = $1",
        id,
        name,
        is_active
    )
    .execute(&mut *tx)
    .await?;
    let e = event(ctx, Category::Admin, "department.updated", "department")
        .object(id, Some(&name))
        .department(Some(id))
        .change("name", before.name, name.as_str())
        .change("is_active", before.is_active, is_active);
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// Create an account by hand (for example a local administrator). Single
/// sign-on never links to it: SSO accounts are matched by issuer and Entra
/// object ID only, never by email (D-086).
pub async fn create_user(
    db: &PgPool,
    ctx: &Ctx,
    display_name: &str,
    email: &str,
    department: Option<Uuid>,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDirectory)?;
    let display_name = clean_name(display_name, "display name")?;
    let email = clean_email(email)?;
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO users (id, display_name, email) VALUES ($1, $2, $3)",
        id,
        display_name,
        email
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a user with that email already exists"))?;
    let mut events = vec![
        event(ctx, Category::Admin, "user.created", "user")
            .object(id, Some(&display_name))
            .change("display_name", AuditValue::Null, display_name.as_str())
            .change("email", AuditValue::Null, email.as_str()),
    ];
    if let Some(dept) = department {
        events.push(insert_membership(&mut tx, ctx, id, &display_name, dept, true).await?);
    }
    pdms_audit::append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update_user(
    db: &PgPool,
    ctx: &Ctx,
    id: Uuid,
    display_name: &str,
    email: &str,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let display_name = clean_name(display_name, "display name")?;
    let email = clean_email(email)?;
    let mut tx = db.begin().await?;
    let before = sqlx::query!(
        "SELECT display_name, email FROM users WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    sqlx::query!(
        "UPDATE users SET display_name = $2, email = $3, updated_at = now() WHERE id = $1",
        id,
        display_name,
        email
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a user with that email already exists"))?;
    let e = event(ctx, Category::Admin, "user.updated", "user")
        .object(id, Some(&display_name))
        .change("display_name", before.display_name, display_name.as_str())
        .change("email", before.email, email.as_str());
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn clean_username(s: &str) -> CoreResult<Option<String>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let ok = (2..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok {
        return Err(CoreError::validation(
            "Usernames are 2–64 letters, numbers, dots, dashes, or underscores.",
        ));
    }
    Ok(Some(s.to_owned()))
}

/// Validate `username` as `set_username` would, and check it is free (ignoring case),
/// without changing anything. `except_user` is the account that may already hold it.
pub async fn check_username(
    db: &PgPool,
    username: &str,
    except_user: Option<Uuid>,
) -> CoreResult<Option<String>> {
    let Some(name) = clean_username(username)? else {
        return Ok(None);
    };
    let taken = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users
                           WHERE lower(username) = lower($1)
                             AND ($2::uuid IS NULL OR id <> $2)) AS "taken!""#,
        name,
        except_user
    )
    .fetch_one(db)
    .await?;
    if taken {
        return Err(CoreError::Conflict(
            "That username is already in use.".into(),
        ));
    }
    Ok(Some(name))
}

/// Set or clear the plain username a local (break-glass) account can sign in with.
/// An empty value clears it. Unique ignoring case.
pub async fn set_username(
    db: &PgPool,
    ctx: &Ctx,
    user_id: Uuid,
    username: Option<&str>,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let username = username.map(clean_username).transpose()?.flatten();
    let mut tx = db.begin().await?;
    let before = sqlx::query!(
        r#"SELECT u.username, u.display_name, (c.user_id IS NOT NULL) AS "has_local!"
             FROM users u LEFT JOIN local_credentials c ON c.user_id = u.id
            WHERE u.id = $1 FOR UPDATE OF u"#,
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    if !before.has_local && username.is_some() {
        return Err(CoreError::validation(
            "Only accounts with local sign-in can have a username.",
        ));
    }
    if before.username == username {
        return Ok(());
    }
    sqlx::query!(
        "UPDATE users SET username = $2, updated_at = now() WHERE id = $1",
        user_id,
        username
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "That username is already in use."))?;
    let e = event(ctx, Category::Admin, "user.username_set", "user")
        .object(user_id, Some(&before.display_name))
        .change(
            "username",
            before.username.as_deref().unwrap_or(""),
            username.as_deref().unwrap_or(""),
        );
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

/// Activate, disable, or mark departed. Leaving `active` revokes all sessions immediately.
pub async fn set_user_status(
    db: &PgPool,
    ctx: &Ctx,
    id: Uuid,
    status: UserStatus,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    if ctx.user_id() == Some(id) && status != UserStatus::Active {
        return Err(CoreError::Conflict(
            "you cannot deactivate your own account".into(),
        ));
    }
    let mut tx = db.begin().await?;
    let before = sqlx::query!(
        r#"SELECT display_name, status AS "status: UserStatus" FROM users WHERE id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    if before.status == status {
        return Ok(());
    }
    if status != UserStatus::Active && is_org_admin(&mut tx, id).await? {
        ensure_other_admin_remains(&mut tx, Some(id), None).await?;
    }
    sqlx::query!(
        "UPDATE users SET status = $2, updated_at = now(),
                deactivated_at = CASE WHEN $2 = 'active'::user_status THEN NULL ELSE now() END
          WHERE id = $1",
        id,
        status as UserStatus
    )
    .execute(&mut *tx)
    .await?;
    let revoked = if status == UserStatus::Active {
        0
    } else {
        session::revoke_all_for_user(&mut tx, id).await?
    };
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Admin, "user.status_changed", "user")
                .object(id, Some(&before.display_name))
                .change("status", before.status.as_str(), status.as_str())
                .context(
                    "sessions_revoked",
                    i64::try_from(revoked).unwrap_or(i64::MAX),
                ),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Set or replace a break-glass password and optional TOTP secret. Revokes sessions.
pub async fn set_local_credentials(
    db: &PgPool,
    keys: &Keys,
    ctx: &Ctx,
    user_id: Uuid,
    new_password: &str,
    totp_secret: Option<&[u8]>,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    password::validate(new_password)?;
    let hash = password::hash(new_password.to_owned()).await?;
    let totp_enc = totp_secret
        .map(|s| keys.encrypt_totp_secret(user_id, s))
        .transpose()
        .map_err(CoreError::internal)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    sqlx::query!(
        "INSERT INTO local_credentials (user_id, password_hash, totp_secret_enc)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE
            SET password_hash = EXCLUDED.password_hash,
                totp_secret_enc = EXCLUDED.totp_secret_enc,
                totp_last_step = NULL,
                failed_attempts = 0,
                locked_until = NULL,
                password_changed_at = now()",
        user_id,
        hash,
        totp_enc,
    )
    .execute(&mut *tx)
    .await?;
    let revoked = session::revoke_all_for_user(&mut tx, user_id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Admin, "user.local_credentials_set", "user")
                .object(user_id, Some(&label))
                .context("totp_enrolled", totp_secret.is_some())
                .context(
                    "sessions_revoked",
                    i64::try_from(revoked).unwrap_or(i64::MAX),
                ),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Remove break-glass credentials (user can then sign in only with OIDC).
pub async fn remove_local_credentials(db: &PgPool, ctx: &Ctx, user_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    let old_username = sqlx::query_scalar!(
        "SELECT username FROM users WHERE id = $1 FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let r = sqlx::query!("DELETE FROM local_credentials WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    if let Some(old) = old_username {
        // A username only belongs to an account that can sign in locally.
        sqlx::query!(
            "UPDATE users SET username = NULL, updated_at = now() WHERE id = $1",
            user_id
        )
        .execute(&mut *tx)
        .await?;
        pdms_audit::append(
            &mut tx,
            vec![
                event(ctx, Category::Admin, "user.username_set", "user")
                    .object(user_id, Some(&label))
                    .change("username", old.as_str(), ""),
            ],
        )
        .await?;
    }
    if r.rows_affected() > 0 {
        pdms_audit::append(
            &mut tx,
            vec![
                event(
                    ctx,
                    Category::Admin,
                    "user.local_credentials_removed",
                    "user",
                )
                .object(user_id, Some(&label)),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Department membership (affects access, so audited as permission changes)
// ---------------------------------------------------------------------------

async fn insert_membership(
    conn: &mut PgConnection,
    ctx: &Ctx,
    user_id: Uuid,
    user_name: &str,
    department_id: Uuid,
    is_primary: bool,
) -> CoreResult<AuditEvent> {
    let dept = department_label(conn, department_id).await?;
    if is_primary {
        sqlx::query!(
            "UPDATE department_members SET is_primary = false WHERE user_id = $1 AND is_primary",
            user_id
        )
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query!(
        "INSERT INTO department_members (department_id, user_id, is_primary, source)
         VALUES ($1, $2, $3, 'manual')
         ON CONFLICT (department_id, user_id) DO UPDATE SET is_primary = EXCLUDED.is_primary",
        department_id,
        user_id,
        is_primary
    )
    .execute(&mut *conn)
    .await?;
    Ok(
        event(ctx, Category::Permission, "department.member_added", "user")
            .object(user_id, Some(user_name))
            .department(Some(department_id))
            .context("department", dept)
            .context("is_primary", is_primary),
    )
}

pub async fn add_department_member(
    db: &PgPool,
    ctx: &Ctx,
    user_id: Uuid,
    department_id: Uuid,
    is_primary: bool,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    let e = insert_membership(&mut tx, ctx, user_id, &label, department_id, is_primary).await?;
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_department_member(
    db: &PgPool,
    ctx: &Ctx,
    user_id: Uuid,
    department_id: Uuid,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    let dept = department_label(&mut tx, department_id).await?;
    let removed = sqlx::query!(
        r#"DELETE FROM department_members WHERE department_id = $1 AND user_id = $2
           RETURNING source AS "source: MembershipSource""#,
        department_id,
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(
                ctx,
                Category::Permission,
                "department.member_removed",
                "user",
            )
            .object(user_id, Some(&label))
            .department(Some(department_id))
            .context("department", dept)
            .context("source", removed.source.as_str()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

pub async fn create_group(
    db: &PgPool,
    ctx: &Ctx,
    name: &str,
    description: &str,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDirectory)?;
    let name = clean_name(name, "name")?;
    let description = description.trim().chars().take(2000).collect::<String>();
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO groups (id, name, description) VALUES ($1, $2, $3)",
        id,
        name,
        description
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "a group with that name already exists"))?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Admin, "group.created", "group")
                .object(id, Some(&name))
                .change("name", AuditValue::Null, name.as_str())
                .change("description", AuditValue::Null, description.as_str()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn add_group_member(
    db: &PgPool,
    ctx: &Ctx,
    group_id: Uuid,
    user_id: Uuid,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    let group = sqlx::query_scalar!("SELECT name FROM groups WHERE id = $1", group_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CoreError::NotFound)?;
    let r = sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, source) VALUES ($1, $2, 'manual')
         ON CONFLICT DO NOTHING",
        group_id,
        user_id
    )
    .execute(&mut *tx)
    .await?;
    if r.rows_affected() > 0 {
        pdms_audit::append(
            &mut tx,
            vec![
                event(ctx, Category::Permission, "group.member_added", "group")
                    .object(group_id, Some(&group))
                    .context("user_id", user_id)
                    .context("user", label),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn remove_group_member(
    db: &PgPool,
    ctx: &Ctx,
    group_id: Uuid,
    user_id: Uuid,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    let group = sqlx::query_scalar!("SELECT name FROM groups WHERE id = $1", group_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CoreError::NotFound)?;
    let r = sqlx::query!(
        "DELETE FROM group_members WHERE group_id = $1 AND user_id = $2",
        group_id,
        user_id
    )
    .execute(&mut *tx)
    .await?;
    if r.rows_affected() == 0 {
        return Err(CoreError::NotFound);
    }
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Permission, "group.member_removed", "group")
                .object(group_id, Some(&group))
                .context("user_id", user_id)
                .context("user", label),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

async fn is_org_admin(conn: &mut PgConnection, user_id: Uuid) -> CoreResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM role_assignments ra JOIN roles r ON r.id = ra.role_id
                           WHERE ra.user_id = $1 AND r.key = $2 AND ra.department_id IS NULL) AS "e!""#,
        user_id,
        role_keys::ADMIN
    )
    .fetch_one(conn)
    .await?)
}

/// Fail if no active org-wide administrator would remain after removing
/// `excluded_user` or `excluded_assignment`.
async fn ensure_other_admin_remains(
    conn: &mut PgConnection,
    excluded_user: Option<Uuid>,
    excluded_assignment: Option<Uuid>,
) -> CoreResult<()> {
    let remaining = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM role_assignments ra
             JOIN roles r ON r.id = ra.role_id
             JOIN users u ON u.id = ra.user_id
            WHERE r.key = $1 AND ra.department_id IS NULL AND u.status = 'active'
              AND ra.user_id IS DISTINCT FROM $2 AND ra.id IS DISTINCT FROM $3"#,
        role_keys::ADMIN,
        excluded_user,
        excluded_assignment,
    )
    .fetch_one(conn)
    .await?;
    if remaining == 0 {
        return Err(CoreError::Conflict(
            "at least one active administrator must remain".into(),
        ));
    }
    Ok(())
}

pub async fn assign_role(
    db: &PgPool,
    ctx: &Ctx,
    user_id: Uuid,
    role_id: Uuid,
    department_id: Option<Uuid>,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let label = user_label(&mut tx, user_id).await?;
    let role = sqlx::query_scalar!("SELECT name FROM roles WHERE id = $1", role_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CoreError::NotFound)?;
    let dept = match department_id {
        Some(d) => Some(department_label(&mut tx, d).await?),
        None => None,
    };
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO role_assignments (id, role_id, user_id, department_id, granted_by)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        role_id,
        user_id,
        department_id,
        ctx.user_id(),
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| CoreError::conflict_on_unique(e, "the user already holds that role"))?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Permission, "role.assigned", "user")
                .object(user_id, Some(&label))
                .department(department_id)
                .context("role_id", role_id)
                .context("role", role)
                .context("department", dept),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn revoke_role(db: &PgPool, ctx: &Ctx, assignment_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let a = sqlx::query!(
        r#"SELECT ra.user_id, ra.role_id, ra.department_id, r.key, r.name AS role_name,
                  u.display_name, d.name AS "department_name?"
             FROM role_assignments ra
             JOIN roles r ON r.id = ra.role_id
             JOIN users u ON u.id = ra.user_id
             LEFT JOIN departments d ON d.id = ra.department_id
            WHERE ra.id = $1 FOR UPDATE OF ra"#,
        assignment_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    if a.key == role_keys::ADMIN && a.department_id.is_none() {
        ensure_other_admin_remains(&mut tx, None, Some(assignment_id)).await?;
    }
    sqlx::query!("DELETE FROM role_assignments WHERE id = $1", assignment_id)
        .execute(&mut *tx)
        .await?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(ctx, Category::Permission, "role.revoked", "user")
                .object(a.user_id, Some(&a.display_name))
                .department(a.department_id)
                .context("role_id", a.role_id)
                .context("role", a.role_name)
                .context("department", a.department_name),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Rename a role's display name (keys are fixed).
pub async fn update_role(
    db: &PgPool,
    ctx: &Ctx,
    role_id: Uuid,
    name: &str,
    description: &str,
) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let name = clean_name(name, "name")?;
    let description = description.trim().chars().take(2000).collect::<String>();
    let mut tx = db.begin().await?;
    let before = sqlx::query!(
        "SELECT key, name, description FROM roles WHERE id = $1 FOR UPDATE",
        role_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    sqlx::query!(
        "UPDATE roles SET name = $2, description = $3 WHERE id = $1",
        role_id,
        name,
        description
    )
    .execute(&mut *tx)
    .await?;
    let e = event(ctx, Category::Admin, "role.updated", "role")
        .object(role_id, Some(&name))
        .context("key", before.key)
        .change("name", before.name, name.as_str())
        .change("description", before.description, description.as_str());
    if e.changes.is_some() {
        pdms_audit::append(&mut tx, vec![e]).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// IdP group mappings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum MappingTarget {
    Department(Uuid),
    Group(Uuid),
    Role {
        role_id: Uuid,
        department_id: Option<Uuid>,
    },
}

pub async fn create_idp_mapping(
    db: &PgPool,
    ctx: &Ctx,
    idp_group_id: &str,
    description: &str,
    target: MappingTarget,
) -> CoreResult<Uuid> {
    ctx.require(Capability::ManageDirectory)?;
    let idp_group_id = idp_group_id.trim();
    if idp_group_id.is_empty() || idp_group_id.len() > 200 {
        return Err(CoreError::validation(
            "IdP group ID must be 1–200 characters",
        ));
    }
    let description = description.trim().chars().take(500).collect::<String>();
    let (dept, group, role, role_dept) = match target {
        MappingTarget::Department(d) => (Some(d), None, None, None),
        MappingTarget::Group(g) => (None, Some(g), None, None),
        MappingTarget::Role {
            role_id,
            department_id,
        } => (None, None, Some(role_id), department_id),
    };
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    sqlx::query!(
        "INSERT INTO idp_group_mappings (id, idp_group_id, description, department_id, group_id, role_id, role_department_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        idp_group_id,
        description,
        dept,
        group,
        role,
        role_dept
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => CoreError::NotFound,
        _ => CoreError::conflict_on_unique(e, "that mapping already exists"),
    })?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(
                ctx,
                Category::Permission,
                "idp_mapping.created",
                "idp_group_mapping",
            )
            .object(id, Some(idp_group_id))
            .change("idp_group_id", AuditValue::Null, idp_group_id)
            .change("department_id", AuditValue::Null, dept)
            .change("group_id", AuditValue::Null, group)
            .change("role_id", AuditValue::Null, role)
            .change("role_department_id", AuditValue::Null, role_dept),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn delete_idp_mapping(db: &PgPool, ctx: &Ctx, id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageDirectory)?;
    let mut tx = db.begin().await?;
    let m = sqlx::query!(
        "DELETE FROM idp_group_mappings WHERE id = $1
         RETURNING idp_group_id, department_id, group_id, role_id, role_department_id",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![
            event(
                ctx,
                Category::Permission,
                "idp_mapping.deleted",
                "idp_group_mapping",
            )
            .object(id, Some(&m.idp_group_id))
            .change("idp_group_id", m.idp_group_id.as_str(), AuditValue::Null)
            .change("department_id", m.department_id, AuditValue::Null)
            .change("group_id", m.group_id, AuditValue::Null)
            .change("role_id", m.role_id, AuditValue::Null)
            .change("role_department_id", m.role_department_id, AuditValue::Null),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
