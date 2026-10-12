//! Chat commands handled without an agent.

use okbot_core::InboundEvent;

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
            _ => None,
        }
    }
}

/// Runs a command other than `/stop` and returns the reply.
pub(crate) async fn execute(s: &Shared, command: Command, event: &InboundEvent) -> String {
    match command {
        Command::Help => help_text(&s.config.bot_name),
        Command::WhoAmI => format!(
            "You are `{}` ({}) in chat `{}` on `{}`.{}",
            event.author.id,
            event.author.display_name,
            event.conversation.room,
            event.conversation.channel,
            if s.gate.is_admin(event) {
                " You are an administrator."
            } else {
                ""
            }
        ),
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
    }
}

async fn memory(s: &Shared, command: Command, event: &InboundEvent) -> String {
    let Some(memory) = &s.memory else {
        return "Memory is not enabled for this bot.".into();
    };
    let room = s
        .config
        .room(&event.conversation.channel, &event.conversation.room);
    let who = okbot_core::Requester {
        conversation: event.conversation.clone(),
        user: event.author.clone(),
        internal: room.internal,
    };
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
        assert_eq!(Command::parse("help"), None);
        assert_eq!(Command::parse("/unknown"), None);
        assert_eq!(Command::parse(""), None);
    }
}
