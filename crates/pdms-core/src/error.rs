// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
use pdms_authz::DenyReason;

/// Errors returned by domain services. The web layer maps these to HTTP statuses.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("not authenticated")]
    Unauthenticated,
    #[error("forbidden")]
    Forbidden(Option<DenyReason>),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    /// A service PDMS depends on (SharePoint, Microsoft Graph) is unreachable;
    /// nothing was changed.
    #[error("{0}")]
    Unavailable(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit error: {0}")]
    Audit(#[from] pdms_audit::AuditError),
    #[error("{0}")]
    Internal(String),
}

impl CoreError {
    pub fn validation(msg: impl Into<String>) -> Self {
        CoreError::Validation(msg.into())
    }

    pub fn internal(msg: impl std::fmt::Display) -> Self {
        CoreError::Internal(msg.to_string())
    }

    /// Map unique-constraint violations to a friendly conflict message.
    pub fn conflict_on_unique(e: sqlx::Error, msg: &str) -> Self {
        match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                CoreError::Conflict(msg.to_owned())
            }
            _ => CoreError::Db(e),
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;
