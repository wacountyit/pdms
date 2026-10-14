// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! db_enum {
    ($(#[$meta:meta])* $name:ident, $pg:literal { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[cfg_attr(feature = "postgres", derive(sqlx::Type))]
        #[cfg_attr(feature = "postgres", sqlx(type_name = $pg, rename_all = "snake_case"))]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = UnknownVariant;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err(UnknownVariant { kind: stringify!($name), value: s.to_owned() }),
                }
            }
        }
    };
}

#[derive(Debug, thiserror::Error)]
#[error("unknown {kind}: {value:?}")]
pub struct UnknownVariant {
    pub kind: &'static str,
    pub value: String,
}

db_enum!(
    /// Broad class of an audit event, so content, metadata, permission and
    /// administrative changes are clearly distinguished.
    Category, "audit_category" {
        Content = "content",
        Metadata = "metadata",
        Permission = "permission",
        Admin = "admin",
        Auth = "auth",
        Access = "access",
        Workflow = "workflow",
        Notification = "notification",
        System = "system",
    }
);

db_enum!(Outcome, "audit_outcome" {
    Success = "success",
    Denied = "denied",
    Failure = "failure",
});

db_enum!(AuthMethod, "auth_method" {
    Oidc = "oidc",
    Local = "local",
    ApiToken = "api_token",
    IcsToken = "ics_token",
    System = "system",
});

/// A value recorded in an audit entry.
///
/// There is deliberately no floating-point variant: hashes are recomputed from
/// values read back out of `jsonb`, and only integers, strings, booleans, and
/// null survive that round trip byte-for-byte. Record decimals as strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditValue {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<AuditValue>),
    Map(BTreeMap<String, AuditValue>),
}

#[derive(Debug, thiserror::Error)]
#[error("value cannot be represented in the audit log: {0}")]
pub struct NotAuditable(pub String);

impl AuditValue {
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            AuditValue::Null => Value::Null,
            AuditValue::Bool(b) => Value::Bool(*b),
            AuditValue::Int(i) => Value::from(*i),
            AuditValue::Str(s) => Value::String(s.clone()),
            AuditValue::List(items) => Value::Array(items.iter().map(Self::to_json).collect()),
            AuditValue::Map(m) => {
                Value::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())
            }
        }
    }

    pub fn from_json(value: &serde_json::Value) -> Result<Self, NotAuditable> {
        use serde_json::Value;
        Ok(match value {
            Value::Null => AuditValue::Null,
            Value::Bool(b) => AuditValue::Bool(*b),
            Value::Number(n) => AuditValue::Int(
                n.as_i64()
                    .ok_or_else(|| NotAuditable(format!("non-i64 number {n}")))?,
            ),
            Value::String(s) => AuditValue::Str(s.clone()),
            Value::Array(items) => AuditValue::List(
                items
                    .iter()
                    .map(Self::from_json)
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(m) => AuditValue::Map(
                m.iter()
                    .map(|(k, v)| Ok((k.clone(), Self::from_json(v)?)))
                    .collect::<Result<_, NotAuditable>>()?,
            ),
        })
    }

    /// Replace characters Postgres cannot store (NUL) in every string.
    fn normalize(&mut self) {
        match self {
            AuditValue::Str(s) => sanitize(s),
            AuditValue::List(items) => items.iter_mut().for_each(Self::normalize),
            AuditValue::Map(m) => {
                *m = std::mem::take(m)
                    .into_iter()
                    .map(|(mut k, mut v)| {
                        sanitize(&mut k);
                        v.normalize();
                        (k, v)
                    })
                    .collect();
            }
            AuditValue::Null | AuditValue::Bool(_) | AuditValue::Int(_) => {}
        }
    }
}

impl fmt::Display for AuditValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditValue::Null => f.write_str("—"),
            AuditValue::Str(s) => f.write_str(s),
            other => write!(f, "{}", other.to_json()),
        }
    }
}

