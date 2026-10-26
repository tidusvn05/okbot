//! Terminal channel for okbot, for development and tests.
//!
//! Each input line is a message from the local user; replies are printed as text. Lines that start
//! with `:` are console commands (`:help` lists them): they switch the user (`:as`), move to a
//! group room (`:room`, where `@<bot name>` mentions the bot) or a thread, attach files, press
//! prompt buttons and wait for answers in scripts. A [`Preset`] makes the console behave like a
//! platform: its dialect and length limit, edits, typing and buttons.
//!
//! When the input ends, [`ConsoleChannel::closed`] is cancelled so the caller can shut down after
//! the last answer.

#[cfg(all(feature = "line-editing", unix))]
mod editor;
mod line;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use okbot_core::{
    Attachment, CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, CommandInfo,
    ConversationKey, InboundEvent, InboundSink, LengthUnit, MarkdownDialect, MessageId, Prompt,
    PromptAnswer, RenderedMessage, ReplyTarget, Secret, SentRef, UserId, UserRef, async_trait,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, watch};

use crate::line::{Command, Line};

type Reader = Box<dyn AsyncBufRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// Resolves once the bot has handled every message delivered so far (`Runtime::handle().wait_idle()`).
pub type IdleWait = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// How long `:wait` waits without an [`IdleWait`] or a limit of its own.
const WAIT: Duration = Duration::from_secs(300);

/// The room id of the default user's direct chat (kept from before rooms existed, so its history
/// stays).
const DEFAULT_DM: &str = "console";

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `--like` named a platform the console cannot imitate.
    #[error("unknown platform `{0}` (use console, zalo, gchat, discord, slack or telegram)")]
    UnknownPreset(String),
}

/// A platform the console imitates. The limits mirror the platform adapters'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Preset {
    /// The plain terminal: CommonMark, long messages, buttons and forms.
    #[default]
    Console,
    /// Zalo Bot: `ZaloMd`, 2000 UTF-16 units, typing, no edits or buttons.
    Zalo,
    /// Google Chat: 4096 characters, threads, edits, buttons without forms.
    GChat,
    /// Discord: 2000 characters, threads, edits, typing, buttons and forms.
    Discord,
    /// Slack: mrkdwn, 3000 characters, threads, edits, buttons without forms.
    Slack,
    /// Telegram: HTML, 4096 UTF-16 units, threads, edits, typing, buttons without forms.
    Telegram,
}

impl Preset {
    /// The capabilities the console reports.
    pub fn caps(self) -> ChannelCaps {
        use LengthUnit::{Chars, Utf16};
        use MarkdownDialect as D;
        let caps = |max_message_len, length_unit, dialect, threads, edit, typing| ChannelCaps {
            max_message_len,
            length_unit,
            dialect,
            threads,
            edit,
            typing,
            ui: okbot_core::UiCaps::default(),
        };
        match self {
            // The terminal prints cards itself and writes charts to files (ADR 0016).
            Self::Console => ChannelCaps {
                max_message_len: 100_000,
                dialect: D::CommonMark,
                ui: okbot_core::UiCaps {
                    cards: true,
                    images: true,
                    ..okbot_core::UiCaps::default()
                },
                ..ChannelCaps::basic()
            },
            Self::Zalo => caps(2000, Utf16, D::ZaloMd, false, false, true),
            Self::GChat => caps(4096, Chars, D::GChatText, true, true, false),
            Self::Discord => caps(2000, Chars, D::DiscordMd, true, true, true),
            Self::Slack => caps(3000, Chars, D::SlackMrkdwn, true, true, false),
            Self::Telegram => caps(4096, Utf16, D::TelegramHtml, true, true, true),
        }
    }

    /// Whether prompts show buttons (otherwise the runtime asks for `/approve` and the like).
    pub fn buttons(self) -> bool {
        self != Self::Zalo
    }

    /// Whether buttons may open forms.
    pub fn forms(self) -> bool {
        matches!(self, Self::Console | Self::Discord)
    }

    /// The name `--like` takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Zalo => "zalo",
            Self::GChat => "gchat",
            Self::Discord => "discord",
            Self::Slack => "slack",
            Self::Telegram => "telegram",
        }
    }
}

impl FromStr for Preset {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            Self::Console,
            Self::Zalo,
            Self::GChat,
            Self::Discord,
            Self::Slack,
            Self::Telegram,
        ]
        .into_iter()
        .find(|p| p.name() == s)
        .ok_or_else(|| Error::UnknownPreset(s.to_string()))
    }
}

