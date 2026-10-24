//! Delegating work to other members: the [`Peers`] port.

use std::sync::Arc;
use std::time::{Duration, Instant};

use okbot_core::{
    ConversationKey, Delegation, PeerError, PeerInfo, PeerPrompt, PeerTask, PeerTaskRecord,
    PeerTaskState, Peers, async_trait,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::a2a::{self, Part};
use crate::node::{Inner, Team, lock};

/// Polling a task that runs: first and longest interval.
const POLL_FIRST: Duration = Duration::from_millis(500);
const POLL_MAX: Duration = Duration::from_secs(3);

/// A bot's peer side: [`Peers`] for delegating, a [`Channel`](okbot_core::Channel) per team for
/// tasks it receives.
#[derive(Clone)]
pub struct Node {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("teams", &self.inner.teams.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Node {
    /// A node for `config`; file manifests are read now.
    pub fn new(
        config: crate::NodeConfig,
        store: Arc<dyn okbot_core::ConversationStore>,
        clock: Arc<dyn okbot_core::Clock>,
    ) -> Result<Self, crate::Error> {
        Ok(Self {
            inner: Arc::new(Inner::new(config, store, clock)?),
        })
    }

    /// Fetches the manifests of URL teams, marks tasks left by a stop as failed, and returns
    /// problems worth a warning (this bot missing from a team, a manifest that cannot be read).
    pub async fn start(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for team in self.inner.teams.values() {
            if team.signed()
                && let Err(error) = self.inner.refresh(team).await
            {
                problems.push(format!("team `{}`: {error}", team.config.id));
            }
        }
        problems.extend(self.inner.check_membership());
        match self
            .inner
            .store
            .interrupt_peer_tasks(self.inner.now())
            .await
        {
            Ok(0) => {}
            Ok(n) => tracing::warn!(
                tasks = n,
                "peer tasks interrupted by the last stop were marked failed"
            ),
            Err(error) => tracing::warn!(%error, "cannot mark interrupted peer tasks"),
        }
        problems
    }

    /// One `peer:<team>` channel per team: it serves the team's endpoint (tasks when the team's
    /// `accept` allows them, relaying, refusals that say why) and the bot's socket.
    pub fn channels(&self) -> Vec<Arc<dyn okbot_core::Channel>> {
        self.inner
            .teams
            .values()
            .map(|t| {
                Arc::new(crate::channel::PeerChannel::new(
                    self.inner.clone(),
                    &t.config.id,
                )) as Arc<dyn okbot_core::Channel>
            })
            .collect()
    }

    /// Checks each team without changing anything: its manifest (fetched for URL teams), this
    /// bot's membership, and whether the members it may call are reachable. One line per finding,
    /// with `true` when it is fine.
    pub async fn check(&self) -> Vec<(String, bool, String)> {
        let mut out = Vec::new();
        for team in self.inner.teams.values() {
            let label = format!("team {}", team.config.id);
            if let Err(error) = self.inner.refresh(team).await {
                out.push((label, false, error.to_string()));
                continue;
            }
            let manifest = team.manifest();
            let problems: Vec<String> = self
                .inner
                .check_membership()
                .into_iter()
                .filter(|p| p.starts_with(&format!("team `{}`", team.config.id)))
                .collect();
            if !problems.is_empty() {
                out.push((label, false, problems.join("; ")));
                continue;
            }
            let callees = manifest.callees(&team.config.member);
            out.push((
                label.clone(),
                true,
                format!(
                    "member `{}`, version {}{}; may call {}; {}",
                    team.config.member,
                    manifest.team.version,
                    manifest
                        .team
                        .expires
                        .map(|e| format!(" (expires {e})"))
                        .unwrap_or_default(),
                    if callees.is_empty() {
                        "nobody".into()
                    } else {
                        callees.join(", ")
                    },
                    if team.config.accept {
                        "accepts tasks"
                    } else {
                        "accepts no tasks"
                    },
                ),
            ));
            for name in callees {
                let member = &manifest.members[name];
                let (ok, detail) = match (&member.dir, &member.url, &member.relay) {
                    (Some(dir), _, _) => {
                        let socket = dir.join("state").join(crate::node::SOCKET_FILE);
                        if socket.exists() {
                            (true, format!("socket {}", socket.display()))
                        } else {
                            (
                                false,
                                format!("{} is missing: is that bot running?", socket.display()),
                            )
                        }
                    }
                    (_, Some(url), _) => match self.inner.http.get(url).send().await {
                        // Without a token the endpoint answers 401: it is there.
                        Ok(r) if r.status().as_u16() == 401 => (true, url.clone()),
                        Ok(r) => (
                            false,
                            format!("{url} answered HTTP {}", r.status().as_u16()),
                        ),
                        Err(e) => (false, format!("{url}: {}", e.without_url())),
                    },
                    (_, _, Some(relay)) => (true, format!("through `{relay}`")),
                    _ => (false, "no way to reach it".into()),
                };
                out.push((format!("{label} → {name}"), ok, detail));
            }
        }
        out
    }

    /// The team ids.
    pub fn teams(&self) -> Vec<String> {
        self.inner.teams.keys().cloned().collect()
    }
}

/// A task from a `GetTask`/`CancelTask` result or a send result, in either version.
pub(crate) fn task_from(team: &str, peer: &str, value: &Value) -> Result<PeerTask, PeerError> {
    let task = a2a::Task::from_wire(a2a::unwrap_send_result(value))
        .ok_or_else(|| PeerError::Refused(format!("`{peer}` sent an invalid task")))?;
    let state = task.state;
    let message = task.message.as_ref();
    let text = message.map(a2a::Message::text).filter(|t| !t.is_empty());
    let prompt = message
        .and_then(a2a::Message::data)
        .filter(|_| state == PeerTaskState::InputRequired)
        .and_then(|data| {
            let id = data.get("prompt")?.as_str()?.to_string();
            let buttons = data
                .get("buttons")?
                .as_array()?
                .iter()
                .filter_map(|b| {
                    Some((
                        b.get("id")?.as_str()?.to_string(),
                        b.get("label")?.as_str()?.to_string(),
                    ))
                })
                .collect();
            Some(PeerPrompt { id, buttons })
        });
    let tokens = message
        .and_then(|m| m.metadata.as_ref())
        .and_then(|m| m.pointer(&format!("/{}/tokens", a2a::USAGE_KEY.replace('/', "~1"))))
        .and_then(Value::as_u64);
    Ok(PeerTask {
        team: team.to_string(),
        peer: peer.to_string(),
        id: task.id,
        state,
        text,
        prompt,
        tokens,
    })
}

impl Node {
    fn team(&self, id: &str) -> Result<&Team, PeerError> {
        self.inner
            .teams
            .get(id)
            .ok_or_else(|| PeerError::NoRoute(format!("no team `{id}`")))
    }

    async fn record(&self, task: &PeerTask, delegation: Option<(&Delegation, &str)>) {
        let now = self.inner.now();
        let existing = self
            .inner
            .store
            .peer_task(false, &task.id)
            .await
            .ok()
            .flatten();
        let record = match (existing, delegation) {
            (Some(old), _) => PeerTaskRecord {
                state: task.state,
                text: task.text.clone(),
                tokens: task.tokens.or(old.tokens),
                updated_at: now,
                ..old
            },
            (None, Some((d, context_id))) => PeerTaskRecord {
                id: task.id.clone(),
                inbound: false,
                team: task.team.clone(),
                peer: task.peer.clone(),
                context_id: context_id.to_string(),
                message_id: d.message_id.clone(),
                state: task.state,
                conversation: d
                    .context
                    .origin
                    .conversation
                    .parse::<ConversationKey>()
                    .ok(),
                trace: d.context.trace.clone(),
                text: task.text.clone(),
                requester: d.requester.clone(),
                direct: d.direct,
                tokens: task.tokens,
                created_at: now,
                updated_at: now,
            },
            (None, None) => return,
        };
        if let Err(error) = self.inner.store.put_peer_task(record).await {
            tracing::warn!(%error, "cannot record a delegated task");
        }
    }

    async fn get(&self, team: &Team, peer: &str, id: &str) -> Result<PeerTask, PeerError> {
        let request = a2a::Request::new(
            a2a::method(a2a::Op::Get, a2a::Version::V10),
            json!({"id": id}),
        );
        let value = self.inner.call(team, peer, &request).await?;
        task_from(&team.config.id, peer, &value)
    }

    /// Polls until the task ends, asks for input, or `until`.
    async fn wait(&self, team: &Team, mut task: PeerTask, until: Instant) -> PeerTask {
        let mut every = POLL_FIRST;
        while !task.state.is_final() && task.state != PeerTaskState::InputRequired {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            tokio::time::sleep(every.min(left)).await;
            every = (every * 2).min(POLL_MAX);
            match self.get(team, &task.peer, &task.id).await {
                Ok(next) => task = next,
                Err(error) => {
                    tracing::debug!(%error, "polling a delegated task failed");
                }
            }
        }
        task
    }
}

#[async_trait]
impl Peers for Node {
    async fn directory(&self) -> Result<Vec<PeerInfo>, PeerError> {
        let mut out = Vec::new();
        for team in self.inner.teams.values() {
            let manifest = team.manifest();
            for name in manifest.callees(&team.config.member) {
                let member = &manifest.members[name];
                out.push(PeerInfo {
                    team: team.config.id.clone(),
                    name: name.to_string(),
                    roles: member.roles.clone(),
                    skills: member.skills.clone(),
                    description: member.description.clone(),
                });
            }
        }
        Ok(out)
    }

    async fn delegate(&self, delegation: Delegation) -> Result<PeerTask, PeerError> {
        let started = Instant::now();
        let mut candidates = Vec::new();
        for team in self.inner.teams.values() {
            if delegation
                .team
                .as_ref()
                .is_some_and(|t| *t != team.config.id)
            {
                continue;
            }
            let manifest = team.manifest();
            for member in manifest.route(&team.config.member, &delegation.to) {
                candidates.push((team, member.to_string()));
            }
        }
        if candidates.is_empty() {
            return Err(PeerError::NoRoute(delegation.to.to_string()));
        }
        // Members that just failed go last.
        candidates.sort_by_key(|(team, member)| self.inner.is_down(&team.config.id, member));
        let mut last = None;
        for (team, member) in candidates {
            let mut context = delegation.context.clone();
            if context.origin.bot.is_empty() {
                context.origin.bot.clone_from(&team.config.member);
            }
            context.chain.push(team.config.member.clone());
            let own = self.inner.now()
                + jiff::SignedDuration::try_from(team.config.task_timeout)
                    .unwrap_or(jiff::SignedDuration::from_secs(600));
            context.deadline = context.deadline.min(own);
            // One A2A context per origin conversation and member: the other bot keeps the history
            // of what this conversation asked it.
            let context_id = hex::encode(Sha256::digest(format!(
                "{}\n{}",
                context.origin.conversation, team.config.member
            )))[..24]
                .to_string();
            let message = a2a::Message::user(
                &delegation.message_id,
                &context_id,
                vec![Part::Text(delegation.task.clone())],
            )
            .with_context(&context);
            let request = a2a::Request::new(
                a2a::method(a2a::Op::Send, a2a::Version::V10),
                json!({"message": message.to_wire(a2a::Version::V10)}),
            );
            match self.inner.call(team, &member, &request).await {
                Ok(value) => {
                    let task = task_from(&team.config.id, &member, &value)?;
                    tracing::info!(team = %team.config.id, %member, task = %task.id, trace = %context.trace, "task delegated");
                    self.record(&task, Some((&delegation, &context_id))).await;
                    let task = self.wait(team, task, started + delegation.wait).await;
                    self.record(&task, None).await;
                    return Ok(task);
                }
                Err(error) => {
                    tracing::warn!(team = %team.config.id, %member, %error, "delegation failed");
                    last = Some(error);
                }
            }
        }
        Err(last.unwrap_or_else(|| PeerError::NoRoute(delegation.to.to_string())))
    }

    async fn status(&self, team: &str, peer: &str, task: &str) -> Result<PeerTask, PeerError> {
        let team = self.team(team)?;
        let task = self.get(team, peer, task).await?;
        self.record(&task, None).await;
        Ok(task)
    }

    async fn cancel(&self, team: &str, peer: &str, task: &str) -> Result<PeerTask, PeerError> {
        let team = self.team(team)?;
        let request = a2a::Request::new(
            a2a::method(a2a::Op::Cancel, a2a::Version::V10),
            json!({"id": task}),
        );
        let value = self.inner.call(team, peer, &request).await?;
        let task = task_from(&team.config.id, peer, &value)?;
        self.record(&task, None).await;
        Ok(task)
    }

    async fn report_usage(&self, conversation: &ConversationKey, task: Option<&str>, tokens: u64) {
        crate::inbound::usage(&self.inner, conversation, task, tokens);
    }

    async fn fail(&self, conversation: &ConversationKey, task: Option<&str>, reason: &str) {
        crate::inbound::fail(&self.inner, conversation, task, reason).await;
    }

    async fn answer(
        &self,
        task: &PeerTask,
        prompt: &str,
        button: &str,
    ) -> Result<PeerTask, PeerError> {
        let team = self.team(&task.team)?;
        let mut message = a2a::Message::user(
            &hex::encode(rand::random::<[u8; 8]>()),
            "",
            vec![Part::Data(json!({"prompt": prompt, "button": button}))],
        );
        message.context_id = None;
        message.task_id = Some(task.id.clone());
        let request = a2a::Request::new(
            a2a::method(a2a::Op::Send, a2a::Version::V10),
            json!({"message": message.to_wire(a2a::Version::V10)}),
        );
        let value = self.inner.call(team, &task.peer, &request).await?;
        let next = task_from(&team.config.id, &task.peer, &value)?;
        self.record(&next, None).await;
        Ok(next)
    }

    async fn caller(&self, conversation: &ConversationKey) -> Option<okbot_core::PeerCaller> {
        lock(&self.inner.callers).get(conversation).cloned()
    }

    fn team_person(&self, account: &str) -> Option<String> {
        self.inner
            .teams
            .values()
            .find_map(|t| t.manifest().person_of(account).map(str::to_string))
    }

    fn member_name(&self, team: &str) -> Option<String> {
        self.inner.teams.get(team).map(|t| t.config.member.clone())
    }
}
