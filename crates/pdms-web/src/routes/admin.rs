// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Directory administration. The router applies a `ManageDirectory` capability
//! check to every route here; core services check again on every mutation.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::{Html, Response};
use chrono::NaiveDate;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::directory::{
    self, Department, Group, GroupMember, IdpMapping, MappingTarget, Role, UserDetail, UserSummary,
};
use pdms_core::model::UserStatus;
use pdms_core::{CoreError, acl};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, fmt_time_opt};

fn uuid_opt(s: &str) -> AppResult<Option<Uuid>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse()
        .map(Some)
        .map_err(|_| AppError::BadRequest("invalid identifier".into()))
}

fn tagged(s: &str) -> AppResult<(&str, Uuid)> {
    let (kind, id) = s
        .split_once(':')
        .ok_or_else(|| AppError::BadRequest("invalid selection".into()))?;
    Ok((
        kind,
        id.parse()
            .map_err(|_| AppError::BadRequest("invalid identifier".into()))?,
    ))
}

fn checkbox(v: &Option<String>) -> bool {
    v.as_deref() == Some("true")
}

/// Only accept local admin paths as "back" targets.
fn back_to(s: &str, default: &str) -> String {
    if s.starts_with("/admin/") && !s.contains("//") && !s.contains('\\') {
        s.to_owned()
    } else {
        default.to_owned()
    }
}

fn methods(u: &UserSummary) -> String {
    match (u.oidc_linked, u.has_local_login) {
        (true, true) => "Single sign-on, emergency local".into(),
        (true, false) => "Single sign-on".into(),
        (false, true) => "Emergency local only".into(),
        (false, false) => "Single sign-on (not yet signed in)".into(),
    }
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

struct UserRow {
    id: Uuid,
    name: String,
    email: String,
    status: &'static str,
    departments: String,
    methods: String,
    last_login: String,
}

#[derive(Template)]
#[template(path = "admin/users.html")]
struct UsersPage {
    l: Layout,
    q: String,
    users: Vec<UserRow>,
    departments: Vec<Department>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

pub async fn users(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Query(q): Query<SearchQuery>,
) -> AppResult<Html<String>> {
    let rows = directory::list_users(&state.core.db, &q.q, 500).await?;
    let page = UsersPage {
        l: Layout::new(&state, &user, flash, "Users", "admin"),
        users: rows
            .into_iter()
            .map(|u| UserRow {
                methods: methods(&u),
                last_login: fmt_time_opt(state.tz, u.last_login_at),
                departments: u.departments.join(", "),
                status: u.status.as_str(),
                id: u.id,
                name: u.display_name,
                email: u.email,
            })
            .collect(),
        departments: directory::list_departments(&state.core.db).await?,
        q: q.q,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct NewUserForm {
    display_name: String,
    email: String,
    #[serde(default)]
    department_id: String,
}

pub async fn create_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<NewUserForm>,
) -> AppResult<Response> {
    let dept = uuid_opt(&f.department_id)?;
    match directory::create_user(&state.core.db, &user.ctx, &f.display_name, &f.email, dept).await {
        Ok(id) => done(&state, &format!("/admin/users/{id}"), Ok(()), "User added."),
        Err(e) => done::<()>(&state, "/admin/users", Err(e), ""),
    }
}

#[derive(Template)]
#[template(path = "admin/user.html")]
struct UserPage {
    l: Layout,
    u: UserDetail,
    methods: String,
    last_login: String,
    departments: Vec<Department>,
    roles: Vec<Role>,
}

pub async fn user_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    let u = directory::get_user(&state.core.db, id).await?;
    let page = UserPage {
        l: Layout::new(&state, &user, flash, &u.user.display_name, "admin"),
        methods: methods(&u.user),
        last_login: fmt_time_opt(state.tz, u.user.last_login_at),
        departments: directory::list_departments(&state.core.db).await?,
        roles: directory::list_roles(&state.core.db).await?,
        u,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct UserForm {
    display_name: String,
    email: String,
}

pub async fn update_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<UserForm>,
) -> AppResult<Response> {
    let r = directory::update_user(&state.core.db, &user.ctx, id, &f.display_name, &f.email).await;
    done(&state, &format!("/admin/users/{id}"), r, "Profile saved.")
}

#[derive(Deserialize)]
pub struct UsernameForm {
    #[serde(default)]
    username: String,
}

pub async fn set_username(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<UsernameForm>,
) -> AppResult<Response> {
    let r = directory::set_username(&state.core.db, &user.ctx, id, Some(&f.username)).await;
    done(&state, &format!("/admin/users/{id}"), r, "Username saved.")
}

#[derive(Deserialize)]
pub struct StatusForm {
    status: String,
}

pub async fn set_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<StatusForm>,
) -> AppResult<Response> {
    let status = UserStatus::parse(&f.status)
        .ok_or_else(|| AppError::BadRequest("invalid status".into()))?;
    let r = directory::set_user_status(&state.core.db, &user.ctx, id, status).await;
    done(&state, &format!("/admin/users/{id}"), r, "Status changed.")
}

#[derive(Deserialize)]
pub struct MembershipForm {
    department_id: Uuid,
    is_primary: Option<String>,
}

pub async fn add_membership(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<MembershipForm>,
) -> AppResult<Response> {
    let r = directory::add_department_member(
        &state.core.db,
        &user.ctx,
        id,
        f.department_id,
        checkbox(&f.is_primary),
    )
    .await;
    done(
        &state,
        &format!("/admin/users/{id}"),
        r,
        "Department added.",
    )
}

pub async fn remove_membership(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, dept)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = directory::remove_department_member(&state.core.db, &user.ctx, id, dept).await;
    done(
        &state,
        &format!("/admin/users/{id}"),
        r,
        "Department removed.",
    )
}

#[derive(Deserialize)]
pub struct RoleForm {
    role_id: Uuid,
    #[serde(default)]
    department_id: String,
}

pub async fn assign_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<RoleForm>,
) -> AppResult<Response> {
    let dept = uuid_opt(&f.department_id)?;
    let r = directory::assign_role(&state.core.db, &user.ctx, id, f.role_id, dept).await;
    done(&state, &format!("/admin/users/{id}"), r, "Role assigned.")
}