/// How the console starts.
#[derive(Debug, Clone)]
pub struct ConsoleOptions {
    /// The default user, who owns the terminal.
    pub user: String,
    /// The bot's name, for `@<name>` mentions in rooms.
    pub bot_name: String,
    /// The platform to imitate.
    pub preset: Preset,
}

impl ConsoleOptions {
    /// Plain console options for `user`.
    pub fn new(user: &str) -> Self {
        Self {
            user: user.to_string(),
            bot_name: "okbot".into(),
            preset: Preset::Console,
        }
    }
}

/// Where the next message goes and what it carries.
#[derive(Debug)]
struct State {
    user: String,
    room: Option<String>,
    thread: Option<String>,
    attachments: Vec<Attachment>,
    /// Prompts with buttons, oldest first.
    prompts: Vec<(Prompt, SentRef)>,
    /// Conversations whose typing indicator is shown.
    typing: HashSet<ConversationKey>,
}

/// A chat in the terminal.
pub struct ConsoleChannel {
    id: ChannelId,
    caps: ChannelCaps,
    options: ConsoleOptions,
    input: Mutex<Option<Reader>>,
    output: Mutex<Writer>,
    state: StdMutex<State>,
    closed: CancellationToken,
    next: AtomicU64,
    /// Everything the bot showed (messages, edits, prompts), for `:wait` and `:expect`.
    transcript: StdMutex<Vec<Shown>>,
    /// The transcript's length.
    shown: watch::Sender<usize>,
    idle: OnceLock<IdleWait>,
    /// Where chart images go; [`chart_dir`] when unset.
    charts: OnceLock<PathBuf>,
    /// `:expect` checks that failed.
    failures: AtomicU64,
}

/// One thing the bot showed.
#[derive(Debug, Clone)]
struct Shown {
    prompt: bool,
    text: String,
}

impl std::fmt::Debug for ConsoleChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleChannel")
            .field("id", &self.id)
            .field("preset", &self.options.preset)
            .finish_non_exhaustive()
    }
}

fn io_error(e: std::io::Error) -> ChannelError {
    ChannelError::Failed(Box::new(e))
}

impl ConsoleChannel {
    /// Reads stdin and writes stdout, as `user`.
    pub fn stdio(user: &str) -> Self {
        Self::stdio_with(ConsoleOptions::new(user))
    }

    /// Reads stdin and writes stdout.
    pub fn stdio_with(options: ConsoleOptions) -> Self {
        Self::new(
            options,
            Box::new(BufReader::new(tokio::io::stdin())),
            Box::new(tokio::io::stdout()),
        )
    }

    /// Reads `input` and writes `output`, as `user`.
    pub fn with_io(user: &str, input: Reader, output: Writer) -> Self {
        Self::new(ConsoleOptions::new(user), input, output)
    }

    /// Reads `input` and writes `output`.
    pub fn new(options: ConsoleOptions, input: Reader, output: Writer) -> Self {
        Self {
            id: ChannelId::new("console", "local").expect("valid literal id"),
            caps: options.preset.caps(),
            state: StdMutex::new(State {
                user: options.user.clone(),
                room: None,
                thread: None,
                attachments: Vec::new(),
                prompts: Vec::new(),
                typing: HashSet::new(),
            }),
            options,
            input: Mutex::new(Some(input)),
            output: Mutex::new(output),
            closed: CancellationToken::new(),
            next: AtomicU64::new(1),
            transcript: StdMutex::new(Vec::new()),
            shown: watch::channel(0).0,
            idle: OnceLock::new(),
            charts: OnceLock::new(),
            failures: AtomicU64::new(0),
        }
    }

    /// Cancelled when the input has ended.
    pub fn closed(&self) -> CancellationToken {
        self.closed.clone()
    }

    /// Makes `:wait` wait until the bot is idle instead of for its next message. Set once.
    pub fn set_idle_wait(&self, wait: IdleWait) {
        let _ = self.idle.set(wait);
    }

    /// Writes chart images to `dir` instead of `<temp dir>/okbot-console`. Set once.
    pub fn set_chart_dir(&self, dir: PathBuf) {
        let _ = self.charts.set(dir);
    }

