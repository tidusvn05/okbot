//! One agent turn: context, agent run, rendering, sending, audit.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use okbot_core::{
    AgentRunRecord, AgentTurn, CancellationToken, InboundEvent, KnowledgeContext, MessageRole,
    NewMessage, Requester, RunGrant, RunId, SentRef,
};
use sha2::{Digest, Sha256};
use tracing::Instrument;

use crate::context::{self, PromptInput};
use crate::dispatch::target;
use crate::{Shared, Tracked};

/// Renders `markdown` for the event's channel and sends it as a reply. Failures are logged.
pub(crate) async fn reply(shared: &Shared, event: &InboundEvent, markdown: &str) -> bool {
    reply_over(shared, event, markdown, None).await
}

/// Like [`reply`], but the first part replaces `placeholder` (a "thinking…" message) when given.
pub(crate) async fn reply_over(
    shared: &Shared,
    event: &InboundEvent,
    markdown: &str,
    placeholder: Option<&SentRef>,
) -> bool {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        tracing::error!(channel = %event.conversation.channel, "reply to unknown channel");
        return false;
    };
    let to = target(event);
    let mut placeholder = placeholder;
    for part in shared.renderer.render(markdown, channel.caps()) {
        let result = match placeholder.take() {
            Some(sent) => channel.edit(sent, &part).await,
            None => channel.send(&to, &part).await.map(drop),
        };
        if let Err(error) = result {
            tracing::error!(channel = %channel.id(), %error, "sending a reply failed");
            return false;
        }
    }
    true
}

/// Renders `markdown` and sends it to `target` (no reply threading beyond the target's own).
pub(crate) async fn send_to(
    shared: &Shared,
    target: &okbot_core::ReplyTarget,
    markdown: &str,
) -> Result<(), okbot_core::ChannelError> {
    let Some(channel) = shared.channels.get(&target.conversation.channel) else {
        return Err(okbot_core::ChannelError::Unsupported("channel"));
    };
    for part in shared.renderer.render(markdown, channel.caps()) {
        channel.send(target, &part).await?;
    }
    Ok(())
}

fn run_id() -> RunId {
    RunId::new(hex::encode(rand::random::<[u8; 8]>()))
}

/// A stable, path-safe directory name for a conversation.
fn workdir(shared: &Shared, event: &InboundEvent) -> PathBuf {
    let digest = Sha256::digest(event.conversation.to_string().as_bytes());
    shared.config.work_dir.join(&hex::encode(digest)[..16])
}

/// Shows that the bot is working: a repeated typing indicator, or, on platforms that can edit
/// but not type (Google Chat), a "thinking…" message that the answer later replaces.
pub(crate) struct Typing(
    Option<tokio::task::JoinHandle<()>>,
    pub(crate) Option<SentRef>,
);

impl Typing {
    /// Shows the indicator once right away, then repeats it in the background.
    pub(crate) async fn start(shared: &Shared, event: &InboundEvent) -> Self {
        let Some(channel) = shared.channels.get(&event.conversation.channel).cloned() else {
            return Self(None, None);
        };
        if !channel.caps().typing {
            if !channel.caps().edit {
                return Self(None, None);
            }
            let thinking = shared
                .renderer
                .render(&shared.config.replies.thinking, channel.caps())
                .into_iter()
                .next();
            let sent = match thinking {
                Some(part) => channel.send(&target(event), &part).await.ok(),
                None => None,
            };
            return Self(None, sent);
        }
        let to = target(event);
        let every = shared.config.typing_interval;
        if let Err(error) = channel.typing(&to).await {
            tracing::debug!(%error, "typing indicator failed");
        }
        Self(
            Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    if let Err(error) = channel.typing(&to).await {
                        tracing::debug!(%error, "typing indicator failed");
                    }
                }
            })),
            None,
        )
    }
}