#[derive(Deserialize)]
pub struct BackForm {
    #[serde(default)]
    back: String,
}

pub async fn revoke_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(assignment): Path<Uuid>,
    Form(f): Form<BackForm>,
) -> AppResult<Response> {
    let r = directory::revoke_role(&state.core.db, &user.ctx, assignment).await;
    done(
        &state,
        &back_to(&f.back, "/admin/users"),
        r,
        "Role revoked.",
    )
}

pub async fn remove_local_credentials(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = directory::remove_local_credentials(&state.core.db, &user.ctx, id).await;
    done(
        &state,
        &format!("/admin/users/{id}"),
        r,
        "Local credentials removed.",
    )
}

// ---------------------------------------------------------------------------
// Departments
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/departments.html")]
struct DepartmentsPage {
    l: Layout,
    departments: Vec<Department>,
}

pub async fn departments(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let page = DepartmentsPage {
        l: Layout::new(&state, &user, flash, "Departments", "admin"),
        departments: directory::list_departments(&state.core.db).await?,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct NewDepartmentForm {
    code: String,
    name: String,
}

pub async fn create_department(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<NewDepartmentForm>,
) -> AppResult<Response> {
    match directory::create_department(&state.core.db, &user.ctx, &f.code, &f.name).await {
        Ok(id) => done(
            &state,
            &format!("/admin/departments/{id}"),
            Ok(()),
            "Department added.",
        ),
        Err(e) => done::<()>(&state, "/admin/departments", Err(e), ""),
    }
}

struct GrantRow {
    id: Uuid,
    subject: String,
    scope: String,
    permission: String,
    expires: String,
    granted: String,
    reason: String,
}

fn grant_rows(state: &AppState, grants: Vec<acl::GrantView>) -> Vec<GrantRow> {
    grants
        .into_iter()
        .map(|g| GrantRow {
            id: g.id,
            subject: format!("{} ({})", g.subject_label, g.subject_kind),
            scope: g.scope_label,
            permission: g.permission,
            expires: fmt_time_opt(state.tz, g.expires_at),
            granted: match g.granted_by {
                Some(by) => format!("{} by {by}", fmt_time(state.tz, g.granted_at)),
                None => fmt_time(state.tz, g.granted_at),
            },
            reason: g.reason,
        })
        .collect()
}

#[derive(Template)]
#[template(path = "admin/department.html")]
struct DepartmentPage {
    l: Layout,
    d: Department,
    members: Vec<GroupMember>,
    grants: Vec<GrantRow>,
    back: String,
    scope_department: Option<Uuid>,
    departments: Vec<Department>,
    roles: Vec<Role>,
    groups: Vec<Group>,
    users: Vec<UserSummary>,
    permissions: Vec<&'static str>,
}

async fn grant_form_options(
    state: &AppState,
) -> AppResult<(Vec<Department>, Vec<Role>, Vec<Group>, Vec<UserSummary>)> {
    let db = &state.core.db;
    Ok((
        directory::list_departments(db).await?,
        directory::list_roles(db).await?,
        directory::list_groups(db).await?,
        directory::list_users(db, "", 2000).await?,
    ))
}

fn permission_names() -> Vec<&'static str> {
    Permission::ALL.iter().map(|p| p.as_str()).collect()
}

pub async fn department_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    let d = directory::get_department(&state.core.db, id).await?;
    let (departments, roles, groups, users) = grant_form_options(&state).await?;
    let page = DepartmentPage {
        l: Layout::new(&state, &user, flash, &d.name, "admin"),
        members: directory::department_members(&state.core.db, id).await?,
        grants: grant_rows(&state, acl::list_grants(&state.core.db, Some(id)).await?),
        back: format!("/admin/departments/{id}"),
        scope_department: Some(id),
        departments,
        roles,
        groups,
        users,
        permissions: permission_names(),
        d,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct DepartmentForm {
    name: String,
    is_active: Option<String>,
}

pub async fn update_department(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<DepartmentForm>,
) -> AppResult<Response> {
    let r = directory::update_department(
        &state.core.db,
        &user.ctx,
        id,
        &f.name,
        checkbox(&f.is_active),
    )
    .await;
    done(
        &state,
        &format!("/admin/departments/{id}"),
        r,
        "Department saved.",
    )
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/grants.html")]
struct GrantsPage {
    l: Layout,
    grants: Vec<GrantRow>,
    back: String,
    scope_department: Option<Uuid>,
    departments: Vec<Department>,
    roles: Vec<Role>,
    groups: Vec<Group>,
    users: Vec<UserSummary>,
    permissions: Vec<&'static str>,
}

pub async fn grants(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let (departments, roles, groups, users) = grant_form_options(&state).await?;
    let page = GrantsPage {
        l: Layout::new(&state, &user, flash, "Access grants", "admin"),
        grants: grant_rows(&state, acl::list_grants(&state.core.db, None).await?),
        back: "/admin/grants".into(),
        scope_department: None,
        departments,
        roles,
        groups,
        users,
        permissions: permission_names(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct GrantForm {
    subject: String,
    scope: String,
    permission: String,
    #[serde(default)]
    expires_on: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    back: String,
}

pub async fn create_grant(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<GrantForm>,
) -> AppResult<Response> {
    let back = back_to(&f.back, "/admin/grants");
    let subject = match tagged(&f.subject)? {
        ("user", id) => Subject::User(id),
        ("group", id) => Subject::Group(id),
        ("department", id) => Subject::Department(id),
        ("role", id) => Subject::Role(id),
        _ => return Err(AppError::BadRequest("invalid subject".into())),
    };
    let scope = if f.scope == "global" {
        Scope::Global
    } else {
        match tagged(&f.scope)? {
            ("department", id) => Scope::Department(id),
            _ => return Err(AppError::BadRequest("invalid scope".into())),
        }
    };
    let permission: Permission = f
        .permission
        .parse()
        .map_err(|_| AppError::BadRequest("invalid permission".into()))?;
    let expires_at = if f.expires_on.trim().is_empty() {
        None
    } else {
        let date = NaiveDate::parse_from_str(f.expires_on.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::BadRequest("invalid date".into()))?;
        // Expire at the end of that day in the organization's time zone.
        let local = date
            .succ_opt()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .and_then(|dt| dt.and_local_timezone(state.tz).earliest())
            .ok_or_else(|| AppError::BadRequest("invalid date".into()))?;
        Some(local.with_timezone(&chrono::Utc))
    };
    if f.reason.trim().is_empty() {
        return done::<()>(
            &state,
            &back,
            Err(CoreError::validation("enter a reason for the grant")),
            "",
        );
    }
    let r = acl::grant(
        &state.core.db,
        &user.ctx,
        subject,
        scope,
        permission,
        expires_at,
        &f.reason,
    )
    .await;
    done(&state, &back, r, "Access granted.")
}

pub async fn revoke_grant(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<BackForm>,
) -> AppResult<Response> {
    let r = acl::revoke(&state.core.db, &user.ctx, id).await;
    done(
        &state,
        &back_to(&f.back, "/admin/grants"),
        r,
        "Access revoked.",
    )
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/groups.html")]
struct GroupsPage {
    l: Layout,
    groups: Vec<Group>,
}

pub async fn groups(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let page = GroupsPage {
        l: Layout::new(&state, &user, flash, "Groups", "admin"),
        groups: directory::list_groups(&state.core.db).await?,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct NewGroupForm {
    name: String,
    #[serde(default)]
    description: String,
}

pub async fn create_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<NewGroupForm>,
) -> AppResult<Response> {
    match directory::create_group(&state.core.db, &user.ctx, &f.name, &f.description).await {
        Ok(id) => done(
            &state,
            &format!("/admin/groups/{id}"),
            Ok(()),
            "Group added.",
        ),
        Err(e) => done::<()>(&state, "/admin/groups", Err(e), ""),
    }
}

#[derive(Template)]
#[template(path = "admin/group.html")]
struct GroupPage {
    l: Layout,
    g: Group,
    members: Vec<GroupMember>,
    users: Vec<UserSummary>,
}

pub async fn group_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
    Path(id): Path<Uuid>,
) -> AppResult<Html<String>> {
    let (g, members) = directory::get_group(&state.core.db, id).await?;
    let page = GroupPage {
        l: Layout::new(&state, &user, flash, &g.name, "admin"),
        users: directory::list_users(&state.core.db, "", 2000).await?,
        g,
        members,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct MemberForm {
    user_id: Uuid,
}

pub async fn add_group_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<MemberForm>,
) -> AppResult<Response> {
    let r = directory::add_group_member(&state.core.db, &user.ctx, id, f.user_id).await;
    done(&state, &format!("/admin/groups/{id}"), r, "Member added.")
}

pub async fn remove_group_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, member)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let r = directory::remove_group_member(&state.core.db, &user.ctx, id, member).await;
    done(&state, &format!("/admin/groups/{id}"), r, "Member removed.")
}

// ---------------------------------------------------------------------------
// Roles and IdP mappings
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/roles.html")]
struct RolesPage {
    l: Layout,
    roles: Vec<Role>,
}

pub async fn roles(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let page = RolesPage {
        l: Layout::new(&state, &user, flash, "Roles", "admin"),
        roles: directory::list_roles(&state.core.db).await?,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct RoleUpdateForm {
    name: String,
    #[serde(default)]
    description: String,
}

pub async fn update_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<RoleUpdateForm>,
) -> AppResult<Response> {
    let r = directory::update_role(&state.core.db, &user.ctx, id, &f.name, &f.description).await;
    done(&state, "/admin/roles", r, "Role saved.")
}

#[derive(Template)]
#[template(path = "admin/idp.html")]
struct IdpPage {
    l: Layout,
    mappings: Vec<IdpMapping>,
    departments: Vec<Department>,
    groups: Vec<Group>,
    roles: Vec<Role>,
    /// The Entra sync is on, so sign-in does not apply these mappings.
    entra_sync: bool,
}

pub async fn idp(
    State(state): State<AppState>,
    user: CurrentUser,
    flash: FlashMsg,
) -> AppResult<Html<String>> {
    let db = &state.core.db;
    let page = IdpPage {
        l: Layout::new(&state, &user, flash, "Identity provider mappings", "admin"),
        mappings: directory::list_idp_mappings(db).await?,
        departments: directory::list_departments(db).await?,
        groups: directory::list_groups(db).await?,
        roles: directory::list_roles(db).await?,
        entra_sync: state.core.config.entra_sync.enabled,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub struct MappingForm {
    idp_group_id: String,
    #[serde(default)]
    description: String,
    target: String,
    #[serde(default)]
    role_department_id: String,
}

pub async fn create_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(f): Form<MappingForm>,
) -> AppResult<Response> {
    let target = match tagged(&f.target)? {
        ("department", id) => MappingTarget::Department(id),
        ("group", id) => MappingTarget::Group(id),
        ("role", id) => MappingTarget::Role {
            role_id: id,
            department_id: uuid_opt(&f.role_department_id)?,
        },
        _ => return Err(AppError::BadRequest("invalid target".into())),
    };
    let r = directory::create_idp_mapping(
        &state.core.db,
        &user.ctx,
        &f.idp_group_id,
        &f.description,
        target,
    )
    .await;
    done(&state, "/admin/idp", r, "Mapping added.")
}

pub async fn delete_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let r = directory::delete_idp_mapping(&state.core.db, &user.ctx, id).await;
    done(&state, "/admin/idp", r, "Mapping deleted.")
}
