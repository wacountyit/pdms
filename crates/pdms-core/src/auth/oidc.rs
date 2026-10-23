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

        let user = match resolve_identity(
            core,
            &claims.issuer,
            &claims.subject,
            claims.email.as_deref(),
            claims.name.as_deref(),
        )
        .await?
        {
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
        let syncing = core.config().entra_sync.enabled;
        if syncing {
            // Departments come from the Entra Department field through the
            // directory sync (D-086), never from token group claims.
        } else if claims.groups_overage {
            tracing::warn!(user_id = %user.0, "IdP group overage: groups claim omitted; memberships left unchanged");
        } else if let Some(change) =
            sync::reconcile_idp_memberships(&mut tx, user.0, &claims.groups).await?
        {
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
        // While syncing, the name is Entra's (set by the sync), not the token's.
        // A token without a name leaves the name as it is.
        let name = claims.name.clone().filter(|_| !syncing);
        sqlx::query!(
            "UPDATE users SET last_login_at = now(), display_name = coalesce($2, display_name),
                    updated_at = now()
              WHERE id = $1",
            user.0,
            name,
        )
        .execute(&mut *tx)
        .await?;
        let token = session::create(
            &mut tx,
            &core.config().auth,
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
        let name = display_name(claims.name().and_then(|n| n.get(None)).map(|n| n.as_str()));
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
}

/// The token's name as a display name, normalized once for both provisioning
/// and the login update: trimmed, at most 200 characters (the
/// `users.display_name` limit), and `None` when blank.
fn display_name(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    (!name.is_empty()).then(|| name.chars().take(200).collect())
}

#[derive(Debug, Clone)]
struct VerifiedClaims {
    issuer: String,
    subject: String,
    email: Option<String>,
    /// Normalized by [`display_name`].
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

/// Find or provision the account for a verified sign-in. `Ok(Err(reason))`
/// means "deny". Accounts are matched by issuer and subject (Entra object ID)
/// only, never by email (D-086).
pub async fn resolve_identity(
    core: &Core,
    issuer: &str,
    subject: &str,
    email: Option<&str>,
    name: Option<&str>,
) -> CoreResult<Result<(Uuid, String), &'static str>> {
    let issuer = normalize_issuer(issuer);
    if let Some(found) = find_identity(core, issuer, subject).await? {
        return Ok(found);
    }
    if core.config().entra_sync.enabled && core.graph().is_some() {
        // Entra decides: enabled member or not, and the department.
        return Ok(
            match crate::directory::entra_sync::sync_one(core, subject).await {
                Ok(Some(_)) => match find_identity(core, issuer, subject).await? {
                    Some(found) => found,
                    None => {
                        // The sync stores the configured issuer and Graph's
                        // object ID; this token's differ in form.
                        tracing::error!(
                            %issuer,
                            %subject,
                            "Entra has this person, but no account has this sign-in's issuer and subject; \
                             check auth.oidc.issuer_url and subject_claim"
                        );
                        Err("not_provisioned")
                    }
                },
                Ok(None) => {
                    // A sync running at the same moment may have created the
                    // account just now (its insert won).
                    if let Some(found) = find_identity(core, issuer, subject).await? {
                        found
                    } else if email_taken(core, issuer, subject, email).await? {
                        Err("email_in_use")
                    } else {
                        Err("not_provisioned")
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Entra lookup at first sign-in failed");
                    Err("directory_unavailable")
                }
            },
        );
    }
    if !core.config().auth.oidc.auto_provision {
        return Ok(Err("not_provisioned"));
    }
    let Some(email) = email else {
        return Ok(Err("no_email_claim"));
    };
    let display: String = name.unwrap_or(email).chars().take(200).collect();
    let id = Uuid::now_v7();
    let mut tx = core.db.begin().await?;
    let inserted = sqlx::query!(
        "INSERT INTO users (id, display_name, email, oidc_issuer, oidc_subject)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        display,
        email,
        issuer,
        subject,
    )
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        return match CoreError::conflict_on_unique(e, "") {
            CoreError::Conflict(_) => {
                drop(tx);
                // The same person's first sign-in in another tab may have won.
                Ok(find_identity(core, issuer, subject)
                    .await?
                    .unwrap_or(Err("email_in_use")))
            }
            other => Err(other),
        };
    }
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(
                Category::Admin,
                "user.provisioned",
                Actor::system("oidc-login"),
                "user",
            )
            .object(id, Some(&display))
            .change("email", None::<String>, email.to_owned())
            .change("oidc_subject", None::<String>, subject.to_owned())
            .context("source", "oidc_first_login"),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(Ok((id, display)))
}

async fn find_identity(
    core: &Core,
    issuer: &str,
    subject: &str,
) -> CoreResult<Option<Result<(Uuid, String), &'static str>>> {
    let u = sqlx::query!(
        r#"SELECT id, display_name, status AS "status: UserStatus"
             FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2"#,
        issuer,
        subject,
    )
    .fetch_optional(&core.db)
    .await?;
    Ok(u.map(|u| {
        if u.status == UserStatus::Active {
            Ok((u.id, u.display_name))
        } else {
            Err("inactive")
        }
    }))
}

/// Whether an account other than this identity already has `email`.
async fn email_taken(
    core: &Core,
    issuer: &str,
    subject: &str,
    email: Option<&str>,
) -> CoreResult<bool> {
    let Some(email) = email else {
        return Ok(false);
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM users
                WHERE lower(email) = lower($1)
                  AND (oidc_issuer IS DISTINCT FROM $2 OR oidc_subject IS DISTINCT FROM $3)
           ) AS "taken!""#,
        email,
        issuer,
        subject,
    )
    .fetch_one(&core.db)
    .await?)
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
    fn token_names_are_trimmed_truncated_or_absent() {
        assert_eq!(
            display_name(Some("  Ada Lovelace ")).as_deref(),
            Some("Ada Lovelace")
        );
        assert_eq!(display_name(Some("")), None);
        assert_eq!(display_name(Some(" \t ")), None);
        assert_eq!(display_name(None), None);
        let long = format!("{}{}", "a".repeat(199), "é".repeat(5));
        let cut = display_name(Some(&long)).unwrap();
        assert_eq!(cut.chars().count(), 200);
        assert!(cut.ends_with('é'), "cut on a character boundary");
    }

    #[test]
    fn decodes_jwt_payload() {
        let p = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"oid":"abc"}"#);
        let v = decode_jwt_payload(&format!("h.{p}.s")).unwrap();
        assert_eq!(v["oid"], "abc");
    }
}