impl Drop for Typing {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// The knowledge servers plus the `[[mcp]]` servers this requester may use here (agents with
/// MCP only), reached through the gateway with a token bound to this turn (ADR 0011). OAuth
/// servers without a connected account are left out and named in `not_connected`.
async fn with_extra_mcp(
    shared: &Shared,
    grant: &RunGrant,
    event: &InboundEvent,
    run_id: RunId,
    person: String,
    personal: bool,
    restricted: bool,
) -> TurnMcp {
    let mut out = TurnMcp {
        servers: grant.mcp.clone(),
        env: grant.env.clone(),
        guard: None,
        not_connected: Vec::new(),
    };
    let Some(base) = shared.gateway.url().map(str::to_string) else {
        return out;
    };
    if !shared.agent.caps().mcp {
        return out;
    }
    let who = crate::permission::Who::new(shared.roles_of(event).await, event);
    let admin = who.admin();
    let mut servers: Vec<&crate::ExtraMcp> = Vec::new();
    let mut upstream = std::collections::HashMap::new();
    for m in shared
        .config
        .mcp
        .iter()
        .filter(|m| crate::permission::may_use(m, &who))
    {
        use crate::connect::Upstream;
        match crate::connect::upstream(shared, m, event, &person, personal, admin, None).await {
            Upstream::Token(token) => {
                upstream.insert(m.server.name.clone(), token);
                servers.push(m);
            }
            Upstream::NotConnected => out.not_connected.push(m.server.name.clone()),
            Upstream::NotHere | Upstream::Unavailable => {}
        }
    }
    // An automation's turn never delegates: its data is untrusted (ADR 0013).
    let team = !restricted && crate::team::offered(shared, &who).await;
    if servers.is_empty() && !team {
        return out;
    }
    let (token, guard) = shared.gateway.issue(crate::gateway::Grant {
        event: event.clone(),
        run_id,
        person,
        who,
        upstream,
        expires: std::time::Instant::now() + shared.config.agent_timeout,
        done: CancellationToken::new(),
        local: std::sync::Mutex::default(),
        restricted,
        team,
    });
    if team {
        out.servers.push(okbot_core::McpServer {
            name: crate::team::SERVER.into(),
            url: format!("{base}/mcp/{}", crate::team::SERVER),
            bearer_token_env: crate::team::TOKEN_ENV.into(),
        });
        out.env.push((crate::team::TOKEN_ENV.into(), token.clone()));
    }
    for m in servers {
        out.servers.push(okbot_core::McpServer {
            name: m.server.name.clone(),
            url: format!("{base}/mcp/{}", m.server.name),
            bearer_token_env: m.server.bearer_token_env.clone(),
        });
        out.env
            .push((m.server.bearer_token_env.clone(), token.clone()));
    }
    out.guard = Some(guard);
    out
}

/// A turn's MCP servers, the environment carrying their tokens, and the gateway grant that
/// ends with the turn.
struct TurnMcp {
    servers: Vec<okbot_core::McpServer>,
    env: Vec<(String, okbot_core::Secret<String>)>,
    guard: Option<crate::gateway::GrantGuard>,
    /// OAuth servers the requester could use here once connected.
    not_connected: Vec<String>,
}

/// The requester used up `[limits]` (administrators never do). A store error lets the turn run.
async fn over_limit(shared: &Shared, person: &str, admin: bool) -> bool {
    let limits = shared.config.limits;
    if admin || (limits.turns_per_day == 0 && limits.tokens_per_day == 0) {
        return false;
    }
    let since = shared.clock.now() - jiff::SignedDuration::from_hours(24);
    match shared.store.usage_since(person, since).await {
        Ok(usage) => {
            let over = (limits.turns_per_day > 0 && usage.turns >= u64::from(limits.turns_per_day))
                || (limits.tokens_per_day > 0 && usage.tokens >= limits.tokens_per_day);
            if over {
                tracing::info!(%person, "over the daily limit");
                shared.metrics.ignored();
            }
            over
        }
        Err(error) => {
            tracing::warn!(%error, "usage unavailable; the limit is not applied");
            false
        }
    }
}

/// Answers a batch of messages from one conversation.
pub(crate) async fn answer(shared: &Shared, batch: &[Tracked], cancel: &CancellationToken) {
    let Some((event, _)) = batch.last() else {
        return;
    };
    let span =
        tracing::info_span!("turn", conversation = %event.conversation, messages = batch.len());
    async {
        let admin = shared.is_admin(event).await;
        let person = shared.person_key(event).await;
        if over_limit(shared, &person, admin).await {
            if !crate::team::fail(shared, event, "the daily limit of this bot is reached").await {
                reply(shared, event, &shared.config.replies.limit_reached).await;
            }
            return;
        }
        answer_batch(shared, batch, event, cancel, admin, person).await;
    }
    .instrument(span)
    .await;
}

async fn answer_batch(
    shared: &Shared,
    batch: &[Tracked],
    event: &InboundEvent,
    cancel: &CancellationToken,
    admin: bool,
    person: String,
) {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        return;
    };
    let typing = Typing::start(shared, event).await;
    store_user_messages(shared, batch).await;

