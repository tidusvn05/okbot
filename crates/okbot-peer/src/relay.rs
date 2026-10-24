//! Relays (ADR 0014): a member behind NAT (`relay = "<member>"`) polls its relay over HTTPS; the
//! relay hands it requests addressed to it and passes its signed answers back.
//!
//! Requests and answers stay signed end to end: the relay forwards bytes and cannot forge either
//! side (it can read them). Three okbot methods on the relay's A2A endpoint:
//!
//! - `okbot/relay.forward` (any member): `{to, authorization, body}` → `{status, signature, body}`;
//! - `okbot/relay.poll` (a member relayed by this bot): → `{requests: [{id, authorization, body}]}`;
//! - `okbot/relay.reply` (the same member): `{id, status, signature, body}`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use okbot_core::{CancellationToken, PeerError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use crate::a2a::{self, code};
use crate::node::{Inner, Team, lock};

/// How long a poll waits for a request.
const POLL_WAIT: Duration = Duration::from_secs(20);
/// How long a forwarded request waits for its answer.
const FORWARD_WAIT: Duration = Duration::from_secs(25);
/// Requests queued for one member at most.
const MAX_QUEUED: usize = 64;

/// A request waiting for a relayed member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Item {
    pub(crate) id: String,
    pub(crate) authorization: String,
    pub(crate) body: String,
}

/// An answer from a relayed member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    #[serde(default)]
    pub(crate) signature: Option<String>,
    pub(crate) body: String,
}

/// Requests waiting for one relayed member, and the poll to wake.
type Queue = (VecDeque<Item>, Arc<Notify>);

/// The relay's queues, by `(team, member)`.
#[derive(Default)]
pub(crate) struct Relay {
    queues: std::sync::Mutex<HashMap<(String, String), Queue>>,
    waiting: std::sync::Mutex<HashMap<String, oneshot::Sender<Answer>>>,
}

impl Relay {
    fn notify(&self, key: &(String, String)) -> Arc<Notify> {
        lock(&self.queues).entry(key.clone()).or_default().1.clone()
    }
}

type Refusal = (i64, String);

fn refused(message: impl Into<String>) -> Refusal {
    (code::REFUSED, message.into())
}

/// The relay methods; `None` when `method` is not one of them.
pub(crate) async fn handle(
    inner: &Arc<Inner>,
    team: &Team,
    caller: &str,
    method: &str,
    params: Value,
) -> Option<Result<Value, Refusal>> {
    let result = match method {
        "okbot/relay.forward" => forward(inner, team, caller, params).await,
        "okbot/relay.poll" => poll(inner, team, caller).await,
        "okbot/relay.reply" => reply(inner, team, caller, params),
        _ => return None,
    };
    Some(result)
}

fn relayed_by_me(team: &Team, member: &str) -> bool {
    team.manifest()
        .members
        .get(member)
        .and_then(|m| m.relay.as_deref())
        == Some(team.config.member.as_str())
}

async fn forward(
    inner: &Inner,
    team: &Team,
    caller: &str,
    params: Value,
) -> Result<Value, Refusal> {
    #[derive(Deserialize)]
    struct Forward {
        to: String,
        authorization: String,
        body: String,
    }
    let Ok(f) = serde_json::from_value::<Forward>(params) else {
        return Err((code::INVALID_PARAMS, "invalid forward".into()));
    };
    if !relayed_by_me(team, &f.to) {
        return Err(refused(format!("this bot does not relay `{}`", f.to)));
    }
    if !team.manifest().may_call(caller, &f.to) {
        return Err(refused(format!("no link lets `{caller}` call `{}`", f.to)));
    }
    let key = (team.config.id.clone(), f.to.clone());
    let id = hex::encode(rand::random::<[u8; 12]>());
    let (tx, rx) = oneshot::channel();
    {
        let mut queues = lock(&inner.relay.queues);
        let (queue, notify) = queues.entry(key).or_default();
        if queue.len() >= MAX_QUEUED {
            return Err(refused(format!("`{}` has too many requests waiting", f.to)));
        }
        queue.push_back(Item {
            id: id.clone(),
            authorization: f.authorization,
            body: f.body,
        });
        lock(&inner.relay.waiting).insert(id.clone(), tx);
        notify.notify_one();
    }
    let answer = tokio::time::timeout(FORWARD_WAIT, rx).await;
    lock(&inner.relay.waiting).remove(&id);
    match answer {
        Ok(Ok(answer)) => Ok(serde_json::to_value(answer).expect("answers serialize")),
        _ => {
            // Not taken in time: drop it, so a late poll does not run a request nobody awaits.
            let mut queues = lock(&inner.relay.queues);
            if let Some((queue, _)) = queues.get_mut(&(team.config.id.clone(), f.to.clone())) {
                queue.retain(|i| i.id != id);
            }
            Err(refused(format!(
                "`{}` did not answer through the relay",
                f.to
            )))
        }
    }
}

async fn poll(inner: &Arc<Inner>, team: &Team, caller: &str) -> Result<Value, Refusal> {
    if !relayed_by_me(team, caller) {
        return Err(refused("this bot is not your relay"));
    }
    let key = (team.config.id.clone(), caller.to_string());
    let notify = inner.relay.notify(&key);
    let deadline = tokio::time::Instant::now() + POLL_WAIT;
    loop {
        let notified = notify.notified();
        let taken: Vec<Item> = lock(&inner.relay.queues)
            .get_mut(&key)
            .map(|(q, _)| q.drain(..).collect())
            .unwrap_or_default();
        if !taken.is_empty() {
            return Ok(json!({"requests": taken}));
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return Ok(json!({"requests": []}));
        }
    }
}

