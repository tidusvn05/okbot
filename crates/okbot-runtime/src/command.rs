//! Chat commands handled without an agent.

use okbot_core::InboundEvent;
use std::fmt::Write as _;

use crate::Shared;

/// A recognized command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// `/help`
    Help,
    /// `/reset`: start a fresh conversation history.
    Reset,
    /// `/whoami`: show the ids the bot sees.
    WhoAmI,
    /// `/stop`: cancel the running answer.
    Stop,
    /// `/remember <text>`: save a personal note.
    Remember(String),
    /// `/forget <id>`: remove a note.
    Forget(String),
    /// `/memories`: list the notes visible here.
    Memories,
    /// `/pair <code>`: make this account the owner's, on a personal bot.
    Pair(String),
    /// `/tasks [all]`: list to-dos and reminders.
    Tasks(bool),
    /// `/done <number|id>`: finish a task.
    Done(String),
}

impl Command {
    /// Parses a message. Only the first word counts; `/Help` and `/help@bot` are accepted.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let word = text.split_whitespace().next()?;
        let rest = text[word.len()..].trim().to_string();
        let word = word.strip_prefix('/')?;
        let word = word.split('@').next().unwrap_or(word).to_ascii_lowercase();
        match word.as_str() {
            "help" => Some(Self::Help),
            "reset" | "new" => Some(Self::Reset),
            "whoami" => Some(Self::WhoAmI),
            "stop" | "cancel" => Some(Self::Stop),
            "remember" => Some(Self::Remember(rest)),
            "forget" => Some(Self::Forget(rest)),
            "memories" | "memory" => Some(Self::Memories),
            "pair" => Some(Self::Pair(rest)),
            "tasks" | "todo" | "todos" => Some(Self::Tasks(rest.eq_ignore_ascii_case("all"))),
            "done" => Some(Self::Done(rest)),
            _ => None,
        }
    }
}

/// Runs a command other than `/stop` and returns the reply.
pub(crate) async fn execute(s: &Shared, command: Command, event: &InboundEvent) -> String {
    match command {
        Command::Help => help_text(&s.config.bot_name),
        Command::WhoAmI => whoami(s, event).await,
        // Personal bots handle `/pair` before commands reach a conversation (dispatch).
        Command::Pair(_) => {
            if s.config.personal {
                "This account is already paired.".into()
            } else {
                "Pairing is for personal assistants (`okbot profile set personal`).".into()
            }
        }
        Command::Reset => match s.store.reset(&event.conversation).await {
            Ok(()) => "Okay, I started a new conversation.".to_string(),
            Err(error) => {
                tracing::error!(%error, "reset failed");
                s.config.replies.error.clone()
            }
        },
        Command::Stop => "Nothing is running.".to_string(),
        Command::Remember(_) | Command::Forget(_) | Command::Memories => {
            memory(s, command, event).await
        }
        Command::Tasks(_) | Command::Done(_) => tasks(s, command, event).await,
    }
}

async fn whoami(s: &Shared, event: &InboundEvent) -> String {
    let account = format!("{}/{}", event.conversation.channel, event.author.id);
    let mut reply = format!(
        "You are `{account}` ({}) in chat `{}`.",
        event.author.display_name, event.conversation.room
    );
    match s.person_of(event).await {
        Some(person) if s.config.personal => {
            let _ = write!(
                reply,
                " You are the owner of this assistant ({} paired account(s), one shared memory).",
                person.accounts.len()
            );
        }
        Some(person) => {
            let _ = write!(
                reply,
                " This account belongs to `{}`, who shares memory across {} linked account(s).",
                person.id,
                person.accounts.len()
            );
        }
        None => {
            let _ = write!(
                reply,
                " To share memory with your other accounts, the operator runs \
                 `okbot people link <name> {account}`."
            );
        }
    }
    if s.is_admin(event).await {
        reply.push_str(" You are an administrator.");
    }
    reply
}

async fn requester(s: &Shared, event: &InboundEvent) -> okbot_core::Requester {
    let room = s
        .config
        .room(&event.conversation.channel, &event.conversation.room);
    okbot_core::Requester {
        conversation: event.conversation.clone(),
        user: event.author.clone(),
        internal: room.internal,
        person: s.person_of(event).await,
    }
}

/// A task as one list line: title, then when it fires or is due.
fn task_line(task: &okbot_core::Task, tz: &jiff::tz::TimeZone) -> String {
    let local = |t: jiff::Timestamp| {
        t.to_zoned(tz.clone())
            .strftime("%a %d/%m %H:%M")
            .to_string()
    };
    let mut line = task.title.clone();
    if let Some(at) = task.remind_at {
        let _ = write!(line, " · ⏰ {}", local(at));
    }
    if let Some(repeat) = &task.repeat {
        let _ = write!(line, " ({repeat})");
    }
    if let Some(due) = task.due {
        let _ = write!(line, " · due {}", local(due));
    }
    if !task.status.is_active() {
        let _ = write!(line, " · {}", task.status.as_str());
    } else if task.status == okbot_core::TaskStatus::Paused {
        line.push_str(" · paused");
    }
    let short = task.id.rsplit('/').next().unwrap_or(&task.id);
    let _ = write!(line, " `{short}`");
    line
}