    let restricted = crate::automation::is_automation(event);
    let tools = tool_policy(shared, event, restricted);
    let requester = crate::command::requester(shared, event).await;
    let timeout = shared.config.agent_timeout;

    let (knowledge, grant, excerpts) =
        gather_knowledge(shared, batch, &requester, tools, admin, restricted).await;
    let run_id = run_id();
    let mcp = with_extra_mcp(
        shared,
        &grant,
        event,
        run_id.clone(),
        person.clone(),
        requester.personal,
        restricted,
    )
    .await;

    let history = history_for(shared, event, restricted).await;
    let peer_note = crate::team::prompt_note(shared, event).await;
    let now = shared.clock.now();
    let new_count = batch.len().min(history.len());
    let input = PromptInput {
        bot_name: &shared.config.bot_name,
        caps: channel.caps(),
        tools,
        knowledge: &knowledge,
        excerpts: excerpts.as_deref().filter(|e| !e.is_empty()),
        new_count,
        history: &history,
        event,
        now,
        timezone: shared.config.timezone.as_deref(),
        history_tokens: shared.config.history_tokens,
        personal: requester.personal,
        not_connected: &mcp.not_connected,
        peer: peer_note.as_deref(),
    };
    let prompt = context::build(&input);
    // An automation turn starts afresh and leaves no session behind, so its data never reaches
    // later turns.
    let resume = if restricted {
        None
    } else {
        resume_for(shared, event, &input, new_count).await
    };

    let (progress, live) = match &typing.1 {
        Some(placeholder) => {
            let (progress, live) = crate::live::Live::start(
                channel.clone(),
                shared.renderer.clone(),
                placeholder.clone(),
                shared.config.progress_interval,
            );
            (Some(progress), Some(live))
        }
        None => (None, None),
    };
    let turn = AgentTurn {
        system: prompt.system,
        prompt: prompt.prompt,
        tools,
        mcp: mcp.servers,
        env: mcp.env,
        workdir: workdir(shared, event),
        timeout,
        resume,
        progress,
    };
    let started = Instant::now();
    let outcome = tokio::select! {
        result = shared.agent.run(turn) => Some(result),
        () = cancel.cancelled() => None,
    };
    drop(grant);
    drop(mcp.guard);
    drop(live);
    if let Some(Ok(answer)) = &outcome {
        remember_session(shared, event, answer, restricted).await;
    }

    let record = AgentRunRecord {
        run_id,
        conversation: event.conversation.clone(),
        backend: shared.agent.name().to_string(),
        model: None,
        tools,
        usage: None,
        duration: started.elapsed(),
        error: None,
        started_at: now,
        person: Some(person),
    };
    finish(shared, event, outcome, record, typing.1.as_ref()).await;
}

