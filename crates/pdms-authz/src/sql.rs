// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! SQL form of [`crate::decide`] for list-style queries.
//!
//! Every query that returns documents (lists, search, calendar, ICS, exports,
//! reports) must embed the predicate built here. Equivalence with the engine is
//! checked by a property test against Postgres in `pdms-core`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::model::{Permission, Policy, Principal};

/// A bind parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlParam {
    Uuid(Uuid),
    UuidArray(Vec<Uuid>),
    /// Bind as `text[]`; the SQL casts it to `permission[]`.
    PermissionArray(Vec<Permission>),
    /// Bind as `uuid[]`; elements may be NULL.
    NullableUuidArray(Vec<Option<Uuid>>),
    Timestamp(DateTime<Utc>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SqlPart {
    Sql(String),
    Param(SqlParam),
}

/// A SQL expression as literal text interleaved with bind parameters, so it
/// can be pushed into any query builder without placeholder renumbering.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SqlFragment {
    parts: Vec<SqlPart>,
}

impl SqlFragment {
    fn sql(&mut self, s: &str) -> &mut Self {
        if let Some(SqlPart::Sql(last)) = self.parts.last_mut() {
            last.push_str(s);
        } else {
            self.parts.push(SqlPart::Sql(s.to_owned()));
        }
        self
    }

    fn param(&mut self, p: SqlParam) -> &mut Self {
        self.parts.push(SqlPart::Param(p));
        self
    }

    pub fn parts(&self) -> &[SqlPart] {
        &self.parts
    }

    /// Render with `$n` placeholders starting at `first`.
    pub fn to_numbered(&self, first: usize) -> (String, Vec<SqlParam>) {
        let mut sql = String::new();
        let mut params = Vec::new();
        for part in &self.parts {
            match part {
                SqlPart::Sql(s) => sql.push_str(s),
                SqlPart::Param(p) => {
                    params.push(p.clone());
                    sql.push_str(&format!("${}", first + params.len() - 1));
                }
            }
        }
        (sql, params)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid SQL alias: {0:?}")]
pub struct InvalidAlias(pub String);

/// Build a boolean SQL expression that is true for rows of `documents` (aliased
/// `doc_alias`) on which `principal` holds `perm`.
///
/// The documents table must provide `id`, `department_id`, `owner_id`, and `visibility`.
pub fn visible_documents_predicate(
    policy: &Policy,
    principal: &Principal,
    perm: Permission,
    now: DateTime<Utc>,
    doc_alias: &str,
) -> Result<SqlFragment, InvalidAlias> {
    let valid_alias = doc_alias.starts_with(|c: char| c.is_ascii_lowercase())
        && doc_alias
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid_alias {
        return Err(InvalidAlias(doc_alias.to_owned()));
    }
    let mut f = SqlFragment::default();
    if !principal.active || perm == Permission::Create {
        f.sql("FALSE");
        return Ok(f);
    }

    let d = doc_alias;
    let user = || SqlParam::Uuid(principal.user_id);
    f.sql("(");
    if perm == Permission::View {
        f.sql(&format!("{d}.visibility = 'county_wide' OR "));
    }
    if policy.owner_permissions.satisfies(perm) {
        f.sql(&format!("{d}.owner_id = ")).param(user()).sql(" OR ");
    }
    f.sql("EXISTS (SELECT 1 FROM acl_entries acl WHERE acl.permission = ANY((")
        .param(SqlParam::PermissionArray(perm.satisfying()))
        .sql(")::text[]::permission[]) AND (acl.expires_at IS NULL OR acl.expires_at > ")
        .param(SqlParam::Timestamp(now))
        .sql(") AND (acl.subject_user_id = ")
        .param(user())
        .sql(" OR acl.subject_group_id = ANY(")
        .param(SqlParam::UuidArray(principal.groups.iter().copied().collect()))
        .sql(") OR acl.subject_department_id = ANY(")
        .param(SqlParam::UuidArray(principal.departments.iter().copied().collect()))
        .sql(") OR EXISTS (SELECT 1 FROM unnest((")
        .param(SqlParam::UuidArray(principal.roles.iter().map(|r| r.role_id).collect()))
        .sql(")::uuid[], (")
        .param(SqlParam::NullableUuidArray(
            principal.roles.iter().map(|r| r.department_id).collect(),
        ))
        .sql(&format!(
            ")::uuid[]) AS held(role_id, department_id) \
WHERE held.role_id = acl.subject_role_id \
AND (held.department_id IS NULL OR held.department_id = {d}.department_id))) \
AND ((acl.scope = 'document' AND acl.scope_document_id = {d}.id) \
OR ({d}.visibility <> 'restricted' \
AND (acl.scope = 'global' OR (acl.scope = 'department' AND acl.scope_department_id = {d}.department_id))))))"
        ));
    Ok(f)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn principal(active: bool) -> Principal {
        Principal {
            user_id: Uuid::nil(),
            active,
            departments: BTreeSet::new(),
            groups: BTreeSet::new(),
            roles: Vec::new(),
        }
    }

    #[test]
    fn rejects_unsafe_alias() {
        let r = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::View,
            Utc::now(),
            "d; DROP TABLE x",
        );
        assert!(r.is_err());
    }

    #[test]
    fn inactive_principal_sees_nothing() {
        let f = visible_documents_predicate(
            &Policy::default(),
            &principal(false),
            Permission::View,
            Utc::now(),
            "d",
        )
        .unwrap();
        assert_eq!(f.to_numbered(1), ("FALSE".to_owned(), vec![]));
    }

    #[test]
    fn placeholders_are_numbered_from_offset() {
        let f = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::Edit,
            Utc::now(),
            "doc",
        )
        .unwrap();
        let (sql, params) = f.to_numbered(4);
        assert_eq!(params.len(), 8);
        assert!(sql.contains("$4") && sql.contains("$11") && !sql.contains("$12"));
        assert!(sql.starts_with("(doc.owner_id = $4 OR "));
    }

    #[test]
    fn owner_clause_omitted_when_policy_excludes_permission() {
        let f = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::Delete,
            Utc::now(),
            "d",
        )
        .unwrap();
        assert!(!f.to_numbered(1).0.contains("owner_id"));
    }

    #[test]
    fn county_wide_clause_only_for_view() {
        let view = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::View,
            Utc::now(),
            "d",
        )
        .unwrap();
        assert!(
            view.to_numbered(1)
                .0
                .starts_with("(d.visibility = 'county_wide' OR ")
        );
        let edit = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::Edit,
            Utc::now(),
            "d",
        )
        .unwrap();
        let sql = edit.to_numbered(1).0;
        assert!(!sql.contains("county_wide"));
        assert!(sql.contains("d.visibility <> 'restricted'"));
        assert!(!sql.contains("inherit_acl") && !sql.contains("is_confidential"));
    }
}
