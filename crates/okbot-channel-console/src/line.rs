//! Input lines: chat text, or console commands that start with `:`.
//!
//! Console commands use `:` so they never collide with the bot's own `/` commands, which are sent
//! to the bot like any other text.

use std::time::Duration;

/// What one input line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// Text for the bot.
    Say(String),
    /// A console command.
    Command(Command),
    /// A console command that could not be read; the message says why.
    Invalid(String),
}

/// A console command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Where,
    As(String),
    Room(String),
    Dm,
    Thread(Option<String>),
    Attach(String),
    Restricted,
    Press {
        button: usize,
        values: Vec<(String, String)>,
    },
    Wait(Option<Duration>),
    Expect {
        text: String,
        present: bool,
    },
    Paste,
}

pub(crate) const HELP: &str = "\
Console commands (everything else is sent to the bot, including its /commands):
  :as <user>              write as another user (their own direct chat, or the current room)
  :room <name>            move to a group room; mention the bot with @<bot name>
  :dm                     back to the current user's direct chat
  :thread [name]          write in a thread of the room (no name: leave the thread)
  :attach <path or url>   attach a file to the next message
  :restricted             send a message whose content the platform withheld
  :press <n> [id=value]   press button n of the last prompt here (form values after it)
  :paste                  the lines up to :end are one message
  :wait [seconds]         wait until the bot has answered everything (for scripts)
  :expect <text>          wait, then check the answer to the last message contains text
  :expect-not <text>      wait, then check it does not (failures make okbot chat exit 1)
  :where                  show the user, room and platform
  :help                   this list
Start a line with :: to send text that starts with a colon.";

/// Reads one line; `None` for a blank one.
pub(crate) fn parse(line: &str) -> Option<Line> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix("::") {
        return Some(Line::Say(format!(":{rest}")));
    }
    let Some(rest) = line
        .strip_prefix(':')
        .filter(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))
    else {
        return Some(Line::Say(line.to_string()));
    };
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let args = args.trim();
    Some(match command(name, args) {
        Ok(c) => Line::Command(c),
        Err(e) => Line::Invalid(e),
    })
}

fn command(name: &str, args: &str) -> Result<Command, String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let none = |c: Command| {
        if words.is_empty() {
            Ok(c)
        } else {
            Err(format!(":{name} takes no arguments"))
        }
    };
    match name {
        "help" => none(Command::Help),
        "where" => none(Command::Where),
        "dm" => none(Command::Dm),
        "restricted" => none(Command::Restricted),
        "paste" => none(Command::Paste),
        "expect" | "expect-not" if !args.is_empty() => Ok(Command::Expect {
            text: args.to_string(),
            present: name == "expect",
        }),
        "expect" | "expect-not" => Err(format!("usage: :{name} <text>")),
        "as" => match words[..] {
            [user] => Ok(Command::As(user.to_string())),
            _ => Err("usage: :as <user>".into()),
        },
        "room" => match words[..] {
            [room] if room == "console" || room.starts_with("dm-") => Err(format!(
                "`{room}` is reserved for direct chats; pick another room name"
            )),
            [room] => Ok(Command::Room(room.trim_start_matches('#').to_string())),
            _ => Err("usage: :room <name>".into()),
        },
        "thread" => match words[..] {
            [] => Ok(Command::Thread(None)),
            [thread] => Ok(Command::Thread(Some(thread.to_string()))),
            _ => Err("usage: :thread [name]".into()),
        },
        "attach" if !args.is_empty() => Ok(Command::Attach(args.to_string())),
        "attach" => Err("usage: :attach <path or url>".into()),
        "press" => {
            let usage = || "usage: :press <n> [field=value ...]".to_string();
            let (n, rest) = words.split_first().ok_or_else(usage)?;
            let button = n
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(usage)?;
            let values = rest
                .iter()
                .map(|w| {
                    w.split_once('=')
                        .filter(|(k, _)| !k.is_empty())
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(usage)
                })
                .collect::<Result<_, _>>()?;
            Ok(Command::Press { button, values })
        }
        "wait" => match words[..] {
            [] => Ok(Command::Wait(None)),
            [secs] => secs
                .parse::<u64>()
                .map(|s| Command::Wait(Some(Duration::from_secs(s))))
                .map_err(|_| "usage: :wait [seconds]".into()),
            _ => Err("usage: :wait [seconds]".into()),
        },
        _ => Err(format!(
            "unknown console command `:{name}` (:help lists them)"
        )),
    }
}

