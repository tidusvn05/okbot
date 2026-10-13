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
    /// `/snooze [10m|1h|15:30]`: send the last reminder of this chat again later.
    Snooze(String),
    /// `/resume <number|id>`: start a paused task (routines need this confirmation).
    Resume(String),
    /// `/pause <number|id>`: stop a task from firing until resumed.
    Pause(String),
    /// `/note <text>`: append to today's journal.
    Note(String),
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
            // `/new` and `/clear` are unlisted aliases people know from other chat assistants.
            "reset" | "new" | "clear" => Some(Self::Reset),
            "whoami" => Some(Self::WhoAmI),
            "stop" | "cancel" => Some(Self::Stop),
            "remember" => Some(Self::Remember(rest)),
            "forget" => Some(Self::Forget(rest)),
            "memories" | "memory" => Some(Self::Memories),
            "pair" => Some(Self::Pair(rest)),
            "tasks" | "todo" | "todos" => Some(Self::Tasks(rest.eq_ignore_ascii_case("all"))),
            "done" => Some(Self::Done(rest)),
            "snooze" | "later" => Some(Self::Snooze(rest)),
            "resume" | "start" => Some(Self::Resume(rest)),
            "pause" => Some(Self::Pause(rest)),
            "note" => Some(Self::Note(rest)),
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
        Command::Tasks(_) | Command::Done(_) | Command::Resume(_) | Command::Pause(_) => {
            tasks(s, command, event).await
        }
        Command::Snooze(when) => snooze(s, &when, event).await,
        Command::Note(text) => note(s, &text, event).await,
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
    match &command {
        Command::Done(which) | Command::Resume(which) | Command::Pause(which) => {
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
            let mut change = okbot_core::TaskChange::default();
            let verb = match &command {
                Command::Resume(_) => {
                    // A routine runs the agent unattended: only an administrator starts one.
                    if target.kind == okbot_core::TaskKind::Routine && !s.is_admin(event).await {
                        return "Only the owner of this assistant can start a routine.".into();
                    }
                    change.status = Some(okbot_core::TaskStatus::Open);
                    // A time that passed while paused moves to the next occurrence.
                    if target.repeat.is_some()
                        && target.remind_at.is_none_or(|at| at <= s.clock.now())
                    {
                        change.remind_at = Some(None);
                    }
                    "Started"
                }
                Command::Pause(_) => {
                    change.status = Some(okbot_core::TaskStatus::Paused);
                    "Paused"
                }
                _ => {
                    change.status = Some(okbot_core::TaskStatus::Done);
                    "Done"
                }
            };
            match store.update(&who, &target.id, change).await {
                Ok(task) => match task.remind_at.filter(|_| verb == "Started") {
                    Some(at) => format!(
                        "{verb}: {} (next: {})",
                        task.title,
                        at.to_zoned(tz.clone()).strftime("%H:%M %d/%m")
                    ),
                    None => format!("{verb}: {}", task.title),
                },
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
                format!(
                    "{}\n\n`/done <number>` finishes one; `/pause` and `/resume` stop and start one.",
                    shown.join("\n")
                )
            }
        }
    }
}

/// When a snooze ends: `10m`, `2h`, `1d`, `10 phút`, `2 giờ`, a bare number of minutes, a local
/// clock time (`15:30`, today or tomorrow) or a date-time. Ten minutes when empty.
pub(crate) fn snooze_until(
    text: &str,
    now: jiff::Timestamp,
    tz: &jiff::tz::TimeZone,
) -> Option<jiff::Timestamp> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return now.checked_add(jiff::SignedDuration::from_mins(10)).ok();
    }
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    let unit = text[digits.len()..].trim();
    if let Ok(n) = digits.parse::<i64>()
        && !unit.starts_with(':')
    {
        let minutes = match unit {
            "" | "m" | "min" | "mins" | "minute" | "minutes" | "p" | "phút" => n,
            "h" | "hour" | "hours" | "g" | "giờ" | "tiếng" => n.checked_mul(60)?,
            "d" | "day" | "days" | "ngày" => n.checked_mul(24 * 60)?,
            _ => return None,
        };
        if !(1..=60 * 24 * 30).contains(&minutes) {
            return None;
        }
        return now
            .checked_add(jiff::SignedDuration::from_mins(minutes))
            .ok();
    }
    if let Ok(time) = text.parse::<jiff::civil::Time>() {
        let today = now.to_zoned(tz.clone()).date();
        let at = today
            .to_datetime(time)
            .to_zoned(tz.clone())
            .ok()?
            .timestamp();
        return if at > now {
            Some(at)
        } else {
            let tomorrow = today.tomorrow().ok()?.to_datetime(time);
            Some(tomorrow.to_zoned(tz.clone()).ok()?.timestamp())
        };
    }
    okbot_core::parse_time(&text, tz).ok().filter(|t| *t > now)
}

