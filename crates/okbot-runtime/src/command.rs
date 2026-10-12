//! Chat commands handled without an agent.

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
}

impl Command {
    /// Parses a message. Only the first word counts; `/Help` and `/help@bot` are accepted.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let word = text.split_whitespace().next()?;
        let word = word.strip_prefix('/')?;
        let word = word.split('@').next().unwrap_or(word).to_ascii_lowercase();
        match word.as_str() {
            "help" => Some(Self::Help),
            "reset" | "new" => Some(Self::Reset),
            "whoami" => Some(Self::WhoAmI),
            "stop" | "cancel" => Some(Self::Stop),
            _ => None,
        }
    }
}

/// The `/help` text.
pub(crate) fn help_text(bot_name: &str) -> String {
    format!(
        "**{bot_name}** answers your questions using its knowledge base and the web.\n\n\
         Commands:\n\
         - `/reset`: forget this conversation and start over\n\
         - `/stop`: stop the answer being written\n\
         - `/whoami`: show the ids I see for you and this chat\n\
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
        assert_eq!(Command::parse("help"), None);
        assert_eq!(Command::parse("/unknown"), None);
        assert_eq!(Command::parse(""), None);
    }
}