impl From<bool> for AuditValue {
    fn from(v: bool) -> Self {
        AuditValue::Bool(v)
    }
}
impl From<i64> for AuditValue {
    fn from(v: i64) -> Self {
        AuditValue::Int(v)
    }
}
impl From<i32> for AuditValue {
    fn from(v: i32) -> Self {
        AuditValue::Int(v.into())
    }
}
impl From<u32> for AuditValue {
    fn from(v: u32) -> Self {
        AuditValue::Int(v.into())
    }
}
impl From<&str> for AuditValue {
    fn from(v: &str) -> Self {
        AuditValue::Str(v.to_owned())
    }
}
impl From<String> for AuditValue {
    fn from(v: String) -> Self {
        AuditValue::Str(v)
    }
}
impl From<Uuid> for AuditValue {
    fn from(v: Uuid) -> Self {
        AuditValue::Str(v.to_string())
    }
}
impl From<DateTime<Utc>> for AuditValue {
    fn from(v: DateTime<Utc>) -> Self {
        AuditValue::Str(format_timestamp(v))
    }
}
impl From<NaiveDate> for AuditValue {
    fn from(v: NaiveDate) -> Self {
        AuditValue::Str(v.format("%Y-%m-%d").to_string())
    }
}
impl<T: Into<AuditValue>> From<Option<T>> for AuditValue {
    fn from(v: Option<T>) -> Self {
        v.map_or(AuditValue::Null, Into::into)
    }
}
impl<T: Into<AuditValue>> From<Vec<T>> for AuditValue {
    fn from(v: Vec<T>) -> Self {
        AuditValue::List(v.into_iter().map(Into::into).collect())
    }
}

/// RFC 3339, UTC, exactly six fractional digits.
pub fn format_timestamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

fn sanitize(s: &mut String) {
    if s.contains('\0') {
        *s = s.replace('\0', "\u{FFFD}");
    }
}

fn sanitize_opt(s: &mut Option<String>) {
    if let Some(s) = s {
        sanitize(s);
    }
}

/// Who performed the action and how they were authenticated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub user_id: Option<Uuid>,
    pub display: Option<String>,
    pub auth_method: AuthMethod,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub api_token_id: Option<Uuid>,
    pub request_id: Option<String>,
}

impl Actor {
    /// The system itself (scheduler, migrations, CLI maintenance).
    pub fn system(label: &str) -> Self {
        Actor {
            user_id: None,
            display: Some(label.to_owned()),
            auth_method: AuthMethod::System,
            ip: None,
            user_agent: None,
            api_token_id: None,
            request_id: None,
        }
    }
}

/// A field-level before/after pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldChange {
    pub field: String,
    pub before: AuditValue,
    pub after: AuditValue,
}

/// An audit event before it is sealed into the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub category: Category,
    /// Dotted, lowercase: `document.metadata_updated`, `auth.login_failed`.
    pub action: String,
    pub outcome: Outcome,
    pub actor: Actor,
    pub object_type: String,
    pub object_id: Option<String>,
    pub object_label: Option<String>,
    pub document_id: Option<Uuid>,
    pub department_id: Option<Uuid>,
    pub changes: Option<Vec<FieldChange>>,
    pub context: BTreeMap<String, AuditValue>,
}

impl AuditEvent {
    pub fn new(category: Category, action: &str, actor: Actor, object_type: &str) -> Self {
        AuditEvent {
            id: Uuid::now_v7(),
            occurred_at: Utc::now(),
            category,
            action: action.to_owned(),
            outcome: Outcome::Success,
            actor,
            object_type: object_type.to_owned(),
            object_id: None,
            object_label: None,
            document_id: None,
            department_id: None,
            changes: None,
            context: BTreeMap::new(),
        }
    }