    /// Writes a card's chart to `<chart dir>/<prompt id>-chart.png`; returns its path.
    async fn save_chart(&self, prompt: &str, png: &[u8]) -> Result<PathBuf, ChannelError> {
        let dir = self.charts.get().cloned().unwrap_or_else(chart_dir);
        tokio::fs::create_dir_all(&dir).await.map_err(io_error)?;
        // Prompt ids are the runtime's short ids; anything else is kept out of the path.
        let id: String = prompt
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        let path = dir.join(format!("{id}-chart.png"));
        tokio::fs::write(&path, png).await.map_err(io_error)?;
        Ok(path)
    }

    /// How many `:expect` and `:expect-not` checks failed.
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::SeqCst)
    }

    fn transcript(&self) -> std::sync::MutexGuard<'_, Vec<Shown>> {
        self.transcript
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // The state holds plain values; a panic elsewhere cannot leave it inconsistent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn dm_room(&self, user: &str) -> String {
        if user == self.options.user {
            DEFAULT_DM.to_string()
        } else {
            format!("dm-{user}")
        }
    }

    fn conversation(&self, state: &State) -> ConversationKey {
        let room = match &state.room {
            Some(room) => room.clone(),
            None => self.dm_room(&state.user),
        };
        let mut key = ConversationKey::room(self.id.clone(), room);
        key.thread.clone_from(&state.thread);
        key
    }

    /// How output for `key` is labelled; nothing for the default user's direct chat.
    fn label(key: &ConversationKey) -> Option<String> {
        let room = match key.room.strip_prefix("dm-") {
            _ if key.room == DEFAULT_DM => None,
            Some(user) => Some(format!("dm {user}")),
            None => Some(format!("#{}", key.room)),
        };
        match (room, &key.thread) {
            (room, Some(thread)) => Some(format!("{}/{thread}", room.unwrap_or_default())),
            (room, None) => room,
        }
    }

    fn next_id(&self) -> MessageId {
        MessageId::new(format!("out-{}", self.next.fetch_add(1, Ordering::SeqCst)))
    }

    fn event(&self, text: String, restricted: bool) -> InboundEvent {
        let mut state = self.state();
        let conversation = self.conversation(&state);
        let is_direct = state.room.is_none();
        let (text, mentions_bot) = if is_direct {
            (text, false)
        } else {
            line::strip_mention(&text, &self.options.bot_name)
        };
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        InboundEvent {
            event_id: format!("console-{}-{n}", std::process::id()),
            conversation,
            author: UserRef {
                id: UserId::new(state.user.clone()),
                display_name: state.user.clone(),
                is_bot: false,
            },
            text: if restricted { String::new() } else { text },
            attachments: std::mem::take(&mut state.attachments),
            mentions_bot,
            is_direct,
            restricted,
            message_id: Some(MessageId::new(n.to_string())),
            sent_at: jiff::Timestamp::now(),
            locale: None,
        }
    }

    /// Writes a block: `[tags]` on its own line when there are any, then the text.
    async fn print(&self, tags: &[String], text: &str) -> Result<(), ChannelError> {
        let mut block = String::new();
        if !tags.is_empty() {
            block.push('[');
            block.push_str(&tags.join(" · "));
            block.push_str("]\n");
        }
        if !text.is_empty() {
            block.push_str(text.trim_end());
            block.push('\n');
        }
        block.push('\n');
        let block = printable(&block);
        let mut out = self.output.lock().await;
        out.write_all(block.as_bytes()).await.map_err(io_error)?;
        out.flush().await.map_err(io_error)
    }

    /// A note from the console itself.
    async fn note(&self, text: &str) -> Result<(), ChannelError> {
        let mut out = self.output.lock().await;
        for line in text.lines() {
            out.write_all(format!("[console] {}\n", printable(line)).as_bytes())
                .await
                .map_err(io_error)?;
        }
        out.flush().await.map_err(io_error)
    }

    /// Tags for output to `key`, with `what` after the room.
    fn tags(&self, key: &ConversationKey, what: Option<String>) -> Vec<String> {
        let mut state = self.state();
        state.typing.remove(key);
        Self::label(key).into_iter().chain(what).collect()
    }

    /// Records what the bot showed.
    fn shown(&self, prompt: bool, text: &str) {
        let len = {
            let mut transcript = self.transcript();
            transcript.push(Shown {
                prompt,
                text: text.to_string(),
            });
            transcript.len()
        };
        self.shown.send_replace(len);
    }

    /// Runs a console command; `Ok(false)` stops reading.
    async fn command(
        &self,
        command: Command,
        sink: &Arc<dyn InboundSink>,
        shutdown: &CancellationToken,
        mark: usize,
    ) -> Result<bool, ChannelError> {
        match command {
            Command::Help => self.note(line::HELP).await?,
            Command::Where => self.where_now().await?,
            Command::As(user) => {
                self.state().user = user;
                self.where_now().await?;
            }
            Command::Room(room) => {
                {
                    let mut state = self.state();
                    state.room = Some(room);
                    state.thread = None;
                }
                self.where_now().await?;
            }
            Command::Dm => {
                {
                    let mut state = self.state();
                    state.room = None;
                    state.thread = None;
                }
                self.where_now().await?;
            }
            Command::Thread(thread) => {
                if !self.caps.threads && thread.is_some() {
                    self.note(&format!(
                        "{} has no threads; try --like gchat, discord, slack or telegram",
                        self.options.preset.name()
                    ))
                    .await?;
                } else {
                    self.state().thread = thread;
                    self.where_now().await?;
                }
            }
            Command::Attach(target) => {
                let attachment = attachment(&target);
                self.state().attachments.push(attachment);
                self.note(&format!("{target} goes with the next message"))
                    .await?;
            }
            Command::Restricted => {
                if sink.deliver(self.event(String::new(), true)).await.is_err() {
                    return Ok(false);
                }
            }
            Command::Press { button, values } => {
                let answer = self.press(button, values);
                match answer {
                    Ok(answer) => {
                        if sink.answer(answer).await.is_err() {
                            return Ok(false);
                        }
                    }
                    Err(e) => self.note(&e).await?,
                }
            }
            Command::Wait(limit) => {
                if !self
                    .wait_bounded(mark, limit.unwrap_or(WAIT), shutdown)
                    .await?
                {
                    return Ok(false);
                }
            }
            Command::Expect { text, present } => {
                if !self.wait_bounded(mark, WAIT, shutdown).await? {
                    return Ok(false);
                }
                let found = self.transcript()[mark..]
                    .iter()
                    .any(|s| s.text.contains(&text));
                if found != present {
                    self.failures.fetch_add(1, Ordering::SeqCst);
                    let what = if present { "missing" } else { "unexpected" };
                    self.note(&format!(
                        "FAILED :expect{}: {what} `{text}` in the answer to the last message",
                        if present { "" } else { "-not" }
                    ))
                    .await?;
                }
            }
            Command::Paste => unreachable!("the input loop reads pasted lines"),
        }
        Ok(true)
    }

    /// Says who writes where.
    async fn where_now(&self) -> Result<(), ChannelError> {
        let text = {
            let state = self.state();
            let key = self.conversation(&state);
            format!(
                "{} in {} (like {}){}",
                state.user,
                Self::label(&key).unwrap_or_else(|| "the direct chat".into()),
                self.options.preset.name(),
                match state.attachments.len() {
                    0 => String::new(),
                    n => format!("; {n} attachment(s) for the next message"),
                }
            )
        };
        self.note(&text).await
    }

    /// [`Self::wait`] for at most `limit`; `Ok(false)` on shutdown.
    async fn wait_bounded(
        &self,
        mark: usize,
        limit: Duration,
        shutdown: &CancellationToken,
    ) -> Result<bool, ChannelError> {
        tokio::select! {
            () = shutdown.cancelled() => Ok(false),
            finished = tokio::time::timeout(limit, self.wait(mark)) => {
                if finished.is_err() {
                    self.note("still waiting for the bot; going on").await?;
                }
                Ok(true)
            }
        }
    }

    /// Waits until the bot is idle (or, without an [`IdleWait`], has shown something since
    /// transcript entry `mark`), or until it shows a prompt since then: a turn that waits for a
    /// button is not idle until someone presses it.
    async fn wait(&self, mark: usize) {
        let mut shown = self.shown.subscribe();
        let prompt = shown.wait_for(|_| self.transcript()[mark..].iter().any(|s| s.prompt));
        let done = async {
            match self.idle.get() {
                Some(idle) => idle().await,
                None => {
                    let _ = self.shown.subscribe().wait_for(|n| *n > mark).await;
                }
            }
        };
        tokio::select! {
            _ = prompt => {}
            () = done => {}
        }
    }

    /// The answer for pressing `button` on the newest prompt in the current conversation.
    fn press(&self, button: usize, values: Vec<(String, String)>) -> Result<PromptAnswer, String> {
        let state = self.state();
        let key = self.conversation(&state);
        let (prompt, sent) = state
            .prompts
            .iter()
            .rev()
            .find(|(_, sent)| sent.target.conversation == key)
            .ok_or("no open prompt in this conversation")?;
        let user = UserId::new(state.user.clone());
        if !prompt.may_answer(&user) {
            return Err(format!("this prompt is not for {}", state.user));
        }
        let pressed = prompt
            .buttons
            .get(button - 1)
            .ok_or_else(|| format!("the prompt has {} button(s)", prompt.buttons.len()))?;
        let mut answers = BTreeMap::new();
        match &pressed.form {
            None if !values.is_empty() => {
                return Err(format!("`{}` has no form to fill in", pressed.label));
            }
            None => {}
            Some(form) => {
                for (id, value) in values {
                    if !form.fields.iter().any(|f| f.id == id) {
                        return Err(format!("the form has no field `{id}`"));
                    }
                    answers.insert(id, Secret::new(value));
                }
                let missing: Vec<&str> = form
                    .fields
                    .iter()
                    .filter(|f| f.required && !answers.contains_key(&f.id))
                    .map(|f| f.id.as_str())
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "fill in {} (:press {button} {}=...)",
                        missing.join(", "),
                        missing[0]
                    ));
                }
            }
        }
        Ok(PromptAnswer {
            prompt: prompt.id.clone(),
            button: pressed.id.clone(),
            values: answers,
            user: UserRef {
                id: user,
                display_name: state.user.clone(),
                is_bot: false,
            },
            conversation: key,
            sent: sent.clone(),
        })
    }
}

