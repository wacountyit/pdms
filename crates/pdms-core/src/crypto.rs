// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Key derivation, random tokens, hashing, and authenticated encryption.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Purpose-specific keys derived from the master secret with HKDF-SHA256.
#[derive(Clone)]
pub struct Keys {
    csrf: [u8; 32],
    cookie: [u8; 64],
    totp: [u8; 32],
    integrations: [u8; 32],
    settings: [u8; 32],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys([redacted])")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("decryption failed")]
    Decrypt,
    #[error("encryption failed")]
    Encrypt,
}

impl Keys {
    pub fn derive(master: &[u8]) -> Keys {
        let hk = Hkdf::<Sha256>::new(Some(b"pdms-v1"), master);
        let mut csrf = [0u8; 32];
        let mut cookie = [0u8; 64];
        let mut totp = [0u8; 32];
        let mut integrations = [0u8; 32];
        let mut settings = [0u8; 32];
        // Output lengths are far below the HKDF limit, so expand cannot fail.
        let _ = hk.expand(b"csrf", &mut csrf);
        let _ = hk.expand(b"cookie", &mut cookie);
        let _ = hk.expand(b"totp-secret-encryption", &mut totp);
        let _ = hk.expand(b"integration-secret-encryption", &mut integrations);
        let _ = hk.expand(b"settings-secret-encryption", &mut settings);
        Keys {
            csrf,
            cookie,
            totp,
            integrations,
            settings,
        }
    }

    /// Key material for encrypted cookies (64 bytes, as `cookie::Key::from` expects).
    pub fn cookie_key(&self) -> &[u8; 64] {
        &self.cookie
    }

    /// CSRF token bound to a session: HMAC-SHA256(csrf key, session token).
    pub fn csrf_token(&self, session_token: &str) -> String {
        URL_SAFE_NO_PAD.encode(self.csrf_mac(session_token))
    }

    pub fn verify_csrf(&self, session_token: &str, presented: &str) -> bool {
        let Ok(presented) = URL_SAFE_NO_PAD.decode(presented.trim()) else {
            return false;
        };
        let expected = self.csrf_mac(session_token);
        presented.len() == expected.len() && bool::from(presented.ct_eq(&expected))
    }

    fn csrf_mac(&self, session_token: &str) -> [u8; 32] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.csrf)
            .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
        mac.update(session_token.as_bytes());
        mac.finalize().into_bytes().into()
    }

    pub fn encrypt_totp_secret(
        &self,
        user_id: uuid::Uuid,
        secret: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        encrypt(&self.totp, secret, user_id.as_bytes())
    }

    pub fn decrypt_totp_secret(
        &self,
        user_id: uuid::Uuid,
        data: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        decrypt(&self.totp, data, user_id.as_bytes())
    }
}

impl Keys {
    /// Encrypt an integration secret (e.g. a webhook signing secret) bound to its record.
    pub fn encrypt_integration_secret(
        &self,
        id: uuid::Uuid,
        secret: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        encrypt(&self.integrations, secret, id.as_bytes())
    }

    /// Encrypt a secret setting (S1), bound to its key so a ciphertext can't
    /// be moved to another setting.
    pub fn encrypt_setting(&self, key: &str, secret: &[u8]) -> Result<Vec<u8>, CryptoError> {
        encrypt(&self.settings, secret, key.as_bytes())
    }

    pub fn decrypt_setting(&self, key: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        decrypt(&self.settings, data, key.as_bytes())
    }

    pub fn decrypt_integration_secret(
        &self,
        id: uuid::Uuid,
        data: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        decrypt(&self.integrations, data, id.as_bytes())
    }
}

/// AES-256-GCM; output is `nonce || ciphertext`. `aad` binds the ciphertext to its owner.
fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce_bytes: [u8; 12] = random_bytes();
    let ct = cipher
        .encrypt(
            &Nonce::from(nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

fn decrypt(key: &[u8; 32], data: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if data.len() < 12 {
        return Err(CryptoError::Decrypt);
    }
    let (nonce, ct) = data.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().map_err(|_| CryptoError::Decrypt)?;
    Aes256Gcm::new(key.into())
        .decrypt(&Nonce::from(nonce), Payload { msg: ct, aad })
        .map_err(|_| CryptoError::Decrypt)
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

/// A new 256-bit bearer token, base64url without padding.
pub fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// A fresh base64 master secret for `PDMS_SECRET_KEY`.
pub fn generate_master_secret() -> String {
    base64::engine::general_purpose::STANDARD.encode(random_bytes::<64>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_tokens_are_session_bound() {
        let k = Keys::derive(&[7u8; 32]);
        let t = k.csrf_token("session-a");
        assert!(k.verify_csrf("session-a", &t));
        assert!(!k.verify_csrf("session-b", &t));
        assert!(!k.verify_csrf("session-a", "garbage"));
        assert!(!Keys::derive(&[8u8; 32]).verify_csrf("session-a", &t));
    }

    #[test]
    fn totp_secret_encryption_round_trips_and_binds_user() {
        let k = Keys::derive(&[1u8; 32]);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let ct = k.encrypt_totp_secret(a, b"secret").unwrap();
        assert_eq!(k.decrypt_totp_secret(a, &ct).unwrap(), b"secret");
        assert!(
            k.decrypt_totp_secret(b, &ct).is_err(),
            "ciphertext moved to another user"
        );
    }
}
