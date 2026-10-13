//! `okbot task`: to-dos, reminders and routines from the terminal (design §5.6).

use std::fmt::Write as _;

use anyhow::{Context, bail};
use okbot_config::{BotProfile, Config};
use okbot_core::{
    ChannelId, ConversationKey, MemoryScope, NewTask, Person, Requester, Task, TaskChange,
    TaskKind, TaskStatus, TaskStore, UserRef, parse_time,
};

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
    },
    /// Add a to-do or a reminder for the owner (personal bots) or a linked person.
    Add {
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
        /// Person from `[people]` on a team bot.
        #[arg(long)]
        person: Option<String>,
        /// Free notes.
        #[arg(long, default_value = "")]
        notes: String,
    },
    /// Mark a task as done.
    Done {
        /// Task id (`t-7k2m9q` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Delete a task.
    #[command(alias = "remove")]
    Rm {
        /// Task id (`t-7k2m9q` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
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

pub(crate) async fn run(command: TaskCommand) -> anyhow::Result<()> {
    match command {
        TaskCommand::List {
            name,
            all,
            upcoming,
        } => {
            let (_, k) = crate::memory::open(name.as_deref()).await?;
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
            if tasks.is_empty() {
                println!("no tasks");
            }
            for t in &tasks {
                println!("{}", line(t, &tz));
            }
        }
        TaskCommand::Add {
            title,
            bot,
            remind,
            due,
            repeat,
            to,
            person,
            notes,
        } => {
            let (config, k) = crate::memory::open(bot.as_deref()).await?;
            let who = requester(&config, person.as_deref())?;
            let tz = k.timezone().clone();
            let time = |v: Option<String>| -> anyhow::Result<Option<jiff::Timestamp>> {
                v.map(|v| parse_time(&v, &tz).map_err(anyhow::Error::msg))
                    .transpose()
            };
            let repeat = repeat
                .map(|r| r.parse().map_err(anyhow::Error::msg))
                .transpose()?;
            let remind_at = time(remind)?;
            let new = NewTask {
                kind: if remind_at.is_some() || repeat.is_some() {
                    TaskKind::Reminder
                } else {
                    TaskKind::Todo
                },
                title,
                notes,
                scope: MemoryScope::User,
                due: time(due)?,
                remind_at,
                repeat,
                deliver_to: to
                    .map(|c| c.parse().map_err(|e| anyhow::anyhow!("--to: {e}")))
                    .transpose()?,
                paused: false,
            };
            let task = TaskStore::add(k.as_ref(), &who, new).await?;
            println!("added\n{}", line(&task, &tz));
        }
        TaskCommand::Done { id, bot } => {
            let (_, k) = crate::memory::open(bot.as_deref()).await?;
            let id = resolve(&k, &id).await?;
            let change = TaskChange {
                status: Some(TaskStatus::Done),
                ..TaskChange::default()
            };
            let task = k.change_task(&id, change).await?;
            println!("done: {}", task.title);
        }
        TaskCommand::Rm { id, bot } => {
            let (_, k) = crate::memory::open(bot.as_deref()).await?;
            let id = resolve(&k, &id).await?;
            k.remove_task(&id).await?;
            println!("removed {id}");
        }
    }
    Ok(())
}