/// Where chart images go by default.
fn chart_dir() -> PathBuf {
    std::env::temp_dir().join("okbot-console")
}

/// A card as Markdown: a bold title with the tone's emoji, then the text (as the runtime prints
/// cards on platforms without them).
fn card_text(card: &okbot_core::Card) -> String {
    let mut out = String::new();
    if let Some(title) = &card.title {
        let emoji = card.tone.emoji();
        let gap = if emoji.is_empty() { "" } else { " " };
        let _ = write!(out, "**{emoji}{gap}{title}**");
    }
    if !card.text.trim().is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(card.text.trim_end());
    }
    out
}

/// Terminal text with control characters other than line breaks escaped: a reply built from web
/// pages or tool results cannot move the cursor, set the window title or write the clipboard.
fn printable(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// An attachment for a path or URL, by its extension.
fn attachment(target: &str) -> Attachment {
    let name = target
        .rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    let ext = target
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let url = target.to_string();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => Attachment::Image { url, caption: None },
        "mp3" | "m4a" | "ogg" | "oga" | "wav" | "aac" => Attachment::Audio { url },
        _ => Attachment::File { url, name },
    }
}

#[async_trait]
impl Channel for ConsoleChannel {
    fn id(&self) -> &ChannelId {
        &self.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        let Some(input) = self.input.lock().await.take() else {
            return Err(ChannelError::Unsupported("the console can only run once"));
        };
        let mut lines = input.lines();
        // The transcript's length at the last message or button press, for `:wait`/`:expect`.
        let mut mark = 0;
        loop {
            let line = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                line = lines.next_line() => line.map_err(io_error)?,
            };
            let Some(line) = line else { break };
            match line::parse(&line) {
                None => {}
                Some(Line::Say(text)) => {
                    mark = *self.shown.borrow();
                    if sink.deliver(self.event(text, false)).await.is_err() {
                        return Ok(());
                    }
                }
                Some(Line::Command(Command::Paste)) => {
                    // Every line up to `:end` is one message.
                    let mut text = Vec::new();
                    loop {
                        let line = tokio::select! {
                            () = shutdown.cancelled() => return Ok(()),
                            line = lines.next_line() => line.map_err(io_error)?,
                        };
                        match line {
                            Some(line) if line.trim() != ":end" => text.push(line),
                            _ => break,
                        }
                    }
                    let text = text.join("\n");
                    if !text.trim().is_empty() {
                        mark = *self.shown.borrow();
                        if sink.deliver(self.event(text, false)).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Some(Line::Command(command)) => {
                    if matches!(command, Command::Restricted | Command::Press { .. }) {
                        mark = *self.shown.borrow();
                    }
                    if !self.command(command, &sink, &shutdown, mark).await? {
                        return Ok(());
                    }
                }
                Some(Line::Invalid(e)) => self.note(&e).await?,
            }
        }
        self.closed.cancel();
        shutdown.cancelled().await;
        Ok(())
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let id = self.next_id();
        let what = self.caps.edit.then(|| id.as_str().to_string());
        self.print(&self.tags(&to.conversation, what), &message.text)
            .await?;
        self.shown(false, &message.text);
        Ok(SentRef {
            target: to.clone(),
            message_id: id,
        })
    }

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        let what = format!("edit {}", sent.message_id.as_str());
        self.print(
            &self.tags(&sent.target.conversation, Some(what)),
            &message.text,
        )
        .await?;
        self.shown(false, &message.text);
        Ok(())
    }

