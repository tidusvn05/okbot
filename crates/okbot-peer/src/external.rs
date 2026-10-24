//! A2A agents and clients that are not okbots (ADR 0014), as the built-in team `external`.
//!
//! - Agents (`[[a2a.agent]]`) are members this bot may delegate to: plain A2A over HTTPS with an
//!   optional bearer token, in the version they speak. They get the task text only: no delegation
//!   context, no person, no okbot signature.
//! - Clients (`[[a2a.client]]`) may give this bot tasks at `/a2a/external` with a bearer token.
//!   Their tasks start a chain of one (they cannot be delegated further), act for the configured
//!   person if any, and get the client's roles intersected with that person's.

use std::time::Duration;

use okbot_core::{Budget, DelegationContext, Origin, PeerError, Secret};
use serde_json::Value;

use crate::a2a;
use crate::manifest::{Link, Manifest, Member};

/// The id of the built-in team.
pub const TEAM: &str = "external";

/// An A2A agent this bot may delegate to.
#[derive(Debug, Clone)]
pub struct ExternalAgent {
    /// Its name here.
    pub name: String,
    /// Its JSON-RPC endpoint.
    pub url: String,
    /// The bearer token it expects.
    pub token: Option<Secret<String>>,
    /// The A2A version it speaks.
    pub version: a2a::Version,
    /// What it does.
    pub description: String,
    /// Skills.
    pub skills: Vec<String>,
}

/// A program that may give this bot tasks.
#[derive(Debug, Clone)]
pub struct ExternalClient {
    /// Its name.
    pub name: String,
    /// The bearer token it presents.
    pub token: Secret<String>,
    /// The person (`[people]` id) it acts for.
    pub person: Option<String>,
}

/// `[a2a]`.
#[derive(Debug, Clone, Default)]
pub struct ExternalConfig {
    /// Agents.
    pub agents: Vec<ExternalAgent>,
    /// Clients.
    pub clients: Vec<ExternalClient>,
    /// Tasks at once per client.
    pub max_concurrent: u32,
    /// The longest a client's task may take, and a delegation to an agent.
    pub task_timeout: Duration,
}

impl ExternalConfig {
    /// The built-in team's manifest: this bot, the agents it may call, the clients that may call
    /// it (each client's team role is its name, for `[a2a.client] roles`).
    pub(crate) fn manifest(&self, me: &str) -> Manifest {
        let mut m = Manifest::new(TEAM);
        m.team.version = 1;
        m.members.insert(me.to_string(), Member::default());
        for agent in &self.agents {
            m.members.insert(
                agent.name.clone(),
                Member {
                    url: Some(agent.url.clone()),
                    skills: agent.skills.clone(),
                    description: agent.description.clone(),
                    ..Member::default()
                },
            );
        }
        for client in &self.clients {
            m.members.insert(
                client.name.clone(),
                Member {
                    roles: vec![client.name.clone()],
                    ..Member::default()
                },
            );
        }
        if !self.clients.is_empty() {
            m.links.push(Link {
                from: self.clients.iter().map(|c| c.name.clone()).collect(),
                to: vec![me.to_string()],
            });
        }
        if !self.agents.is_empty() {
            m.links.push(Link {
                from: vec![me.to_string()],
                to: self.agents.iter().map(|a| a.name.clone()).collect(),
            });
        }
        m
    }

    /// The client whose token is `bearer` (compared in constant time).
    pub(crate) fn client(&self, bearer: &str) -> Option<&ExternalClient> {
        self.clients.iter().find(|c| same(c.token.expose(), bearer))
    }

    pub(crate) fn agent(&self, name: &str) -> Option<&ExternalAgent> {
        self.agents.iter().find(|a| a.name == name)
    }
}

/// Equal without an early exit on the first differing byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// The context of a client's task: a chain of one, for the client's person, that cannot go
/// further.
pub(crate) fn context(
    client: &ExternalClient,
    now: jiff::Timestamp,
    timeout: Duration,
) -> DelegationContext {
    DelegationContext {
        origin: Origin {
            bot: client.name.clone(),
            conversation: String::new(),
            person: client.person.clone(),
            name: client.name.clone(),
        },
        chain: vec![client.name.clone()],
        deadline: now
            + jiff::SignedDuration::try_from(timeout)
                .unwrap_or(jiff::SignedDuration::from_secs(600)),
        budget: Budget {
            turns: 1,
            tokens: None,
        },
        trace: hex::encode(rand::random::<[u8; 8]>()),
    }
}

/// Sends one JSON-RPC request to an agent.
pub(crate) async fn call(
    http: &reqwest::Client,
    agent: &ExternalAgent,
    request: &a2a::Request,
) -> Result<Value, PeerError> {
    let mut post = http
        .post(&agent.url)
        .header("content-type", "application/json")
        .header(a2a::VERSION_HEADER, agent.version.header())
        .json(request);
    if let Some(token) = &agent.token {
        post = post.bearer_auth(token.expose());
    }
    let reply = post.send().await.map_err(|e| {
        PeerError::Unreachable(if e.is_timeout() {
            "timed out".into()
        } else {
            e.without_url().to_string()
        })
    })?;
    let status = reply.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(PeerError::Refused(format!(
            "`{}` answered HTTP {status}",
            agent.name
        )));
    }
    reply
        .json()
        .await
        .map_err(|_| PeerError::Refused(format!("`{}` sent invalid JSON", agent.name)))
}
