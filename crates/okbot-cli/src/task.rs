//! `okbot task`: to-dos, reminders and routines from the terminal (design §5.6).

use std::fmt::Write as _;

use anyhow::{Context, bail};
use okbot_config::{BotProfile, Config};
use okbot_core::{
    ChannelId, ConversationKey, MemoryScope, NewTask, Person, Requester, Task, TaskChange,
    TaskKind, TaskStatus, TaskStore, UserRef, parse_time,
};
use serde_json::{Value, json};

/// `okbot task` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum TaskCommand {
    /// List tasks of every person and chat (open and paused; `--all` adds finished ones).
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
        /// Include done and cancelled tasks.
        #[arg(long)]
        all: bool,
        /// Only tasks with a coming reminder, soonest first.
        #[arg(long)]
        upcoming: bool,
        /// Print a JSON array (for scripts and MCP servers; the text output may change).
        #[arg(long)]
        json: bool,
    },
    /// Add a to-do or a reminder for the owner (personal bots) or a linked person.
    Add(Box<AddArgs>),
    /// Stop a task from firing until resumed.
    Pause {
        /// Task id (`t-7k2m9q` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print the task as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start a paused task (routines created in chat start paused).
    Resume {
        /// Task id (`t-7k2m9q` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print the task as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Mark a task as done.
    Done {
        /// Task id (`t-7k2m9q` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print the task as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete a task.
    #[command(alias = "remove")]
    Rm {
        /// Task id (`t-7k2m9q` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print the task as JSON.
        #[arg(long)]
        json: bool,
    },
}

impl TaskCommand {
    /// Whether errors should be reported as JSON.
    pub(crate) fn json(&self) -> bool {
        match self {
            Self::List { json, .. }
            | Self::Pause { json, .. }
            | Self::Resume { json, .. }
            | Self::Done { json, .. }
            | Self::Rm { json, .. } => *json,
            Self::Add(args) => args.json,
        }
    }
}

/// `okbot task add` options.
#[derive(Debug, clap::Args)]
pub(crate) struct AddArgs {
    /// Title.
    title: String,
    /// Bot name; default: the bot in the current directory.
    #[arg(long)]
    bot: Option<String>,
    /// Send a reminder at this local time (`2026-10-14 08:00`).
    #[arg(long)]
    remind: Option<String>,
    /// Due time or date (`2026-10-17 17:00`, `2026-10-17`).
    #[arg(long)]
    due: Option<String>,
    /// Repeat rule: `daily 08:00`, `weekdays 08:00`, `weekly mon,fri 09:00`,
    /// `monthly 1 09:00` or `every 3h`.
    #[arg(long)]
    repeat: Option<String>,
    /// Channel to send the reminder to (`discord:main`); default: the owner's latest direct chat.
    #[arg(long)]
    to: Option<String>,
    /// Person from `[people]` on a team bot (wins over `--run-token`).
    #[arg(long)]
    person: Option<String>,
    /// For the person behind a signed run token (ADR 0007): an `[[mcp]] auth = "signed"` server
    /// passes the token it received, so the task is theirs without `--person`.
    #[arg(long, env = "OKBOT_RUN_TOKEN", hide_env_values = true)]
    run_token: Option<String>,
    /// Free notes.
    #[arg(long, default_value = "")]
    notes: String,
    /// Make it a routine: at each time the agent carries out this instruction (needs
    /// --repeat or --remind), for example "Summarise my tasks for today".
    #[arg(long, conflicts_with = "notes")]
    routine: Option<String>,
    /// Print the created task as JSON.
    #[arg(long)]
    json: bool,
}

/// The terminal acting for `person`: tasks land in the person's folder, like a chat of theirs.
fn requester(config: &Config, person: Option<&str>) -> anyhow::Result<Requester> {
    let id = match (config.bot.profile, person) {
        (BotProfile::Personal, None) => okbot_runtime::OWNER.to_string(),
        (_, Some(id)) => {
            if config.bot.profile != BotProfile::Personal && !config.people.contains_key(id) {
                bail!("no person `{id}` in [people]; `okbot people list` shows them");
            }
            id.to_string()
        }
        (BotProfile::Team, None) => {
            bail!("a team bot has no owner: pass --person <id> from [people] (`okbot people list`)")
        }
    };
    let accounts = config
        .people
        .get(&id)
        .map(|p| {
            p.accounts
                .iter()
                .filter_map(|a| okbot_config::parse_account(a).ok())
                .collect()
        })
        .unwrap_or_default();
    let channel: ChannelId = "console:cli".parse().expect("a valid channel id");
    Ok(Requester {
        conversation: ConversationKey::room(channel, "terminal"),
        user: UserRef {
            id: "operator".into(),
            display_name: "operator".into(),
            is_bot: false,
        },
        internal: true,
        person: Some(Person { id, accounts }),
        personal: true,
    })
}

/// The person behind a signed run token of one of this bot's `[[mcp]] auth = "signed"` servers:
/// the owner on a personal bot (it answers no one else), the linked `[people]` entry on a team bot.
fn person_from_token(config: &Config, token: &str) -> anyhow::Result<String> {
    let invalid = || anyhow::anyhow!("--run-token is not a valid, unexpired run token of this bot");
    let server = okbot_runtime::identity::claimed_server(token).ok_or_else(invalid)?;
    let m = config
        .mcp
        .iter()
        .find(|m| m.name == server && m.auth == okbot_config::McpAuth::Signed)
        .ok_or_else(invalid)?;
    let key = config.secret(&m.token_env)?;
    let claims = okbot_runtime::identity::verify(
        key.expose().as_bytes(),
        token,
        jiff::Timestamp::now().as_second(),
    )
    .ok_or_else(invalid)?;
    if claims.bot != config.bot.name || claims.mcp != m.name {
        return Err(invalid());
    }
    if config.bot.profile == BotProfile::Personal {
        return Ok(okbot_runtime::OWNER.to_string());
    }
    let (channel, user) = okbot_config::parse_account(&claims.user).map_err(|_| invalid())?;
    config
        .people
        .iter()
        .find(|(_, p)| {
            p.accounts
                .iter()
                .filter_map(|a| okbot_config::parse_account(a).ok())
                .any(|(c, u)| c == channel && u == user)
        })
        .map(|(id, _)| id.clone())
        .with_context(|| {
            format!(
                "`{}` is not linked to a person: `okbot people link <person> {}`",
                claims.user, claims.user
            )
        })
}

/// A task as JSON: the contract of `--json` (times in UTC RFC 3339 and in the bot's zone).
pub(crate) fn task_json(task: &Task, tz: &jiff::tz::TimeZone) -> Value {
    let time = |t: Option<jiff::Timestamp>| {
        t.map(|t| {
            json!({
                "utc": t.to_string(),
                "local": t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M").to_string(),
            })
        })
    };
    // `tasks/people/luan/t-7k2m9q` → owner `people/luan`, person `luan`.
    let folder = task
        .id
        .strip_prefix("tasks/")
        .unwrap_or(&task.id)
        .rsplit_once('/')
        .map_or("", |(folder, _)| folder);
    let person = folder.strip_prefix("people/").filter(|p| !p.contains('/'));
    json!({
        "id": task.id,
        "short_id": task.id.rsplit('/').next().unwrap_or(&task.id),
        "kind": task.kind.as_str(),
        "state": task.status.as_str(),
        "title": task.title,
        "notes": task.notes,
        "scope": match task.scope {
            MemoryScope::User => "user",
            MemoryScope::Room => "room",
            MemoryScope::Global => "global",
        },
        "remind": time(task.remind_at),
        "due": time(task.due),
        "repeat": task.repeat.as_ref().map(ToString::to_string),
        "deliver_to": task.deliver_to.as_ref().map(ToString::to_string),
        "owner": folder,
        "person": person,
        "author": task.author,
        "created": task.created.to_string(),
    })
}

fn line(task: &Task, tz: &jiff::tz::TimeZone) -> String {
    let local = |t: jiff::Timestamp| {
        t.to_zoned(tz.clone())
            .strftime("%Y-%m-%d %H:%M")
            .to_string()
    };
    let mut out = format!(
        "{}  [{}, {}] {}",
        task.id,
        task.kind.as_str(),
        task.status.as_str(),
        task.title
    );
    if let Some(at) = task.remind_at {
        let _ = write!(out, "\n    remind {}", local(at));
        if let Some(r) = &task.repeat {
            let _ = write!(out, " ({r})");
        }
    }
    if let Some(due) = task.due {
        let _ = write!(out, "\n    due {}", local(due));
    }
    out
}

/// The full id of a task given its short or full id.
async fn resolve(k: &okbot_knowledge::Knowledge, id: &str) -> anyhow::Result<String> {
    k.all_tasks()
        .await
        .into_iter()
        .find(|t| t.id == id || t.id.rsplit('/').next() == Some(id))
        .map(|t| t.id)
        .with_context(|| format!("no task `{id}`; `okbot task list` shows them"))
}

async fn list(name: Option<&str>, all: bool, upcoming: bool, json: bool) -> anyhow::Result<()> {
    let (_, k) = crate::memory::open(name).await?;
    let tz = k.timezone().clone();
    let mut tasks: Vec<Task> = k
        .all_tasks()
        .await
        .into_iter()
        .filter(|t| all || t.status.is_active())
        .filter(|t| !upcoming || t.remind_at.is_some() && t.status == TaskStatus::Open)
        .collect();
    if upcoming {
        tasks.sort_by_key(|t| t.remind_at);
    }
    if json {
        let tasks: Vec<Value> = tasks.iter().map(|t| task_json(t, &tz)).collect();
        println!("{}", Value::Array(tasks));
        return Ok(());
    }
    if tasks.is_empty() {
        println!("no tasks");
    }
    for t in &tasks {
        println!("{}", line(t, &tz));
    }
    Ok(())
}

async fn add(a: AddArgs) -> anyhow::Result<()> {
    let (config, k) = crate::memory::open(a.bot.as_deref()).await?;
    let person = match a.run_token.as_deref() {
        Some(token) if a.person.is_none() => Some(person_from_token(&config, token)?),
        _ => a.person,
    };
    let who = requester(&config, person.as_deref())?;
    let tz = k.timezone().clone();
    let time = |v: Option<String>| -> anyhow::Result<Option<jiff::Timestamp>> {
        v.map(|v| parse_time(&v, &tz).map_err(anyhow::Error::msg))
            .transpose()
    };
    let repeat = a
        .repeat
        .map(|r| r.parse().map_err(anyhow::Error::msg))
        .transpose()?;
    let remind_at = time(a.remind)?;
    let kind = if a.routine.is_some() {
        TaskKind::Routine
    } else if remind_at.is_some() || repeat.is_some() {
        TaskKind::Reminder
    } else {
        TaskKind::Todo
    };
    let new = NewTask {
        kind,
        title: a.title,
        notes: a.routine.unwrap_or(a.notes),
        scope: MemoryScope::User,
        due: time(a.due)?,
        remind_at,
        repeat,
        deliver_to: a
            .to
            .map(|c| c.parse().map_err(|e| anyhow::anyhow!("--to: {e}")))
            .transpose()?,
        paused: false,
    };
    let task = TaskStore::add(k.as_ref(), &who, new).await?;
    if a.json {
        println!("{}", task_json(&task, &tz));
    } else {
        println!("added\n{}", line(&task, &tz));
    }
    Ok(())
}

/// Sets a task's state: `done`, `paused` or (resumed) `open`.
async fn set_state(
    id: &str,
    bot: Option<&str>,
    state: TaskStatus,
    json: bool,
) -> anyhow::Result<()> {
    let (_, k) = crate::memory::open(bot).await?;
    let id = resolve(&k, id).await?;
    let current = k
        .all_tasks()
        .await
        .into_iter()
        .find(|t| t.id == id)
        .with_context(|| format!("no task `{id}`"))?;
    let mut change = TaskChange {
        status: Some(state),
        ..TaskChange::default()
    };
    // A time that passed while paused moves to the next occurrence.
    if state == TaskStatus::Open
        && current.repeat.is_some()
        && current
            .remind_at
            .is_none_or(|at| at <= jiff::Timestamp::now())
    {
        change.remind_at = Some(None);
    }
    let task = k.change_task(&id, change).await?;
    let word = match state {
        TaskStatus::Open => "resumed",
        other => other.as_str(),
    };
    if json {
        println!("{}", task_json(&task, k.timezone()));
    } else {
        println!("{word}\n{}", line(&task, k.timezone()));
    }
    Ok(())
}

pub(crate) async fn run(command: TaskCommand) -> anyhow::Result<()> {
    match command {
        TaskCommand::List {
            name,
            all,
            upcoming,
            json,
        } => list(name.as_deref(), all, upcoming, json).await,
        TaskCommand::Add(args) => add(*args).await,
        TaskCommand::Done { id, bot, json } => {
            set_state(&id, bot.as_deref(), TaskStatus::Done, json).await
        }
        TaskCommand::Pause { id, bot, json } => {
            set_state(&id, bot.as_deref(), TaskStatus::Paused, json).await
        }
        TaskCommand::Resume { id, bot, json } => {
            set_state(&id, bot.as_deref(), TaskStatus::Open, json).await
        }
        TaskCommand::Rm { id, bot, json } => {
            let (_, k) = crate::memory::open(bot.as_deref()).await?;
            let id = resolve(&k, &id).await?;
            k.remove_task(&id).await?;
            if json {
                println!("{}", json!({ "removed": id }));
            } else {
                println!("removed {id}");
            }
            Ok(())
        }
    }
}
