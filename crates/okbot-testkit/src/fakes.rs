//! Fake adapters.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use okbot_core::{
    AgentBackend, AgentCaps, AgentError, AgentReply, AgentTurn, ToolPolicy, async_trait,
};

/// What a [`FakeAgent`] does.
#[derive(Debug, Clone)]
enum Behaviour {
    Echo,
    /// Reports these progress events `gap` apart, then answers `text`.
    Stream(Vec<okbot_core::AgentProgress>, Duration, String),
    Fail {
        retryable: bool,
    },
    Reply(String),
    Sleep(Duration),
}

/// An [`AgentBackend`] for tests: echoes, replies, fails or sleeps, and records its turns.
#[derive(Debug)]
pub struct FakeAgent {
    name: String,
    behaviour: Behaviour,
    caps: AgentCaps,
    session: Option<String>,
    calls: AtomicUsize,
    turns: Mutex<Vec<AgentTurn>>,
}

impl FakeAgent {
    fn with(name: &str, behaviour: Behaviour) -> Self {
        Self {
            name: name.into(),
            behaviour,
            caps: AgentCaps::default(),
            session: None,
            calls: AtomicUsize::new(0),
            turns: Mutex::new(Vec::new()),
        }
    }

    /// Answers `echo: <prompt>`.
    pub fn echo(name: &str) -> Self {
        Self::with(name, Behaviour::Echo)
    }

    /// Always answers `text`.
    pub fn replying(name: &str, text: &str) -> Self {
        Self::with(name, Behaviour::Reply(text.into()))
    }

    /// Always fails.
    pub fn failing(name: &str, retryable: bool) -> Self {
        Self::with(name, Behaviour::Fail { retryable })
    }

    /// Sleeps for `d`, then echoes.
    pub fn sleeping(name: &str, d: Duration) -> Self {
        Self::with(name, Behaviour::Sleep(d))
    }

    /// Reports `progress` events `gap` apart, then answers `text`.
    pub fn streaming(
        name: &str,
        progress: Vec<okbot_core::AgentProgress>,
        gap: Duration,
        text: &str,
    ) -> Self {
        Self::with(name, Behaviour::Stream(progress, gap, text.into()))
    }

    /// Returns `session` as the native session id and declares resume support.
    pub fn with_session(mut self, session: &str) -> Self {
        self.session = Some(session.into());
        self.caps.resume = true;
        self
    }

    /// Declares the tool policies it enforces.
    pub fn enforcing(mut self, policies: &[ToolPolicy]) -> Self {
        self.caps.tool_policies = policies.to_vec();
        self
    }

    /// Declares native capabilities.
    pub fn with_caps(mut self, caps: AgentCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Number of turns started.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The turns received, oldest first.
    pub fn turns(&self) -> Vec<AgentTurn> {
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl AgentBackend for FakeAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn caps(&self) -> AgentCaps {
        self.caps.clone()
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(turn.clone());
        let text = match &self.behaviour {
            Behaviour::Echo => format!("echo: {}", turn.prompt),
            Behaviour::Reply(text) => text.clone(),
            Behaviour::Fail { retryable } => {
                return Err(AgentError::Failed {
                    message: format!("{} failed", self.name),
                    retryable: *retryable,
                });
            }
            Behaviour::Sleep(d) => {
                tokio::time::sleep(*d).await;
                format!("echo: {}", turn.prompt)
            }
            Behaviour::Stream(events, gap, text) => {
                for event in events {
                    if let Some(progress) = &turn.progress {
                        progress.report(event.clone());
                    }
                    tokio::time::sleep(*gap).await;
                }
                text.clone()
            }
        };
        Ok(AgentReply {
            text,
            backend: self.name.clone(),
            model: None,
            session_id: self.session.clone(),
            usage: None,
            duration: Duration::ZERO,
        })
    }
}

/// A minimal turn with `prompt`, the default tool policy and a temp working directory.
pub fn turn(prompt: &str) -> AgentTurn {
    AgentTurn {
        system: String::new(),
        prompt: prompt.into(),
        tools: ToolPolicy::default(),
        mcp: Vec::new(),
        env: Vec::new(),
        workdir: std::env::temp_dir().join("okbot-testkit-work"),
        timeout: Duration::from_secs(10),
        resume: None,
        progress: None,
    }
}

/// Writes an executable shell script `dir/name` that prints `1.0.0` for `--version` and runs
/// `body` otherwise (Unix only). Mirrors agent-call's test helper.
#[cfg(unix)]
pub fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0.0; exit 0; fi\n{body}\n"),
    )
    .expect("write fake CLI");
    let mut permissions = std::fs::metadata(&path).expect("stat").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod");
    // Some filesystems briefly report ETXTBSY right after creating an executable.
    std::thread::sleep(Duration::from_millis(10));
    path
}