    async fn typing(&self, to: &ReplyTarget) -> Result<(), ChannelError> {
        if !self.caps.typing || !self.state().typing.insert(to.conversation.clone()) {
            return Ok(());
        }
        let tags: Vec<String> = Self::label(&to.conversation)
            .into_iter()
            .chain(["typing…".to_string()])
            .collect();
        self.print(&tags, "").await
    }

    async fn prompt(&self, to: &ReplyTarget, prompt: &Prompt) -> Result<SentRef, ChannelError> {
        let preset = self.options.preset;
        if !preset.buttons() {
            return Err(ChannelError::Unsupported("buttons"));
        }
        if !preset.forms() && prompt.buttons.iter().any(|b| b.form.is_some()) {
            return Err(ChannelError::Unsupported("forms"));
        }
        let id = self.next_id();
        let mut parts = Vec::new();
        if !prompt.text.trim().is_empty() {
            parts.push(prompt.text.trim_end().to_string());
        }
        for card in &prompt.cards {
            let mut part = card_text(card);
            // Empty bytes: a prompt read back from the store; its chart was written before.
            if let Some(image) = card.image.as_ref().filter(|i| !i.png.is_empty()) {
                let path = self.save_chart(&prompt.id, &image.png).await?;
                if !part.is_empty() {
                    part.push('\n');
                }
                let _ = write!(part, "[chart: {}] {}", path.display(), image.alt);
            }
            if !part.is_empty() {
                parts.push(part);
            }
        }
        let mut text = parts.join("\n\n");
        text.push('\n');
        for (i, b) in prompt.buttons.iter().enumerate() {
            let _ = write!(text, "\n  [{}] {}", i + 1, b.label);
            if let Some(form) = &b.form {
                let fields: Vec<String> = form
                    .fields
                    .iter()
                    .map(|f| format!("{}{}", f.id, if f.required { "*" } else { "" }))
                    .collect();
                let _ = write!(text, " (form: {})", fields.join(", "));
            }
        }
        // Cards alone (a view without choices) cannot be answered.
        let answerable = !prompt.buttons.is_empty();
        if answerable {
            text.push_str("\n\n(:press <n> to answer)");
        }
        let tags = self.tags(&to.conversation, Some(format!("prompt {}", id.as_str())));
        self.print(&tags, &text).await?;
        let sent = SentRef {
            target: to.clone(),
            message_id: id,
        };
        if answerable {
            self.state().prompts.push((prompt.clone(), sent.clone()));
        }
        self.shown(answerable, &text);
        Ok(sent)
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        message: &RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        if !keep_buttons {
            self.state()
                .prompts
                .retain(|(_, s)| s.message_id != sent.message_id);
        }
        let what = format!(
            "prompt {} {}",
            sent.message_id.as_str(),
            if keep_buttons { "updated" } else { "closed" }
        );
        self.print(
            &self.tags(&sent.target.conversation, Some(what)),
            &message.text,
        )
        .await?;
        self.shown(false, &message.text);
        Ok(())
    }

    async fn set_commands(&self, commands: &[CommandInfo]) -> Result<(), ChannelError> {
        let mut text = String::from("the bot's commands:");
        for c in commands {
            let _ = write!(text, "\n  /{}", c.name);
            for p in &c.params {
                let dots = if p.rest { "..." } else { "" };
                let _ = if p.required {
                    write!(text, " <{}{dots}>", p.name)
                } else {
                    write!(text, " [{}{dots}]", p.name)
                };
            }
            let _ = write!(text, "  {}", c.description);
        }
        self.note(&text).await
    }

    async fn open_direct(&self, user: &UserId) -> Result<ConversationKey, ChannelError> {
        Ok(ConversationKey::room(
            self.id.clone(),
            self.dm_room(user.as_str()),
        ))
    }
}

#[cfg(test)]
mod tests;