fn reply(inner: &Arc<Inner>, team: &Team, caller: &str, params: Value) -> Result<Value, Refusal> {
    #[derive(Deserialize)]
    struct Reply {
        id: String,
        #[serde(flatten)]
        answer: Answer,
    }
    if !relayed_by_me(team, caller) {
        return Err(refused("this bot is not your relay"));
    }
    let Ok(r) = serde_json::from_value::<Reply>(params) else {
        return Err((code::INVALID_PARAMS, "invalid reply".into()));
    };
    if let Some(tx) = lock(&inner.relay.waiting).remove(&r.id) {
        let _ = tx.send(r.answer);
    }
    Ok(json!({}))
}

/// Sends a request to a relayed member through its relay.
pub(crate) async fn send(
    inner: &Inner,
    team: &Team,
    relay: &str,
    to: &str,
    authorization: &str,
    body: &[u8],
) -> Result<(u16, Option<String>, Vec<u8>), PeerError> {
    let manifest = team.manifest();
    let target = manifest
        .members
        .get(to)
        .ok_or_else(|| PeerError::NoRoute(to.to_string()))?;
    let box_key = crate::node::member_box_key(target)
        .map_err(|e| PeerError::Unreachable(format!("`{to}`: {e}")))?;
    // Sealed for the target: the relay forwards bytes it cannot read.
    let sealed = B64.encode(crate::seal::seal(&box_key, body));
    let params = json!({"to": to, "authorization": authorization, "body": sealed});
    let value = if relay == team.config.member {
        // This bot is the relay: queue it here.
        forward(inner, team, &team.config.member, params)
            .await
            .map_err(|(_, message)| PeerError::Refused(message))?
    } else {
        let request = a2a::Request::new("okbot/relay.forward", params);
        inner.call(team, relay, &request).await?
    };
    let answer: Answer = serde_json::from_value(value)
        .map_err(|_| PeerError::Refused(format!("`{relay}` sent an invalid relay answer")))?;
    let body = B64
        .decode(&answer.body)
        .ok()
        .and_then(|sealed| crate::seal::open(&inner.box_key, &sealed).ok())
        .ok_or_else(|| {
            PeerError::Refused(format!(
                "`{to}`'s answer through the relay is not sealed for this bot"
            ))
        })?;
    Ok((answer.status, answer.signature, body))
}

/// Runs a sealed request that came through the relay and seals the answer for its sender.
async fn run_sealed(
    inner: &Arc<Inner>,
    team_id: &str,
    item: &Item,
) -> (u16, Option<String>, String) {
    let refused = |status: u16, text: &str| (status, None, text.to_string());
    let Some(plain) = B64
        .decode(&item.body)
        .ok()
        .and_then(|sealed| crate::seal::open(&inner.box_key, &sealed).ok())
    else {
        return refused(400, "requests through a relay must be sealed for this bot");
    };
    let Some(caller_key) = item
        .authorization
        .strip_prefix(crate::node::SCHEME)
        .and_then(|t| crate::token::peek(t).ok())
        .and_then(|claims| {
            let manifest = inner.teams.get(team_id)?.manifest();
            crate::node::member_box_key(manifest.members.get(&claims.iss)?).ok()
        })
    else {
        return refused(401, "the caller has no box_key to answer to");
    };
    let reply = crate::inbound::handle(
        inner,
        team_id,
        "POST",
        Some(&item.authorization),
        None,
        &plain,
    )
    .await;
    let sealed = B64.encode(crate::seal::seal(&caller_key, &reply.body));
    (reply.status, reply.signature, sealed)
}

/// For each team where this bot is reached through a relay: polls it and answers what comes.
pub(crate) async fn run(inner: Arc<Inner>, stop: CancellationToken) {
    let mut tasks = tokio::task::JoinSet::new();
    for id in inner.teams.keys().cloned() {
        let inner = inner.clone();
        let stop = stop.clone();
        tasks.spawn(async move { poll_loop(inner, id, stop).await });
    }
    while tasks.join_next().await.is_some() {}
}

async fn poll_loop(inner: Arc<Inner>, team_id: String, stop: CancellationToken) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let team = &inner.teams[&team_id];
        let relay = team
            .manifest()
            .members
            .get(&team.config.member)
            .and_then(|m| m.relay.clone());
        let Some(relay) = relay else {
            // Not relayed (now): look again after the next manifest refresh.
            tokio::select! {
                () = tokio::time::sleep(crate::node::REFRESH) => continue,
                () = stop.cancelled() => return,
            }
        };
        let request = a2a::Request::new("okbot/relay.poll", json!({}));
        let polled = tokio::select! {
            r = inner.call(team, &relay, &request) => r,
            () = stop.cancelled() => return,
        };
        let items = match polled {
            Ok(value) => {
                backoff = Duration::from_secs(1);
                serde_json::from_value::<Vec<Item>>(value["requests"].clone()).unwrap_or_default()
            }
            Err(error) => {
                tracing::warn!(team = %team_id, %relay, %error, "cannot poll the relay");
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    () = stop.cancelled() => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        for item in items {
            let inner = inner.clone();
            let team_id = team_id.clone();
            let relay = relay.clone();
            tokio::spawn(async move {
                let (status, signature, body) = run_sealed(&inner, &team_id, &item).await;
                let request = a2a::Request::new(
                    "okbot/relay.reply",
                    json!({"id": item.id, "status": status, "signature": signature, "body": body}),
                );
                let team = &inner.teams[&team_id];
                if let Err(error) = inner.call(team, &relay, &request).await {
                    tracing::warn!(%error, "cannot pass an answer back through the relay");
                }
            });
        }
    }
}
