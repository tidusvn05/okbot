//! One agent turn: context, agent run, rendering, sending, audit.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use okbot_core::{
    AgentRunRecord, AgentTurn, CancellationToken, InboundEvent, KnowledgeContext, MessageRole,
    NewMessage, Requester, RunGrant, RunId,
};
use sha2::{Digest, Sha256};
use tracing::Instrument;

use crate::context::{self, PromptInput};
use crate::dispatch::target;
use crate::{Shared, Tracked};

/// Renders `markdown` for the event's channel and sends it as a reply. Failures are logged.
pub(crate) async fn reply(shared: &Shared, event: &InboundEvent, markdown: &str) -> bool {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        tracing::error!(channel = %event.conversation.channel, "reply to unknown channel");
        return false;
    };
    let to = target(event);
    for part in shared.renderer.render(markdown, channel.caps()) {
        if let Err(error) = channel.send(&to, &part).await {
            tracing::error!(channel = %channel.id(), %error, "sending a reply failed");
            return false;
        }
    }
    true
}

fn run_id() -> RunId {
    RunId::new(hex::encode(rand::random::<[u8; 8]>()))
}

/// A stable, path-safe directory name for a conversation.
fn workdir(shared: &Shared, event: &InboundEvent) -> PathBuf {
    let digest = Sha256::digest(event.conversation.to_string().as_bytes());
    shared.config.work_dir.join(&hex::encode(digest)[..16])
}

/// Keeps the typing indicator up until dropped.
struct Typing(Option<tokio::task::JoinHandle<()>>);

impl Typing {
    /// Shows the indicator once right away, then repeats it in the background.
    async fn start(shared: &Shared, event: &InboundEvent) -> Self {
        let Some(channel) = shared.channels.get(&event.conversation.channel).cloned() else {
            return Self(None);
        };
        if !channel.caps().typing {
            return Self(None);
        }
        let to = target(event);
        let every = shared.config.typing_interval;
        if let Err(error) = channel.typing(&to).await {
            tracing::debug!(%error, "typing indicator failed");
        }
        Self(Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                if let Err(error) = channel.typing(&to).await {
                    tracing::debug!(%error, "typing indicator failed");
                }
            }
        })))
    }
}

impl Drop for Typing {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
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
    answer_batch(shared, batch, event, cancel)
        .instrument(span)
        .await;
}

async fn answer_batch(
    shared: &Shared,
    batch: &[Tracked],
    event: &InboundEvent,
    cancel: &CancellationToken,
) {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        return;
    };
    let _typing = Typing::start(shared, event).await;
    store_user_messages(shared, batch).await;

    let room = shared
        .config
        .room(&event.conversation.channel, &event.conversation.room);
    let tools = room.tool_policy.unwrap_or(shared.config.tool_policy);
    let requester = Requester {
        conversation: event.conversation.clone(),
        user: event.author.clone(),
        internal: room.internal,
    };
    let timeout = shared.config.agent_timeout;

    let (knowledge, grant, excerpts) = gather_knowledge(shared, batch, &requester, tools).await;

    let history = shared
        .store
        .recent(&event.conversation, shared.config.history_messages)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "history unavailable");
            Vec::new()
        });
    let now = shared.clock.now();
    let prompt = context::build(&PromptInput {
        bot_name: &shared.config.bot_name,
        caps: channel.caps(),
        tools,
        knowledge: &knowledge,
        excerpts: excerpts.as_deref().filter(|e| !e.is_empty()),
        new_count: batch.len().min(history.len()),
        history: &history,
        event,
        now,
        timezone: shared.config.timezone.as_deref(),
        history_tokens: shared.config.history_tokens,
    });

    let turn = AgentTurn {
        system: prompt.system,
        prompt: prompt.prompt,
        tools,
        mcp: grant.mcp.clone(),
        env: grant.env.clone(),
        workdir: workdir(shared, event),
        timeout,
        resume: None,
    };
    let run_id = run_id();
    let started = Instant::now();
    let outcome = tokio::select! {
        result = shared.agent.run(turn) => Some(result),
        () = cancel.cancelled() => None,
    };
    drop(grant);

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
    };
    finish(shared, event, outcome, record).await;
}

/// Stores the batch's messages now, so history order matches the order turns see them in.
async fn store_user_messages(shared: &Shared, batch: &[Tracked]) {
    for (event, _) in batch {
        let message = NewMessage {
            conversation: event.conversation.clone(),
            role: MessageRole::User,
            author_id: Some(event.author.id.clone()),
            author_name: event.author.display_name.clone(),
            text: event.text.clone(),
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
) -> (KnowledgeContext, RunGrant, Option<String>) {
    let Some(k) = &shared.knowledge else {
        return (KnowledgeContext::default(), RunGrant::none(), None);
    };
    let context = k.context(requester).await.unwrap_or_else(|error| {
        tracing::warn!(%error, "knowledge context unavailable");
        KnowledgeContext::default()
    });
    if shared.agent.caps().mcp {
        let ttl = shared.config.agent_timeout + Duration::from_secs(30);
        let grant = k
            .grant(requester, tools, ttl)
            .await
            .unwrap_or_else(|error| {
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
) {
    match outcome {
        None => {
            tracing::info!(run = %record.run_id, "turn cancelled");
            record.error = Some("cancelled".into());
        }
        Some(Ok(answer)) => {
            tracing::info!(run = %record.run_id, backend = %answer.backend, ms = record.duration.as_millis(), "answered");
            record.backend = answer.backend;
            record.model = answer.model;
            record.usage = answer.usage;
            if reply(shared, event, &answer.text).await {
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
            reply(shared, event, &shared.config.replies.error).await;
        }
    }
    if let Err(error) = shared.store.record_run(record).await {
        tracing::warn!(%error, "cannot record the agent run");
    }
}
