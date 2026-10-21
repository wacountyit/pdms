// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Certificate credentials for Microsoft Graph.
//! Entra ID accepts a JWT signed with the app's certificate in place of a
//! client secret: PS256, with the certificate's SHA-256 thumbprint in
//! `x5t#S256` (learn.microsoft.com, "certificate credentials").

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PSS_SHA256, RsaKeyPair};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::GraphError;

/// Lifetime of one client assertion (Entra: keep it to 5–10 minutes).
const ASSERTION_SECONDS: i64 = 600;

/// The app's certificate and private key, ready to sign assertions.
pub struct Credential {
    key: RsaKeyPair,
    thumbprint: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("thumbprint", &self.thumbprint)
            .finish_non_exhaustive()
    }
}

impl Credential {
    /// Parse a PEM bundle: the certificate and its unencrypted RSA key.
    pub fn from_pem(pem: &str) -> Result<Credential, GraphError> {
        let blocks = pem_blocks(pem)?;
        let cert = blocks
            .iter()
            .find(|(label, _)| label == "CERTIFICATE")
            .ok_or_else(|| {
                GraphError::Auth("the certificate PEM has no CERTIFICATE block".into())
            })?;
        let key = match blocks
            .iter()
            .find(|(label, _)| label.ends_with("PRIVATE KEY"))
        {
            Some((label, der)) if label == "PRIVATE KEY" => RsaKeyPair::from_pkcs8(der),
            Some((label, der)) if label == "RSA PRIVATE KEY" => RsaKeyPair::from_der(der),
            Some((label, _)) => {
                return Err(GraphError::Auth(format!(
                    "unsupported key {label:?}: use an unencrypted RSA private key"
                )));
            }
            None => {
                return Err(GraphError::Auth(
                    "the certificate PEM has no PRIVATE KEY block".into(),
                ));
            }
        }
        .map_err(|e| GraphError::Auth(format!("private key rejected: {e}")))?;
        Ok(Credential {
            key,
            thumbprint: thumbprint(&cert.1),
        })
    }

    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }

    /// A signed client assertion for `audience` (the token endpoint URL).
    pub fn client_assertion(
        &self,
        client_id: &str,
        audience: &str,
        now: i64,
    ) -> Result<String, GraphError> {
        let header = serde_json::json!({
            "alg": "PS256",
            "typ": "JWT",
            "x5t#S256": self.thumbprint,
        });
        let claims = serde_json::json!({
            "aud": audience,
            "iss": client_id,
            "sub": client_id,
            "jti": Uuid::new_v4().to_string(),
            "nbf": now,
            "iat": now,
            "exp": now + ASSERTION_SECONDS,
        });
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PSS_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| GraphError::Auth("signing the client assertion failed".into()))?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }

    /// DER `RSAPublicKey` (for verifying signatures in tests).
    pub fn public_key_der(&self) -> &[u8] {
        self.key.public().as_ref()
    }
}

/// Base64url SHA-256 of a DER certificate: the `x5t#S256` header value.
pub fn thumbprint(der: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(der))
}

/// A new self-signed certificate for the "PADM Service" app registration.
pub struct GeneratedCertificate {
    /// Certificate followed by its private key: what PADM reads.
    pub pem: String,
    /// The public certificate alone: what you upload to Entra ID.
    pub certificate_pem: String,
    pub thumbprint: String,
    pub not_after: chrono::DateTime<chrono::Utc>,
}