async fn tasks(s: &Shared, command: Command, event: &InboundEvent) -> String {
    let Some(store) = &s.tasks else {
        return "Tasks are not enabled for this bot.".into();
    };
    let who = requester(s, event).await;
    let tz = s.config.time_zone();
    let listed = match store.list(&who).await {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::error!(%error, "listing tasks failed");
            return s.config.replies.error.clone();
        }
    };
    match command {
        Command::Done(which) => {
            let which = which.trim();
            let active: Vec<&okbot_core::Task> =
                listed.iter().filter(|t| t.status.is_active()).collect();
            let target = which
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| active.get(i).copied())
                .or_else(|| {
                    listed
                        .iter()
                        .find(|t| !which.is_empty() && t.id.rsplit('/').next() == Some(which))
                });
            let Some(target) = target else {
                return "Which task? `/tasks` lists them; use its number, like `/done 2`.".into();
            };
            let change = okbot_core::TaskChange {
                status: Some(okbot_core::TaskStatus::Done),
                ..okbot_core::TaskChange::default()
            };
            match store.update(&who, &target.id, change).await {
                Ok(task) => format!("Done: {}", task.title),
                Err(okbot_core::TaskError::Backend(error)) => {
                    tracing::error!(%error, "finishing a task failed");
                    s.config.replies.error.clone()
                }
                Err(other) => other.to_string(),
            }
        }
        _ => {
            let all = matches!(command, Command::Tasks(true));
            let shown: Vec<String> = listed
                .iter()
                .filter(|t| all || t.status.is_active())
                .enumerate()
                .map(|(i, t)| format!("{}. {}", i + 1, task_line(t, &tz)))
                .collect();
            if shown.is_empty() {
                "No tasks. Ask me to remind you of something or to add a to-do.".into()
            } else {
                format!("{}\n\n`/done <number>` finishes one.", shown.join("\n"))
            }
        }
    }
}

async fn memory(s: &Shared, command: Command, event: &InboundEvent) -> String {
    let Some(memory) = &s.memory else {
        return "Memory is not enabled for this bot.".into();
    };
    let who = requester(s, event).await;
    let result = match command {
        Command::Remember(text) if text.trim().is_empty() => {
            return "Tell me what to remember, for example `/remember I prefer short answers`."
                .into();
        }
        Command::Remember(text) => memory
            .save(
                &who,
                okbot_core::MemoryNote {
                    scope: okbot_core::MemoryScope::User,
                    title: None,
                    text,
                },
            )
            .await
            .map(|e| format!("Okay, I'll remember that (`{}`).", e.id)),
        Command::Forget(id) if id.trim().is_empty() => {
            return "Which note? `/memories` lists them with their ids.".into();
        }
        Command::Forget(id) => memory
            .forget(&who, &id)
            .await
            .map(|e| format!("Forgotten: {}", e.title)),
        _ => memory.list(&who).await.map(|entries| {
            if entries.is_empty() {
                "I don't remember anything here yet.".to_string()
            } else {
                entries
                    .iter()
                    .map(|e| format!("- `{}`: {}", e.id, e.text.replace('\n', " ")))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }),
    };
    result.unwrap_or_else(|error| match error {
        okbot_core::MemoryError::Backend(e) => {
            tracing::error!(error = %e, "memory command failed");
            s.config.replies.error.clone()
        }
        other => other.to_string(),
    })
}

/// The `/help` text.
pub(crate) fn help_text(bot_name: &str) -> String {
    format!(
        "**{bot_name}** answers your questions using its knowledge base and the web.\n\n\
         Commands:\n\
         - `/reset`: forget this conversation and start over\n\
         - `/stop`: stop the answer being written\n\
         - `/whoami`: show the ids I see for you and this chat\n\
         - `/remember <text>`: remember something about you\n\
         - `/memories`: show what I remember here\n\
         - `/forget <id>`: forget a note (ids are listed by `/memories`)\n\
         - `/tasks`: your to-dos and reminders (`/tasks all` includes finished ones)\n\
         - `/done <number>`: mark a task as done\n\
         - `/pair <code>`: on a personal assistant, add this account to the owner's (`okbot pair`)\n\
         - `/help`: this message"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(Command::parse("/help"), Some(Command::Help));
        assert_eq!(Command::parse("  /Reset please"), Some(Command::Reset));
        assert_eq!(Command::parse("/stop@okbot"), Some(Command::Stop));
        assert_eq!(
            Command::parse("/remember  tôi thích trà  "),
            Some(Command::Remember("tôi thích trà".into()))
        );
        assert_eq!(
            Command::parse("/forget memory/x"),
            Some(Command::Forget("memory/x".into()))
        );
        assert_eq!(Command::parse("/memories"), Some(Command::Memories));
        assert_eq!(Command::parse("/tasks"), Some(Command::Tasks(false)));
        assert_eq!(Command::parse("/todo ALL"), Some(Command::Tasks(true)));
        assert_eq!(Command::parse("/done 2"), Some(Command::Done("2".into())));
        assert_eq!(Command::parse("help"), None);
        assert_eq!(Command::parse("/unknown"), None);
        assert_eq!(Command::parse(""), None);
    }
}
