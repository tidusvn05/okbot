//! Where a notification goes (`[[notify]] to`, ADR 0010).

use std::fmt;
use std::str::FromStr;

use crate::id::{ChannelId, ConversationKey, IdError, UserId};

/// One destination of a `[[notify]]` target, written as text in `okbot.toml`.
///
/// | Text | Meaning |
/// |---|---|
/// | `room:<conversation>` | a room, chat, space or thread, as `/whoami` shows it |
/// | `user:<platform>:<instance>/<user id>` | that account's direct chat |
/// | `person:<id>` | one direct chat of a person from `[people]` (`owner` on a personal bot) |
/// | `admins` | the direct chat of every `[bot] admins` account (and the owner's) |
///
/// A room is the exact [`ConversationKey`] text, so threads and workspaces work on every
/// platform without a platform-specific address format.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NotifyTo {
    /// A conversation the bot posts into.
    Room(ConversationKey),
    /// One account's direct chat.
    User(ChannelId, UserId),
    /// A linked person's direct chat on one of their platforms.
    Person(String),
    /// Every administrator's direct chat.
    Admins,
}

impl NotifyTo {
    /// The text that addresses the conversation `key` (`room:<key>`).
    pub fn room_address(key: &ConversationKey) -> String {
        format!("room:{key}")
    }
}

impl FromStr for NotifyTo {
    type Err = IdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = |why: &str| IdError::NotifyTarget(format!("`{value}`: {why}"));
        if value == "admins" {
            return Ok(Self::Admins);
        }
        if let Some(room) = value.strip_prefix("room:") {
            return room
                .parse()
                .map(Self::Room)
                .map_err(|_| invalid("send /whoami in the room and copy its `room:` address"));
        }
        if let Some(account) = value.strip_prefix("user:") {
            let (channel, user) = account
                .split_once('/')
                .filter(|(_, user)| !user.is_empty())
                .ok_or_else(|| invalid("write `user:<platform>:<instance>/<user id>`"))?;
            let channel = channel
                .parse()
                .map_err(|e: IdError| invalid(&e.to_string()))?;
            return Ok(Self::User(channel, UserId::new(user)));
        }
        if let Some(person) = value.strip_prefix("person:") {
            let slug = !person.is_empty()
                && person.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                });
            return if slug {
                Ok(Self::Person(person.to_string()))
            } else {
                Err(invalid("a person id uses a-z, 0-9, `-` and `_`"))
            };
        }
        Err(invalid(
            "use `room:<conversation>`, `user:<platform>:<instance>/<id>`, `person:<id>` or `admins`",
        ))
    }
}

impl fmt::Display for NotifyTo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Room(key) => write!(f, "room:{key}"),
            Self::User(channel, user) => write!(f, "user:{channel}/{user}"),
            Self::Person(id) => write!(f, "person:{id}"),
            Self::Admins => f.write_str("admins"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_round_trip() {
        for text in [
            "admins",
            "person:luan",
            "user:zalo:shop/6ede9afa",
            "room:discord:ops|1111|2222|-",
            "room:gchat:corp|-|spaces/AAA|spaces/AAA/threads/T1",
            "room:slack:corp|T1|C2|1700000000.000100",
        ] {
            let target: NotifyTo = text.parse().unwrap();
            assert_eq!(target.to_string(), text);
        }
    }

    #[test]
    fn rooms_are_conversation_keys() {
        let key = ConversationKey::room("telegram:main".parse().unwrap(), "-100123");
        let address = NotifyTo::room_address(&key);
        assert_eq!(address, "room:telegram:main|-|-100123|-");
        assert_eq!(address.parse::<NotifyTo>().unwrap(), NotifyTo::Room(key));
    }

    #[test]
    fn bad_targets_say_how_to_write_them() {
        for text in [
            "",
            "everyone",
            "room:discord:ops/123",
            "user:zalo:shop",
            "user:Zalo:shop/1",
            "person:",
            "person:Luan",
        ] {
            let error = text.parse::<NotifyTo>().unwrap_err().to_string();
            assert!(error.contains(&format!("`{text}`")), "{error}");
        }
    }
}