/// A [`Channel`](okbot_core::Channel) for tests: events are injected by the test, sent messages
/// are recorded.
pub struct FakeChannel {
    id: okbot_core::ChannelId,
    caps: okbot_core::ChannelCaps,
    sink: Mutex<Option<std::sync::Arc<dyn okbot_core::InboundSink>>>,
    sent: Mutex<Vec<(okbot_core::ReplyTarget, String)>>,
    edits: Mutex<Vec<(String, String)>>,
    prompts: Mutex<Vec<okbot_core::Prompt>>,
    prompt_updates: Mutex<Vec<(String, String, bool)>>,
    typing: AtomicUsize,
    ready: tokio::sync::Notify,
    opens_direct: bool,
}

impl std::fmt::Debug for FakeChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl FakeChannel {
    /// A channel `id` (`fake:test`) with `caps`.
    pub fn new(id: &str, caps: okbot_core::ChannelCaps) -> Self {
        Self {
            id: id.parse().expect("valid channel id"),
            caps,
            sink: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
            edits: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
            prompt_updates: Mutex::new(Vec::new()),
            typing: AtomicUsize::new(0),
            ready: tokio::sync::Notify::new(),
            opens_direct: false,
        }
    }

    /// Lets [`Channel::open_direct`](okbot_core::Channel::open_direct) open a direct chat with
    /// anyone, as room `dm-<user id>` (Discord, Telegram); without it the call is unsupported.
    pub fn opening_direct_chats(mut self) -> Self {
        self.opens_direct = true;
        self
    }

    /// Waits until the runtime started this channel, then delivers `event`.
    pub async fn inject(&self, event: okbot_core::InboundEvent) {
        loop {
            let notified = self.ready.notified();
            let sink = self
                .sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(sink) = sink {
                sink.deliver(event).await.expect("runtime accepts events");
                return;
            }
            notified.await;
        }
    }