/// Generate an RSA-2048 key and a self-signed certificate valid for `valid_days`.
pub fn generate(common_name: &str, valid_days: u32) -> Result<GeneratedCertificate, GraphError> {
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    let fail = |what: &str, e: &dyn std::fmt::Display| GraphError::Auth(format!("{what}: {e}"));
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)
        .map_err(|e| fail("generating the key", &e))?;
    let key_pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| fail("encoding the key", &e))?;
    let signer = rcgen::KeyPair::from_pkcs8_pem_and_sign_algo(&key_pem, &rcgen::PKCS_RSA_SHA256)
        .map_err(|e| fail("loading the key", &e))?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())
        .map_err(|e| fail("certificate parameters", &e))?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, common_name);
    params.distinguished_name = dn;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(i64::from(valid_days));
    let cert = params
        .self_signed(&signer)
        .map_err(|e| fail("signing the certificate", &e))?;
    let certificate_pem = cert.pem();
    Ok(GeneratedCertificate {
        pem: format!("{certificate_pem}{}", key_pem.as_str()),
        thumbprint: thumbprint(cert.der()),
        certificate_pem,
        not_after: chrono::Utc::now() + chrono::Duration::days(i64::from(valid_days)),
    })
}

/// `(label, DER)` for every PEM block in `text`.
fn pem_blocks(text: &str) -> Result<Vec<(String, Vec<u8>)>, GraphError> {
    let mut out = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in text.lines().map(str::trim) {
        if let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|l| l.strip_suffix("-----"))
        {
            open = Some((label.to_owned(), String::new()));
        } else if let Some(label) = line
            .strip_prefix("-----END ")
            .and_then(|l| l.strip_suffix("-----"))
        {
            let Some((begun, body)) = open.take() else {
                return Err(GraphError::Auth(format!("PEM END {label:?} without BEGIN")));
            };
            if begun != label {
                return Err(GraphError::Auth(format!(
                    "PEM block {begun:?} ends as {label:?}"
                )));
            }
            let der = STANDARD
                .decode(body)
                .map_err(|e| GraphError::Auth(format!("PEM {label}: {e}")))?;
            out.push((label.to_owned(), der));
        } else if let Some((_, body)) = open.as_mut() {
            body.push_str(line);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    #[test]
    fn thumbprint_is_base64url_sha256() {
        assert_eq!(
            thumbprint(b"abc"),
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
        );
    }

    #[test]
    fn generated_certificate_signs_verifiable_ps256_assertions() {
        let g = generate("PADM test", 30).unwrap();
        assert!(g.pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(g.pem.contains("-----BEGIN PRIVATE KEY-----"));
        let c = Credential::from_pem(&g.pem).unwrap();
        assert_eq!(c.thumbprint(), g.thumbprint);

        let aud = "https://login.example/t/oauth2/v2.0/token";
        let jwt = c.client_assertion("client-1", aud, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "PS256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["x5t#S256"], g.thumbprint.as_str());
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], aud);
        assert_eq!(claims["iss"], "client-1");
        assert_eq!(claims["sub"], "client-1");
        assert_eq!(claims["nbf"], 1_700_000_000);
        assert_eq!(claims["exp"], 1_700_000_600);
        assert_eq!(claims["jti"].as_str().unwrap().len(), 36);

        let key = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PSS_2048_8192_SHA256,
            c.public_key_der(),
        );
        key.verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pkcs1_keys_from_openssl_are_accepted() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::pkcs8::{DecodePrivateKey, LineEnding};
        let g = generate("PADM test", 30).unwrap();
        let key_pem = &g.pem[g.certificate_pem.len()..];
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(key_pem).unwrap();
        let pkcs1 = key.to_pkcs1_pem(LineEnding::LF).unwrap();
        let bundle = format!("{}{}", g.certificate_pem, pkcs1.as_str());
        assert_eq!(
            Credential::from_pem(&bundle).unwrap().thumbprint(),
            g.thumbprint
        );
    }

    #[test]
    fn missing_or_encrypted_keys_are_rejected() {
        let g = generate("PADM test", 30).unwrap();
        let e = Credential::from_pem(&g.certificate_pem)
            .unwrap_err()
            .to_string();
        assert!(e.contains("PRIVATE KEY"), "{e}");
        let encrypted = format!(
            "{}-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
            g.certificate_pem
        );
        let e = Credential::from_pem(&encrypted).unwrap_err().to_string();
        assert!(e.contains("unencrypted"), "{e}");
        assert!(Credential::from_pem("not a pem").is_err());
    }
}
