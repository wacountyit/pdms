// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! OpenID Connect login (authorization code + PKCE). Primary IdP: Microsoft Entra ID.

use base64::Engine;
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse, reqwest,
};
use pdms_audit::{Actor, AuditEvent, AuthMethod, Category, Outcome};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Core;
use crate::auth::local::LoginSuccess;
use crate::auth::session;
use crate::config::OidcConfig;
use crate::context::ClientInfo;
use crate::directory::sync;
use crate::error::{CoreError, CoreResult};
use crate::model::UserStatus;

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

pub const CALLBACK_PATH: &str = "/auth/oidc/callback";

/// A configured, discovered OIDC provider.
pub struct OidcProvider {
    client: Client,
    http: reqwest::Client,
    config: OidcConfig,
}

impl std::fmt::Debug for OidcProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcProvider")
            .field("issuer", &self.config.issuer_url)
            .finish_non_exhaustive()
    }
}

/// Per-login state kept in an encrypted, short-lived cookie between redirect and callback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlowState {
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub return_to: String,
}

impl OidcProvider {
    pub async fn discover(config: &OidcConfig, public_url: &url::Url) -> CoreResult<OidcProvider> {
        let http = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(CoreError::internal)?;
        let issuer = IssuerUrl::new(config.issuer_url.clone())
            .map_err(|e| CoreError::validation(format!("auth.oidc.issuer_url: {e}")))?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|e| CoreError::internal(format!("OIDC discovery failed: {e}")))?;
        let redirect = public_url
            .join(CALLBACK_PATH)
            .map_err(|e| CoreError::validation(format!("server.public_url: {e}")))?;
        let secret = (!config.client_secret.is_empty())
            .then(|| ClientSecret::new(config.client_secret.expose().to_owned()));
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(config.client_id.clone()),
            secret,
        )
        .set_redirect_uri(RedirectUrl::from_url(redirect));
        Ok(OidcProvider {
            client,
            http,
            config: config.clone(),
        })
    }

    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    /// Build the authorization redirect.
    pub fn begin(&self, return_to: &str) -> (url::Url, FlowState) {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut req = self
            .client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        for s in self.config.scopes.iter().filter(|s| *s != "openid") {
            req = req.add_scope(Scope::new(s.clone()));
        }
        let (url, state, nonce) = req.url();
        (
            url,
            FlowState {
                state: state.secret().clone(),
                nonce: nonce.secret().clone(),
                pkce_verifier: verifier.secret().clone(),
                return_to: safe_return_to(return_to),
            },
        )
    }

    /// Complete the login after the IdP redirects back.
    pub async fn complete(
        &self,
        core: &Core,
        flow: FlowState,
        returned_state: &str,
        code: &str,
        client_info: &ClientInfo,
    ) -> CoreResult<LoginSuccess> {
        let fail = |reason: &str, detail: String| {
            let actor = Actor {
                user_id: None,
                display: None,
                auth_method: AuthMethod::Oidc,
                ip: client_info.ip,
                user_agent: client_info.user_agent.clone(),
                api_token_id: None,
                request_id: client_info.request_id.clone(),
            };
            AuditEvent::new(Category::Auth, "auth.login_failed", actor, "user")
                .outcome(Outcome::Failure)
                .context("reason", reason)
                .context("detail", detail.chars().take(500).collect::<String>())
        };

        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            flow.state.as_bytes(),
            returned_state.as_bytes(),
        )) {
            pdms_audit::record(&core.db, vec![fail("state_mismatch", String::new())]).await?;
            return Err(CoreError::Unauthenticated);
        }

        let claims = match self.exchange(flow, code).await {
            Ok(c) => c,
            Err(detail) => {
                tracing::warn!(%detail, "OIDC token exchange or validation failed");
                pdms_audit::record(&core.db, vec![fail("token_invalid", detail)]).await?;
                return Err(CoreError::Unauthenticated);
            }
        };

        let user = match self.resolve_user(core, &claims).await? {
            Ok(u) => u,
            Err(reason) => {
                let mut e = fail(reason, String::new())
                    .context("issuer", claims.issuer.clone())
                    .context("subject", claims.subject.clone());
                if let Some(email) = &claims.email {
                    e = e.context("attempted_identifier", email.clone());
                }
                pdms_audit::record(&core.db, vec![e]).await?;
                return Err(CoreError::Forbidden(None));
            }
        };

        let mut tx = core.db.begin().await?;
        let ctx = crate::context::Ctx::system("idp-sync");
        let mut actor = Actor {
            user_id: Some(user.0),
            display: Some(user.1.clone()),
            auth_method: AuthMethod::Oidc,
            ip: client_info.ip,
            user_agent: client_info.user_agent.clone(),
            api_token_id: None,
            request_id: client_info.request_id.clone(),
        };
        let mut events = Vec::new();
        if claims.groups_overage {
            tracing::warn!(user_id = %user.0, "IdP group overage: groups claim omitted; memberships left unchanged");
        } else if let Some(change) =
            sync::reconcile_idp_memberships(&mut tx, user.0, &claims.groups).await?
        {
            let _ = &ctx;
            events.push(
                AuditEvent::new(
                    Category::Permission,
                    "user.idp_memberships_synced",
                    actor.clone(),
                    "user",
                )
                .object(user.0, Some(&user.1))
                .change(
                    "departments",
                    change.departments_before,
                    change.departments_after,
                )
                .change("groups", change.groups_before, change.groups_after)
                .change("roles", change.roles_before, change.roles_after),
            );
        }
        sqlx::query!(
            "UPDATE users SET last_login_at = now(), display_name = $2, updated_at = now() WHERE id = $1",
            user.0,
            claims.name.clone().unwrap_or_else(|| user.1.clone()),
        )
        .execute(&mut *tx)
        .await?;
        let token = session::create(
            &mut tx,
            &core.config.auth,
            user.0,
            AuthMethod::Oidc,
            client_info,
        )
        .await?;
        actor.display = Some(user.1.clone());
        events.push(
            AuditEvent::new(Category::Auth, "auth.login", actor, "user")
                .object(user.0, Some(&user.1))
                .context("issuer", claims.issuer.clone()),
        );
        pdms_audit::append(&mut tx, events).await?;
        tx.commit().await?;
        Ok(LoginSuccess {
            token,
            user_id: user.0,
            display_name: user.1,
        })
    }

    async fn exchange(&self, flow: FlowState, code: &str) -> Result<VerifiedClaims, String> {
        let response = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .map_err(|e| e.to_string())?
            .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier))
            .request_async(&self.http)
            .await
            .map_err(|e| format!("token request: {e}"))?;
        let id_token = response
            .id_token()
            .ok_or_else(|| "no id_token in response".to_owned())?;
        let verifier = self.client.id_token_verifier();
        let claims = id_token
            .claims(&verifier, &Nonce::new(flow.nonce))
            .map_err(|e| format!("id_token: {e}"))?;

        // Signature, issuer, audience, expiry, and nonce are verified above;
        // re-read the payload for provider-specific claims (oid, groups).
        let raw = decode_jwt_payload(&id_token.to_string())?;
        let subject = if self.config.subject_claim == "sub" {
            claims.subject().to_string()
        } else {
            raw.get(&self.config.subject_claim)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("missing {} claim", self.config.subject_claim))?
                .to_owned()
        };
        let email = claims
            .email()
            .map(|e| e.to_string())
            .or_else(|| claims.preferred_username().map(|u| u.to_string()))
            .filter(|e| e.contains('@'));
        let name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.to_string());
        let groups = raw
            .get(&self.config.groups_claim)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|g| g.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        // Entra replaces the groups claim with _claim_names when a user has too many groups.
        let groups_overage = raw
            .get("_claim_names")
            .and_then(|c| c.get(&self.config.groups_claim))
            .is_some();
        Ok(VerifiedClaims {
            issuer: claims.issuer().to_string(),
            subject,
            email,
            name,
            groups,
            groups_overage,
        })
    }

    /// Find, link, or provision the local user. `Ok(Err(reason))` means "deny".
    async fn resolve_user(
        &self,
        core: &Core,
        claims: &VerifiedClaims,
    ) -> CoreResult<Result<(Uuid, String), &'static str>> {
        let existing = sqlx::query!(
            r#"SELECT id, display_name, status AS "status: UserStatus"
                 FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2"#,
            claims.issuer,
            claims.subject,
        )
        .fetch_optional(&core.db)
        .await?;
        if let Some(u) = existing {
            return Ok(if u.status == UserStatus::Active {
                Ok((u.id, u.display_name))
            } else {
                Err("inactive")
            });
        }

        let Some(email) = &claims.email else {
            return Ok(Err("no_email_claim"));
        };
        let display = claims.name.clone().unwrap_or_else(|| email.clone());

        // An administrator may have pre-provisioned the user by email.
        let mut tx = core.db.begin().await?;
        let pre = sqlx::query!(
            r#"SELECT id, display_name, status AS "status: UserStatus", oidc_subject
                 FROM users WHERE lower(email) = lower($1) FOR UPDATE"#,
            email
        )
        .fetch_optional(&mut *tx)
        .await?;
        let system = Actor::system("oidc-login");
        let result = match pre {
            Some(u) if u.oidc_subject.is_some() => Err("email_bound_to_other_identity"),
            Some(u) if u.status != UserStatus::Active => Err("inactive"),
            Some(u) => {
                sqlx::query!(
                    "UPDATE users SET oidc_issuer = $2, oidc_subject = $3, updated_at = now() WHERE id = $1",
                    u.id,
                    claims.issuer,
                    claims.subject,
                )
                .execute(&mut *tx)
                .await?;
                pdms_audit::append(
                    &mut tx,
                    vec![
                        AuditEvent::new(Category::Admin, "user.identity_linked", system, "user")
                            .object(u.id, Some(&u.display_name))
                            .change("oidc_issuer", None::<String>, claims.issuer.clone())
                            .change("oidc_subject", None::<String>, claims.subject.clone()),
                    ],
                )
                .await?;
                Ok((u.id, u.display_name))
            }
            None if self.config.auto_provision => {
                let id = Uuid::now_v7();
                sqlx::query!(
                    "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject)
                     VALUES ($1, $2, $3, $4, $5)",
                    id,
                    display,
                    email,
                    claims.issuer,
                    claims.subject,
                )
                .execute(&mut *tx)
                .await?;
                pdms_audit::append(
                    &mut tx,
                    vec![
                        AuditEvent::new(Category::Admin, "user.provisioned", system, "user")
                            .object(id, Some(&display))
                            .change("email", None::<String>, email.clone())
                            .change("oidc_subject", None::<String>, claims.subject.clone())
                            .context("source", "oidc_first_login"),
                    ],
                )
                .await?;
                Ok((id, display))
            }
            None => Err("not_provisioned"),
        };
        tx.commit().await?;
        Ok(result)
    }
}