    /// Texts sent so far, oldest first.
    pub fn sent_texts(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// Messages sent so far with their targets.
    pub fn sent(&self) -> Vec<(okbot_core::ReplyTarget, String)> {
        self.sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Edits so far: `(message id, new text)`.
    pub fn edits(&self) -> Vec<(String, String)> {
        self.edits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Prompts shown so far, oldest first.
    pub fn prompts(&self) -> Vec<okbot_core::Prompt> {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Prompt updates so far: `(message id, text, buttons kept)`.
    pub fn prompt_updates(&self) -> Vec<(String, String, bool)> {
        self.prompt_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Delivers an answer to a prompt, as a click would.
    pub async fn answer(&self, answer: okbot_core::PromptAnswer) {
        let sink = self
            .sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("the runtime started this channel");
        sink.answer(answer).await.expect("runtime accepts answers");
    }

    /// Typing indicators shown so far.
    pub fn typing_count(&self) -> usize {
        self.typing.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl okbot_core::Channel for FakeChannel {
    fn id(&self) -> &okbot_core::ChannelId {
        &self.id
    }

    fn caps(&self) -> &okbot_core::ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: std::sync::Arc<dyn okbot_core::InboundSink>,
        shutdown: okbot_core::CancellationToken,
    ) -> Result<(), okbot_core::ChannelError> {
        *self
            .sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
        self.ready.notify_waiters();
        shutdown.cancelled().await;
        Ok(())
    }

    async fn open_direct(
        &self,
        user: &okbot_core::UserId,
    ) -> Result<okbot_core::ConversationKey, okbot_core::ChannelError> {
        if !self.opens_direct {
            return Err(okbot_core::ChannelError::Unsupported("open_direct"));
        }
        Ok(okbot_core::ConversationKey::room(
            self.id.clone(),
            format!("dm-{user}"),
        ))
    }

    async fn send(
        &self,
        to: &okbot_core::ReplyTarget,
        message: &okbot_core::RenderedMessage,
    ) -> Result<okbot_core::SentRef, okbot_core::ChannelError> {
        let mut sent = self
            .sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sent.push((to.clone(), message.text.clone()));
        Ok(okbot_core::SentRef {
            target: to.clone(),
            message_id: okbot_core::MessageId::new(format!("sent-{}", sent.len())),
        })
    }

    async fn typing(&self, _to: &okbot_core::ReplyTarget) -> Result<(), okbot_core::ChannelError> {
        self.typing.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn prompt(
        &self,
        to: &okbot_core::ReplyTarget,
        prompt: &okbot_core::Prompt,
    ) -> Result<okbot_core::SentRef, okbot_core::ChannelError> {
        let mut prompts = self
            .prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prompts.push(prompt.clone());
        Ok(okbot_core::SentRef {
            target: to.clone(),
            message_id: okbot_core::MessageId::new(format!("prompt-{}", prompts.len())),
        })
    }

    async fn close_prompt(
        &self,
        sent: &okbot_core::SentRef,
        message: &okbot_core::RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), okbot_core::ChannelError> {
        self.prompt_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                sent.message_id.to_string(),
                message.text.clone(),
                keep_buttons,
            ));
        Ok(())
    }

    async fn edit(
        &self,
        sent: &okbot_core::SentRef,
        message: &okbot_core::RenderedMessage,
    ) -> Result<(), okbot_core::ChannelError> {
        if !self.caps.edit {
            return Err(okbot_core::ChannelError::Unsupported("edit"));
        }
        self.edits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((sent.message_id.to_string(), message.text.clone()));
        Ok(())
    }
}

/// An inbound text event from user `user` in `room` of `channel`, with a unique event id.
pub fn inbound(channel: &str, room: &str, user: &str, text: &str) -> okbot_core::InboundEvent {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    okbot_core::InboundEvent {
        event_id: format!("ev-{n}"),
        conversation: okbot_core::ConversationKey::room(
            channel.parse().expect("valid channel id"),
            room,
        ),
        author: okbot_core::UserRef {
            id: okbot_core::UserId::new(user),
            display_name: user.to_string(),
            is_bot: false,
        },
        text: text.into(),
        attachments: Vec::new(),
        mentions_bot: false,
        is_direct: true,
        restricted: false,
        message_id: Some(okbot_core::MessageId::new(format!("m-{n}"))),
        sent_at: "2026-01-01T00:00:00Z".parse().expect("timestamp"),
        locale: None,
    }
}

/// A [`Renderer`](okbot_core::Renderer) that sends the Markdown unchanged as one message.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainRenderer;

impl okbot_core::Renderer for PlainRenderer {
    fn render(
        &self,
        markdown: &str,
        _caps: &okbot_core::ChannelCaps,
    ) -> Vec<okbot_core::RenderedMessage> {
        vec![okbot_core::RenderedMessage {
            text: markdown.to_string(),
        }]
    }
}

/// An [`InboundSink`](okbot_core::InboundSink) that keeps every event, for adapter tests.
#[derive(Debug, Default)]
pub struct CollectSink(Mutex<Vec<okbot_core::InboundEvent>>);

impl CollectSink {
    /// Events delivered so far.
    pub fn events(&self) -> Vec<okbot_core::InboundEvent> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Waits up to 10 seconds for the first event.
    ///
    /// # Panics
    /// When none arrives.
    pub async fn first(&self) -> okbot_core::InboundEvent {
        for _ in 0..500 {
            if let Some(event) = self.events().into_iter().next() {
                return event;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no inbound event within 10 s");
    }
}

#[okbot_core::async_trait]
impl okbot_core::InboundSink for CollectSink {
    async fn deliver(&self, event: okbot_core::InboundEvent) -> Result<(), okbot_core::SinkClosed> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
        Ok(())
    }
}
