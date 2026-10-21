// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Errors from Microsoft Graph.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::CoreError;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("Microsoft Graph is busy; retry after {}s", retry_after.as_secs())]
    Throttled { retry_after: Duration },
    #[error("not found in Microsoft Graph")]
    NotFound,
    #[error("conflict in Microsoft Graph (the item exists or changed)")]
    Conflict,
    #[error("Microsoft Graph asks for a full resynchronization")]
    ResyncRequired,
    #[error("Microsoft Graph authentication: {0}")]
    Auth(String),
    #[error("Microsoft Graph error {status} {code}: {message}")]
    Http {
        status: u16,
        code: String,
        message: String,
    },
    #[error("Microsoft Graph unreachable: {0}")]
    Transport(String),
}

impl GraphError {
    /// Map a non-success response (after retries) to an error.
    pub fn from_response(status: StatusCode, body: &str) -> GraphError {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let code = v
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let message: String = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(300)
            .collect();
        match status.as_u16() {
            404 => GraphError::NotFound,
            409 | 412 => GraphError::Conflict,
            410 => GraphError::ResyncRequired,
            _ if matches!(
                code.as_str(),
                "resyncRequired" | "syncStateNotFound" | "syncStateInvalid"
            ) =>
            {
                GraphError::ResyncRequired
            }
            401 | 403 => GraphError::Auth(if code.is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!("{code}: {message}")
            }),
            s => GraphError::Http {
                status: s,
                code,
                message,
            },
        }
    }
}

/// Callers handle `NotFound` and `ResyncRequired` before converting; whatever
/// is left is an internal failure (not a 404 for the person in the browser).
impl From<GraphError> for CoreError {
    fn from(e: GraphError) -> Self {
        match e {
            GraphError::Conflict => CoreError::Conflict(e.to_string()),
            other => CoreError::Internal(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;

    #[test]
    fn responses_map_to_error_kinds() {
        let e =
            |s: u16, body: &str| GraphError::from_response(StatusCode::from_u16(s).unwrap(), body);
        assert!(matches!(e(404, ""), GraphError::NotFound));
        assert!(matches!(e(409, ""), GraphError::Conflict));
        assert!(matches!(e(412, ""), GraphError::Conflict));
        assert!(matches!(e(410, ""), GraphError::ResyncRequired));
        assert!(matches!(
            e(
                400,
                r#"{"error":{"code":"syncStateNotFound","message":"x"}}"#
            ),
            GraphError::ResyncRequired
        ));
        assert!(matches!(
            e(403, r#"{"error":{"code":"Authorization_RequestDenied","message":"no"}}"#),
            GraphError::Auth(m) if m.contains("Authorization_RequestDenied")
        ));
        assert!(matches!(
            e(500, r#"{"error":{"code":"generalException","message":"boom"}}"#),
            GraphError::Http { status: 500, ref code, .. } if code == "generalException"
        ));
        let core: crate::CoreError = GraphError::Conflict.into();
        assert!(matches!(core, crate::CoreError::Conflict(_)));
        let core: crate::CoreError = GraphError::NotFound.into();
        assert!(matches!(core, crate::CoreError::Internal(_)));
    }
}
