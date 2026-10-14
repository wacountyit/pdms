// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Argon2id password hashing (OWASP parameters: m=19 MiB, t=2, p=1).

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

use crate::error::{CoreError, CoreResult};

pub const MIN_LENGTH: usize = 12;
pub const MAX_LENGTH: usize = 1024;

/// Length bounds only, per NIST 800-63B; no composition rules.
pub fn validate(password: &str) -> CoreResult<()> {
    let n = password.chars().count();
    if n < MIN_LENGTH {
        return Err(CoreError::validation(format!(
            "password must be at least {MIN_LENGTH} characters"
        )));
    }
    if password.len() > MAX_LENGTH {
        return Err(CoreError::validation("password is too long"));
    }
    Ok(())
}

pub async fn hash(password: String) -> CoreResult<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(CoreError::internal)
    })
    .await
    .map_err(CoreError::internal)?
}

/// Verify a password. With `hash = None` a dummy hash is checked so unknown
/// accounts take the same time as known ones.
pub async fn verify(hash: Option<String>, password: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let known = hash.is_some();
        let stored = hash.unwrap_or_else(|| dummy_hash().to_owned());
        let Ok(parsed) = PasswordHash::new(&stored) else {
            return false;
        };
        let ok = Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        ok && known
    })
    .await
    .unwrap_or(false)
}

/// Hash of a random string with production parameters, for timing equalization.
fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(crate::crypto::new_token().as_bytes(), &salt)
            .map(|h| h.to_string())
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_and_verify() {
        let h = hash("correct horse battery staple".into()).await.unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify(Some(h.clone()), "correct horse battery staple".into()).await);
        assert!(!verify(Some(h), "wrong password here".into()).await);
        assert!(!verify(None, "anything at all!".into()).await);
    }

    #[test]
    fn length_rules() {
        assert!(validate("short").is_err());
        assert!(validate("twelve chars").is_ok());
        assert!(validate(&"x".repeat(2000)).is_err());
    }
}