/// The task id of the newest reminder sent in this conversation: the scheduler ends its history
/// line with "(reminder `t-…`; …)". Users write user messages, so only bot lines count.
async fn last_reminder(s: &Shared, event: &InboundEvent) -> Option<String> {
    let history = s.store.recent(&event.conversation, 40).await.ok()?;
    history
        .iter()
        .rev()
        .filter(|m| m.role == okbot_core::MessageRole::Bot)
        .find_map(|m| {
            let (_, rest) = m.text.rsplit_once("\n\n(reminder `")?;
            let (id, tail) = rest.split_once('`')?;
            (tail == "; the user may snooze it or mark it done)").then(|| id.to_string())
        })
}

async fn snooze(s: &Shared, when: &str, event: &InboundEvent) -> String {
    let Some(store) = &s.tasks else {
        return "Tasks are not enabled for this bot.".into();
    };
    let tz = s.config.time_zone();
    let Some(until) = snooze_until(when, s.clock.now(), &tz) else {
        return "When? For example `/snooze 10m`, `/snooze 2h` or `/snooze 15:30`.".into();
    };
    let Some(id) = last_reminder(s, event).await else {
        return "There is no reminder in this chat to snooze.".into();
    };
    let who = requester(s, event).await;
    // An agent's answer could imitate the scheduler's line; a routine only starts with `/resume`.
    let is_routine = store.list(&who).await.is_ok_and(|tasks| {
        tasks.iter().any(|t| {
            t.kind == okbot_core::TaskKind::Routine && t.id.rsplit('/').next() == Some(id.as_str())
        })
    });
    if is_routine {
        return "Routines cannot be snoozed; use `/pause` and `/resume`.".into();
    }
    let change = okbot_core::TaskChange {
        status: Some(okbot_core::TaskStatus::Open),
        remind_at: Some(Some(until)),
        ..okbot_core::TaskChange::default()
    };
    match store.update(&who, &id, change).await {
        Ok(task) => format!(
            "Okay, I'll remind you again at {}: {}",
            until.to_zoned(tz).strftime("%H:%M %d/%m"),
            task.title
        ),
        Err(okbot_core::TaskError::Backend(error)) => {
            tracing::error!(%error, "snoozing failed");
            s.config.replies.error.clone()
        }
        Err(other) => other.to_string(),
    }
}

async fn note(s: &Shared, text: &str, event: &InboundEvent) -> String {
    let Some(memory) = &s.memory else {
        return "Notes are not enabled for this bot.".into();
    };
    if text.trim().is_empty() {
        return "What should I note? For example `/note call the bank on Monday`.".into();
    }
    let who = requester(s, event).await;
    match memory.note(&who, okbot_core::MemoryScope::User, text).await {
        Ok(_) => "Noted.".into(),
        Err(okbot_core::MemoryError::Backend(error)) => {
            tracing::error!(%error, "writing a note failed");
            s.config.replies.error.clone()
        }
        Err(other) => other.to_string(),
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
         - `/snooze [10m|2h|15:30]`: remind me again of the last reminder\n\
         - `/pause <number>`, `/resume <number>`: stop or start a task; new routines start paused\n\
         - `/note <text>`: add a line to today's notes\n\
         - `/pair <code>`: on a personal assistant, add this account to the owner's (`okbot pair`)\n\
         - `/help`: this message"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snooze_times() {
        let tz = jiff::tz::TimeZone::get("Asia/Ho_Chi_Minh").unwrap();
        // 14:00 local.
        let now: jiff::Timestamp = "2026-10-14T07:00:00Z".parse().unwrap();
        let at = |text: &str| snooze_until(text, now, &tz).map(|t| t.to_string());
        assert_eq!(at("").as_deref(), Some("2026-10-14T07:10:00Z"));
        assert_eq!(at("30").as_deref(), Some("2026-10-14T07:30:00Z"));
        assert_eq!(at("10 phút").as_deref(), Some("2026-10-14T07:10:00Z"));
        assert_eq!(at("2h").as_deref(), Some("2026-10-14T09:00:00Z"));
        assert_eq!(at("1 ngày").as_deref(), Some("2026-10-15T07:00:00Z"));
        assert_eq!(at("15:30").as_deref(), Some("2026-10-14T08:30:00Z"));
        assert_eq!(
            at("09:00").as_deref(),
            Some("2026-10-15T02:00:00Z"),
            "tomorrow"
        );
        assert_eq!(at("0"), None);
        assert_eq!(at("soon"), None);
    }

    #[test]
    fn parses_commands() {
        assert_eq!(Command::parse("/help"), Some(Command::Help));
        assert_eq!(Command::parse("  /Reset please"), Some(Command::Reset));
        assert_eq!(Command::parse("/clear"), Some(Command::Reset));
        assert_eq!(Command::parse("/new"), Some(Command::Reset));
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
        assert_eq!(
            Command::parse("/snooze"),
            Some(Command::Snooze(String::new()))
        );
        assert_eq!(
            Command::parse("/resume 1"),
            Some(Command::Resume("1".into()))
        );
        assert_eq!(
            Command::parse("/pause t-x"),
            Some(Command::Pause("t-x".into()))
        );
        assert_eq!(
            Command::parse("/note gọi ngân hàng"),
            Some(Command::Note("gọi ngân hàng".into()))
        );
        assert_eq!(Command::parse("help"), None);
        assert_eq!(Command::parse("/unknown"), None);
        assert_eq!(Command::parse(""), None);
    }
}
