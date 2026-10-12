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
async fn reply_over(
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
struct Typing(Option<tokio::task::JoinHandle<()>>, Option<SentRef>);

impl Typing {
    /// Shows the indicator once right away, then repeats it in the background.
    async fn start(shared: &Shared, event: &InboundEvent) -> Self {
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
    let typing = Typing::start(shared, event).await;
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
    };
    let prompt = context::build(&input);
    let resume = resume_for(shared, event, &input, new_count).await;

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
        mcp: grant.mcp.clone(),
        env: grant.env.clone(),
        workdir: workdir(shared, event),
        timeout,
        resume,
        progress,
    };
    let run_id = run_id();
    let started = Instant::now();
    let outcome = tokio::select! {
        result = shared.agent.run(turn) => Some(result),
        () = cancel.cancelled() => None,
    };
    drop(grant);
    drop(live);
    if let Some(Ok(answer)) = &outcome
        && let Some(session) = &answer.session_id
        && let Err(error) = shared
            .store
            .set_session(&event.conversation, &answer.backend, session)
            .await
    {
        tracing::warn!(%error, "cannot remember the agent session");
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
    };
    finish(shared, event, outcome, record, typing.1.as_ref()).await;
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
    // Attachments are numbered across the batch, in the order the viewing tool uses.
    let mut number = 0;
    for (event, _) in batch {
        let mut text = event.text.clone();
        let images = event
            .attachments
            .iter()
            .filter(|a| matches!(a, okbot_core::Attachment::Image { .. }))
            .count();
        if images > 0 {
            let ids: Vec<String> = (number + 1..=number + images)
                .map(|i| format!("#{i}"))
                .collect();
            number += images;
            let _ = write!(
                text,
                "\n[The user attached image {} — view it with the attachment_view tool.]",
                ids.join(", ")
            );
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
            .filter(|a| matches!(a, okbot_core::Attachment::Image { .. }))
            .collect();
        let run = okbot_core::RunInfo {
            tools,
            ttl: shared.config.agent_timeout + Duration::from_secs(30),
            attachments: &attachments,
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
            reply_over(shared, event, &shared.config.replies.stopped, placeholder).await;
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
            reply_over(shared, event, &shared.config.replies.error, placeholder).await;
        }
    }
    if let Err(error) = shared.store.record_run(record).await {
        tracing::warn!(%error, "cannot record the agent run");
    }
}
