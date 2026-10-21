//! Read-only local history commands.

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use okbot_config::Config;
use okbot_core::{ChannelId, MessageRole};
use okbot_store::{HistoryQuery, HistoryReader};
use serde_json::{Value, json};
use std::fmt::Write;

#[derive(Debug, Subcommand)]
pub(crate) enum HistoryCommand {
    /// List conversations, newest activity first.
    List(ReadArgs),
    /// Show retained messages, including before the latest /reset (oldest first within the page).
    Show {
        /// Conversation id from `history list`.
        #[arg(value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
        #[command(flatten)]
        args: ReadArgs,
    },
    /// Find literal substrings with Unicode lowercase matching, newest matches first.
    Search {
        /// Text to find (not a regular expression).
        #[arg(value_parser = nonempty)]
        text: String,
        /// Search only this conversation id.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        conversation: Option<i64>,
        #[command(flatten)]
        args: ReadArgs,
    },
    /// Read the existing agent-turn audit, newest first; unknown model/usage stay null.
    Runs {
        /// Only this conversation id.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        conversation: Option<i64>,
        /// Only failed or cancelled turns.
        #[arg(long)]
        errors: bool,
        #[command(flatten)]
        args: ReadArgs,
    },
}

fn nonempty(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("search text must not be empty".into())
    } else {
        Ok(s.into())
    }
}

#[derive(Debug, Args)]
pub(crate) struct ReadArgs {
    /// Bot name or path; default: the current directory's bot.
    #[arg(long)]
    bot: Option<String>,
    /// Exact channel instance (`discord:main`).
    #[arg(long)]
    channel: Option<ChannelId>,
    /// Inclusive lower time bound, RFC3339 with an offset.
    #[arg(long)]
    since: Option<Timestamp>,
    /// Exclusive upper time bound, RFC3339 with an offset.
    #[arg(long)]
    until: Option<Timestamp>,
    /// Page size. Show selects the newest page before displaying it oldest first.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    limit: u32,
    /// Matching records to skip from the newest end.
    #[arg(long, default_value_t = 0)]
    offset: u32,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

impl HistoryCommand {
    fn args(&self) -> &ReadArgs {
        match self {
            Self::List(a)
            | Self::Show { args: a, .. }
            | Self::Search { args: a, .. }
            | Self::Runs { args: a, .. } => a,
        }
    }
    pub(crate) fn json(&self) -> bool {
        self.args().json
    }
}

fn time(at: Timestamp, config: &Config) -> String {
    config
        .bot
        .timezone
        .as_deref()
        .and_then(|name| at.in_tz(name).ok())
        .map_or_else(
            || at.to_string(),
            |z| z.strftime("%Y-%m-%d %H:%M:%S %:z").to_string(),
        )
}

fn read(
    command: &HistoryCommand,
    reader: &HistoryReader,
    query: &HistoryQuery,
    config: &Config,
) -> anyhow::Result<(Value, String)> {
    let show = matches!(command, HistoryCommand::Show { .. });
    let mut text = String::new();
    let items = match command {
        HistoryCommand::List(_) => {
            let rows = reader.conversations(query)?;
            for r in &rows {
                writeln!(
                    text,
                    "{}  channel={}  room={}  thread={}  space={}  messages={}  {}",
                    r.id,
                    r.conversation.channel,
                    r.conversation.room,
                    r.conversation.thread.as_deref().unwrap_or("-"),
                    r.conversation.space.as_deref().unwrap_or("-"),
                    r.messages,
                    time(r.updated_at, config)
                )?;
            }
            serde_json::to_value(rows)?
        }
        HistoryCommand::Show { .. } | HistoryCommand::Search { .. } => {
            if show {
                let exists = reader.conversations(&HistoryQuery {
                    conversation: query.conversation,
                    ..HistoryQuery::default()
                })?;
                if exists.is_empty() {
                    bail!(
                        "no conversation {}; use `okbot history list`",
                        query.conversation.unwrap_or_default()
                    );
                }
            }
            let mut rows = reader.messages(query)?;
            if show {
                rows.reverse();
            }
            let mut before = None;
            for r in &rows {
                if show && before != Some(r.before_reset) {
                    text.push_str(if r.before_reset {
                        "--- before latest /reset ---\n"
                    } else {
                        "--- current context ---\n"
                    });
                    before = Some(r.before_reset);
                }
                writeln!(
                    text,
                    "{}  conversation={}  {} {}\n{}\n",
                    time(r.at, config),
                    r.conversation_id,
                    if r.role == MessageRole::Bot {
                        "bot"
                    } else {
                        "user"
                    },
                    r.author_name,
                    r.text
                )?;
            }
            serde_json::to_value(rows)?
        }
        HistoryCommand::Runs { .. } => {
            let rows = reader.runs(query)?;
            for r in &rows {
                writeln!(
                    text,
                    "{}  {}  {}  model={}  {}ms  {}\n  conversation={}  usage={}",
                    time(r.started_at, config),
                    r.run_id,
                    r.backend,
                    r.model.as_deref().unwrap_or("unknown (CLI default)"),
                    r.duration_ms,
                    r.error.as_deref().unwrap_or("ok"),
                    r.conversation,
                    serde_json::to_string(&r.usage)?
                )?;
            }
            serde_json::to_value(rows)?
        }
    };
    Ok((items, text))
}

pub(crate) async fn run(command: HistoryCommand) -> anyhow::Result<()> {
    let args = command.args();
    if args.since.zip(args.until).is_some_and(|(a, b)| a >= b) {
        bail!("--since must be earlier than --until");
    }
    let config = Config::load(&crate::registry::resolve(args.bot.as_deref())?)?;
    let path = config.state_dir().join("okbot.db");
    let json_output = args.json;
    let mut query = HistoryQuery {
        channel: args.channel.clone(),
        since: args.since,
        until: args.until,
        limit: args.limit as usize,
        offset: args.offset as usize,
        ..HistoryQuery::default()
    };
    let kind = match &command {
        HistoryCommand::List(_) => "conversations",
        HistoryCommand::Show { id, .. } => {
            query.conversation = Some(*id);
            "messages"
        }
        HistoryCommand::Search {
            text, conversation, ..
        } => {
            query.search = Some(text.clone());
            query.conversation = *conversation;
            "search"
        }
        HistoryCommand::Runs {
            conversation,
            errors,
            ..
        } => {
            query.conversation = *conversation;
            query.errors_only = *errors;
            "runs"
        }
    };
    let limit = query.limit;
    let offset = query.offset;
    let config_for_read = config.clone();
    let redactor = crate::operator::redactor(Some(&config));
    let (items, mut text) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<(Value, String)> {
            if !path.try_exists()? {
                return Ok((json!([]), "no retained history yet\n".into()));
            }
            let reader = HistoryReader::open(&path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            read(&command, &reader, &query, &config_for_read)
        })
        .await?
        .map_err(|e| crate::operator::redact_error(&e, &redactor))?;
    if text.is_empty() {
        text.push_str("no matching history\n");
    }
    crate::operator::emit(
        json!({"ok": true, "bot": config.bot.name, "kind": kind,
        "items": items, "limit": limit, "offset": offset}),
        &text,
        json_output,
        &redactor,
    );
    Ok(())
}