/// Removes mentions of `bot` (`@okbot`, `@okbot,`) from `text`, line by line; true if there was
/// one.
pub(crate) fn strip_mention(text: &str, bot: &str) -> (String, bool) {
    let is_mention = |w: &str| {
        w.strip_prefix('@')
            .map(|n| n.trim_end_matches([',', ':', ';', '.', '!', '?']))
            .is_some_and(|n| n.eq_ignore_ascii_case(bot))
    };
    if !text.split_whitespace().any(is_mention) {
        return (text.to_string(), false);
    }
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            if line.split_whitespace().any(is_mention) {
                let words: Vec<&str> = line.split_whitespace().filter(|w| !is_mention(w)).collect();
                words.join(" ")
            } else {
                line.to_string()
            }
        })
        .collect();
    (lines.join("\n").trim().to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> Command {
        match parse(line) {
            Some(Line::Command(c)) => c,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn invalid(line: &str) -> String {
        match parse(line) {
            Some(Line::Invalid(e)) => e,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn text_and_bot_commands_go_to_the_bot() {
        assert_eq!(parse("  "), None);
        assert_eq!(parse(" hi "), Some(Line::Say("hi".into())));
        assert_eq!(parse("/help"), Some(Line::Say("/help".into())));
        assert_eq!(parse(":)"), Some(Line::Say(":)".into())));
        assert_eq!(parse("::as x"), Some(Line::Say(":as x".into())));
    }

    #[test]
    fn reads_console_commands() {
        assert_eq!(cmd(":as alice"), Command::As("alice".into()));
        assert_eq!(cmd(":room #sales"), Command::Room("sales".into()));
        assert_eq!(cmd(":dm"), Command::Dm);
        assert_eq!(cmd(":thread t1"), Command::Thread(Some("t1".into())));
        assert_eq!(cmd(":thread"), Command::Thread(None));
        assert_eq!(
            cmd(":attach docs/price list.pdf"),
            Command::Attach("docs/price list.pdf".into())
        );
        assert_eq!(
            cmd(":press 2 reason=late code=7"),
            Command::Press {
                button: 2,
                values: vec![
                    ("reason".into(), "late".into()),
                    ("code".into(), "7".into())
                ],
            }
        );
        assert_eq!(cmd(":paste"), Command::Paste);
        assert_eq!(
            cmd(":expect-not  out of  stock "),
            Command::Expect {
                text: "out of  stock".into(),
                present: false
            }
        );
        assert_eq!(cmd(":wait"), Command::Wait(None));
        assert_eq!(cmd(":wait 5"), Command::Wait(Some(Duration::from_secs(5))));
    }

    #[test]
    fn explains_bad_commands() {
        assert!(invalid(":nope").contains(":help"));
        assert_eq!(invalid(":as"), "usage: :as <user>");
        assert!(invalid(":room dm-bob").contains("reserved"));
        assert!(invalid(":press 0").starts_with("usage"));
        assert!(invalid(":press 1 novalue").starts_with("usage"));
        assert_eq!(invalid(":dm now"), ":dm takes no arguments");
        assert_eq!(invalid(":expect"), "usage: :expect <text>");
    }

    #[test]
    fn strips_bot_mentions() {
        assert_eq!(
            strip_mention("@OKBot, what is the price?", "okbot"),
            ("what is the price?".into(), true)
        );
        assert_eq!(
            strip_mention("@okbot\n  keep  this\nand this", "okbot"),
            ("keep  this\nand this".into(), true)
        );
        assert_eq!(
            strip_mention("ask @okbotx  later", "okbot"),
            ("ask @okbotx  later".into(), false)
        );
    }
}