/// The room's tool policy. An automation's data is untrusted: no web, so nothing can be sent
/// out through a URL.
fn tool_policy(shared: &Shared, event: &InboundEvent, restricted: bool) -> okbot_core::ToolPolicy {
    if restricted {
        return okbot_core::ToolPolicy::McpOnly;
    }
    let room = shared
        .config
        .room(&event.conversation.channel, &event.conversation.room);
    room.tool_policy.unwrap_or(shared.config.tool_policy)
}

/// Keeps the agent's session for the next turn. After an automation the session is emptied, so
/// the next turn sends the history again and the automation's data stays behind.
async fn remember_session(
    shared: &Shared,
    event: &InboundEvent,
    answer: &okbot_core::AgentReply,
    restricted: bool,
) {
    let Some(session) = &answer.session_id else {
        return;
    };
    let session = if restricted { "" } else { session.as_str() };
    if let Err(error) = shared
        .store
        .set_session(&event.conversation, &answer.backend, session)
        .await
    {
        tracing::warn!(%error, "cannot remember the agent session");
    }
}

/// The conversation's recent messages; for an automation, the last one with its data (the
/// history keeps a short line, and this turn alone sees the data).
async fn history_for(
    shared: &Shared,
    event: &InboundEvent,
    restricted: bool,
) -> Vec<okbot_core::StoredMessage> {
    let mut history = shared
        .store
        .recent(&event.conversation, shared.config.history_messages)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "history unavailable");
            Vec::new()
        });
    let line = crate::automation::history_line(&event.text);
    if restricted
        && let Some(stored) = history
            .iter_mut()
            .rev()
            .find(|m| m.role == okbot_core::MessageRole::User && m.text == line)
    {
        stored.text.clone_from(&event.text);
    }
    history
}

/// The stored session of this conversation with a prompt holding only the new messages.
async fn resume_for(
    shared: &Shared,
    event: &InboundEvent,
    input: &PromptInput<'_>,
    new_count: usize,
) -> Option<okbot_core::Resume> {
    if !shared.config.resume_sessions || !shared.agent.caps().resume {
        return None;
    }
    let (backend, session) = shared.store.session(&event.conversation).await.ok()??;
    if session.is_empty() {
        return None;
    }
    let new = &input.history[input.history.len() - new_count..];
    let only_new = context::build(&PromptInput {
        history: new,
        ..*input
    });
    Some(okbot_core::Resume {
        backend,
        session,
        prompt: only_new.prompt,
    })
}

/// Stores the batch's messages now, so history order matches the order turns see them in.
async fn store_user_messages(shared: &Shared, batch: &[Tracked]) {
    // Images and files are numbered across the batch, in the order the tools use.
    let mut number = 0;
    for (event, _) in batch {
        let mut text = if crate::automation::is_automation(event) {
            crate::automation::history_line(&event.text)
        } else {
            event.text.clone()
        };
        for attachment in &event.attachments {
            match attachment {
                okbot_core::Attachment::Image { .. } => {
                    number += 1;
                    let _ = write!(
                        text,
                        "\n[The user attached image #{number} — view it with the attachment_view tool.]"
                    );
                }
                okbot_core::Attachment::File { name, .. } => {
                    number += 1;
                    let _ = write!(
                        text,
                        "\n[The user attached file #{number} ({}) — it can be saved to the \
                         knowledge base with the knowledge_add tool.]",
                        name.as_deref().unwrap_or("no name")
                    );
                }
                _ => {}
            }
        }
        let message = NewMessage {
            conversation: event.conversation.clone(),
            role: MessageRole::User,
            author_id: Some(event.author.id.clone()),
            author_name: event.author.display_name.clone(),
            text,
            message_id: event.message_id.clone(),
            at: event.sent_at,
        };
        if let Err(error) = shared.store.append(message).await {
            tracing::error!(%error, "cannot store message");
        }
    }
}