    pub fn outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = outcome;
        self
    }

    pub fn object(mut self, id: impl ToString, label: Option<&str>) -> Self {
        self.object_id = Some(id.to_string());
        self.object_label = label.map(str::to_owned);
        self
    }

    pub fn document(mut self, id: Uuid) -> Self {
        self.document_id = Some(id);
        self
    }

    pub fn document_opt(mut self, id: Option<Uuid>) -> Self {
        if id.is_some() {
            self.document_id = id;
        }
        self
    }

    pub fn department(mut self, id: Option<Uuid>) -> Self {
        self.department_id = id;
        self
    }

    /// Record a field change. Unchanged values are skipped.
    pub fn change(
        mut self,
        field: &str,
        before: impl Into<AuditValue>,
        after: impl Into<AuditValue>,
    ) -> Self {
        let (before, after) = (before.into(), after.into());
        if before != after {
            self.changes.get_or_insert_with(Vec::new).push(FieldChange {
                field: field.to_owned(),
                before,
                after,
            });
        }
        self
    }

    pub fn context(mut self, key: &str, value: impl Into<AuditValue>) -> Self {
        self.context.insert(key.to_owned(), value.into());
        self
    }

    /// Bring the event into the exact form it will have after a database round
    /// trip: microsecond timestamps, no NUL characters, IPv4-mapped addresses
    /// as IPv4. Called by [`crate::seal`].
    pub(crate) fn normalize(&mut self) {
        self.occurred_at = self.occurred_at.trunc_subsecs(6);
        sanitize(&mut self.action);
        sanitize(&mut self.object_type);
        sanitize_opt(&mut self.object_id);
        sanitize_opt(&mut self.object_label);
        sanitize_opt(&mut self.actor.display);
        sanitize_opt(&mut self.actor.user_agent);
        sanitize_opt(&mut self.actor.request_id);
        if let Some(IpAddr::V6(v6)) = self.actor.ip
            && let Some(v4) = v6.to_ipv4_mapped()
        {
            self.actor.ip = Some(IpAddr::V4(v4));
        }
        for c in self.changes.iter_mut().flatten() {
            sanitize(&mut c.field);
            c.before.normalize();
            c.after.normalize();
        }
        let mut ctx = AuditValue::Map(std::mem::take(&mut self.context));
        ctx.normalize();
        if let AuditValue::Map(m) = ctx {
            self.context = m;
        }
    }

    pub fn changes_json(&self) -> Option<serde_json::Value> {
        self.changes.as_ref().map(|cs| {
            serde_json::Value::Array(
                cs.iter()
                    .map(|c| {
                        serde_json::json!({
                            "field": c.field,
                            "before": c.before.to_json(),
                            "after": c.after.to_json(),
                        })
                    })
                    .collect(),
            )
        })
    }

    pub fn context_json(&self) -> serde_json::Value {
        AuditValue::Map(self.context.clone()).to_json()
    }
}

pub(crate) fn changes_from_json(
    value: &serde_json::Value,
) -> Result<Vec<FieldChange>, NotAuditable> {
    let items = value
        .as_array()
        .ok_or_else(|| NotAuditable("changes is not an array".into()))?;
    items
        .iter()
        .map(|item| {
            let field = item
                .get("field")
                .and_then(|f| f.as_str())
                .ok_or_else(|| NotAuditable("change without field".into()))?;
            let get = |k: &str| {
                item.get(k)
                    .map(AuditValue::from_json)
                    .unwrap_or(Ok(AuditValue::Null))
            };
            Ok(FieldChange {
                field: field.to_owned(),
                before: get("before")?,
                after: get("after")?,
            })
        })
        .collect()
}

pub(crate) fn context_from_json(
    value: &serde_json::Value,
) -> Result<BTreeMap<String, AuditValue>, NotAuditable> {
    match AuditValue::from_json(value)? {
        AuditValue::Map(m) => Ok(m),
        _ => Err(NotAuditable("context is not an object".into())),
    }
}