#[derive(Debug, Clone)]
struct VerifiedClaims {
    issuer: String,
    subject: String,
    email: Option<String>,
    name: Option<String>,
    groups: Vec<String>,
    groups_overage: bool,
}

fn decode_jwt_payload(jwt: &str) -> Result<serde_json::Value, String> {
    let payload = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| "malformed JWT".to_owned())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|e| format!("JWT payload: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("JWT payload: {e}"))
}

/// Only allow local, absolute-path redirects (prevents open redirects).
pub fn safe_return_to(path: &str) -> String {
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(char::is_control);
    if ok { path.to_owned() } else { "/".to_owned() }
}

/// Issuer URLs are compared and stored without surrounding spaces or a trailing `/`.
pub fn normalize_issuer(issuer: &str) -> &str {
    issuer.trim().trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_rejects_offsite_targets() {
        assert_eq!(safe_return_to("/admin/users?x=1"), "/admin/users?x=1");
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/\\evil",
            "javascript:x",
            "/a\nb",
        ] {
            assert_eq!(safe_return_to(bad), "/", "{bad}");
        }
    }

    #[test]
    fn decodes_jwt_payload() {
        let p = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"oid":"abc"}"#);
        let v = decode_jwt_payload(&format!("h.{p}.s")).unwrap();
        assert_eq!(v["oid"], "abc");
    }
}
