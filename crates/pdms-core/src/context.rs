// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! The acting party for a service call: authorization subject plus audit actor.

use std::net::IpAddr;

use pdms_audit::{Actor, AuthMethod};
use pdms_authz::{Capability, Principal, has_capability};
use uuid::Uuid;

use crate::error::{CoreError, CoreResult};

/// Request metadata captured by the web layer.
#[derive(Clone, Debug, Default)]
pub struct ClientInfo {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug)]
enum Who {
    User(Box<Principal>),
    /// Trusted internal callers: CLI maintenance, scheduler, migrations.
    System,
}

/// Every mutating service call takes a `Ctx`, which supplies both the
/// authorization principal and the audit actor.
#[derive(Clone, Debug)]
pub struct Ctx {
    who: Who,
    pub actor: Actor,
}

impl Ctx {
    pub fn user(
        principal: Principal,
        display_name: &str,
        auth_method: AuthMethod,
        client: &ClientInfo,
    ) -> Ctx {
        Ctx {
            actor: Actor {
                user_id: Some(principal.user_id),
                display: Some(display_name.to_owned()),
                auth_method,
                ip: client.ip,
                user_agent: client.user_agent.clone(),
                api_token_id: None,
                request_id: client.request_id.clone(),
            },
            who: Who::User(Box::new(principal)),
        }
    }

    pub fn system(label: &str) -> Ctx {
        Ctx {
            who: Who::System,
            actor: Actor::system(label),
        }
    }

    pub fn principal(&self) -> Option<&Principal> {
        match &self.who {
            Who::User(p) => Some(p),
            Who::System => None,
        }
    }

    pub fn user_id(&self) -> Option<Uuid> {
        self.principal().map(|p| p.user_id)
    }

    pub fn is_system(&self) -> bool {
        matches!(self.who, Who::System)
    }

    pub fn can(&self, cap: Capability) -> bool {
        match &self.who {
            Who::User(p) => has_capability(p, cap),
            Who::System => true,
        }
    }

    pub fn require(&self, cap: Capability) -> CoreResult<()> {
        if self.can(cap) {
            Ok(())
        } else {
            Err(CoreError::Forbidden(None))
        }
    }
}
