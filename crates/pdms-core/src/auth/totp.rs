// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! RFC 6238 TOTP (SHA-1, 6 digits, 30 s) with ±1 step skew and replay protection.

use totp_rs::{Algorithm, TOTP};

use crate::error::{CoreError, CoreResult};

const STEP: u64 = 30;

pub fn generate_secret() -> Vec<u8> {
    crate::crypto::random_bytes::<20>().to_vec()
}

fn totp(secret: &[u8], issuer: &str, account: &str) -> CoreResult<TOTP> {
    // otpauth labels may not contain ':'.
    let clean = |s: &str| s.replace(':', "-");
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        STEP,
        secret.to_vec(),
        Some(clean(issuer)),
        clean(account),
    )
    .map_err(CoreError::internal)
}

/// `otpauth://` URI for authenticator apps.
pub fn provisioning_uri(secret: &[u8], issuer: &str, account: &str) -> CoreResult<String> {
    Ok(totp(secret, issuer, account)?.get_url())
}

pub fn secret_base32(secret: &[u8]) -> String {
    totp_rs::Secret::Raw(secret.to_vec())
        .to_encoded()
        .to_string()
}

/// Returns the matched time step if `code` is valid at `unix_time` and newer
/// than `last_step`.
pub fn verify(secret: &[u8], code: &str, unix_time: u64, last_step: Option<i64>) -> Option<i64> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let t = totp(secret, "pdms", "user").ok()?;
    let current = unix_time / STEP;
    [current.saturating_sub(1), current, current + 1]
        .into_iter()
        .filter(|step| last_step.is_none_or(|last| i64::try_from(*step).is_ok_and(|s| s > last)))
        .find(|step| {
            let expected = t.generate(step * STEP);
            subtle::ConstantTimeEq::ct_eq(expected.as_bytes(), code.as_bytes()).into()
        })
        .and_then(|step| i64::try_from(step).ok())
}

/// The current code, for tests and the demo.
pub fn current_code(secret: &[u8], unix_time: u64) -> CoreResult<String> {
    Ok(totp(secret, "pdms", "user")?.generate(unix_time))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_and_adjacent_steps_once() {
        let s = generate_secret();
        let now = 1_800_000_000;
        let code = current_code(&s, now).unwrap();
        let step = verify(&s, &code, now, None).unwrap();
        assert_eq!(step, (now / 30) as i64);
        assert_eq!(verify(&s, &code, now, Some(step)), None, "replay rejected");
        assert!(
            verify(&s, &code, now + 30, None).is_some(),
            "one step of skew allowed"
        );
        assert!(verify(&s, &code, now + 90, None).is_none());
        assert!(verify(&s, "12345", now, None).is_none());
    }

    #[test]
    fn provisioning_uri_sanitizes_labels() {
        let uri = provisioning_uri(&generate_secret(), "Org: PDMS", "a@b.org").unwrap();
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(!uri.contains("Org:"));
    }
}