/// Knowledge for the prompt, tool access for MCP-capable agents, or text excerpts otherwise.
async fn gather_knowledge(
    shared: &Shared,
    batch: &[Tracked],
    requester: &Requester,
    tools: okbot_core::ToolPolicy,
    admin: bool,
    read_only: bool,
) -> (KnowledgeContext, RunGrant, Option<String>) {
    let Some(k) = &shared.knowledge else {
        return (KnowledgeContext::default(), RunGrant::none(), None);
    };
    let context = k.context(requester).await.unwrap_or_else(|error| {
        tracing::warn!(%error, "knowledge context unavailable");
        KnowledgeContext::default()
    });
    if shared.agent.caps().mcp {
        let attachments: Vec<okbot_core::Attachment> = batch
            .iter()
            .flat_map(|(e, _)| e.attachments.iter().cloned())
            .filter(|a| {
                matches!(
                    a,
                    okbot_core::Attachment::Image { .. } | okbot_core::Attachment::File { .. }
                )
            })
            .collect();
        let run = okbot_core::RunInfo {
            tools,
            ttl: shared.config.agent_timeout + Duration::from_secs(30),
            attachments: &attachments,
            admin: admin && !read_only,
            read_only,
        };
        let grant = k.grant(requester, &run).await.unwrap_or_else(|error| {
            tracing::warn!(%error, "knowledge tools unavailable");
            RunGrant::none()
        });
        return (context, grant, None);
    }
    let query: Vec<&str> = batch.iter().map(|(e, _)| e.text.as_str()).collect();
    let excerpts = k
        .excerpts(requester, &query.join("\n"))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "knowledge excerpts unavailable");
            String::new()
        });
    (context, RunGrant::none(), Some(excerpts))
}

/// Sends the answer (or the error reply), stores it and records the run.
async fn finish(
    shared: &Shared,
    event: &InboundEvent,
    outcome: Option<Result<okbot_core::AgentReply, okbot_core::AgentError>>,
    mut record: AgentRunRecord,
    placeholder: Option<&SentRef>,
) {
    let kind = match &outcome {
        None => crate::metrics::Outcome::Cancelled,
        Some(Ok(_)) => crate::metrics::Outcome::Answered,
        Some(Err(_)) => crate::metrics::Outcome::Failed,
    };
    let backend = match &outcome {
        Some(Ok(answer)) => answer.backend.clone(),
        _ => record.backend.clone(),
    };
    shared.metrics.turn(&backend, kind, record.duration);
    match outcome {
        None => {
            tracing::info!(run = %record.run_id, "turn cancelled");
            record.error = Some("cancelled".into());
            if !crate::team::fail(shared, event, "stopped").await {
                reply_over(shared, event, &shared.config.replies.stopped, placeholder).await;
            }
        }
        Some(Ok(answer)) => {
            tracing::info!(run = %record.run_id, backend = %answer.backend, ms = record.duration.as_millis(), "answered");
            record.backend = answer.backend;
            record.model = answer.model;
            record.usage = answer.usage;
            if reply_over(shared, event, &answer.text, placeholder).await {
                let message = NewMessage {
                    conversation: event.conversation.clone(),
                    role: MessageRole::Bot,
                    author_id: None,
                    author_name: shared.config.bot_name.clone(),
                    text: answer.text,
                    message_id: None,
                    at: shared.clock.now(),
                };
                if let Err(error) = shared.store.append(message).await {
                    tracing::error!(%error, "cannot store the answer");
                }
            }
        }
        Some(Err(error)) => {
            tracing::error!(run = %record.run_id, %error, "agent turn failed");
            record.error = Some(error.to_string());
            // Another bot learns that the task failed, not a chat apology.
            if !crate::team::fail(shared, event, "the agent of this bot failed").await {
                reply_over(shared, event, &shared.config.replies.error, placeholder).await;
            }
        }
    }
    if let Err(error) = shared.store.record_run(record).await {
        tracing::warn!(%error, "cannot record the agent run");
    }
}
