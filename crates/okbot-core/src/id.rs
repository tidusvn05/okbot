//! Identifiers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An invalid identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    /// The value is empty.
    #[error("{kind} is empty")]
    Empty {
        /// Which identifier.
        kind: &'static str,
    },
    /// The value contains characters outside the allowed set.
    #[error("{kind} `{value}` may only contain a-z, 0-9, `-` and `_`")]
    Charset {
        /// Which identifier.
        kind: &'static str,
        /// The rejected value.
        value: String,
    },
    /// A channel id is not `platform:instance`.
    #[error("channel id `{0}` must look like `platform:instance`, for example `zalo:shop`")]
    ChannelFormat(String),
}

fn check_slug(kind: &'static str, value: &str) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty { kind });
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(IdError::Charset {
            kind,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// One configured instance of a chat platform, written `platform:instance` (`zalo:shop`).
///
/// Both parts are lowercase slugs, so the id is safe in file paths, logs and okbase paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChannelId {
    platform: String,
    instance: String,
}

impl ChannelId {
    /// Builds an id from its parts.
    pub fn new(platform: &str, instance: &str) -> Result<Self, IdError> {
        check_slug("platform", platform)?;
        check_slug("instance", instance)?;
        Ok(Self {
            platform: platform.to_string(),
            instance: instance.to_string(),
        })
    }

    /// The platform part (`zalo`).
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// The instance part (`shop`).
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// `platform-instance`, for places where `:` is not allowed (okbase paths).
    pub fn slug(&self) -> String {
        format!("{}-{}", self.platform, self.instance)
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.platform, self.instance)
    }
}

impl FromStr for ChannelId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (platform, instance) = s
            .split_once(':')
            .ok_or_else(|| IdError::ChannelFormat(s.to_string()))?;
        Self::new(platform, instance)
    }
}

impl TryFrom<String> for ChannelId {
    type Error = IdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ChannelId> for String {
    fn from(value: ChannelId) -> Self {
        value.to_string()
    }
}

macro_rules! opaque_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps a platform-provided value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The raw value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

opaque_id!(
    /// A user id as the platform reports it, unique within one [`ChannelId`].
    UserId
);
opaque_id!(
    /// A message id as the platform reports it.
    MessageId
);
opaque_id!(
    /// The id of one agent run, used in logs and the audit table.
    RunId
);

/// Who wrote a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRef {
    /// Platform user id.
    pub id: UserId,
    /// Name to show and to address the user by.
    pub display_name: String,
    /// Whether the author is a bot (okbot ignores bots by default).
    pub is_bot: bool,
}

/// Identifies one conversation. Messages with the same key are handled in order and share history.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConversationKey {
    /// The channel instance.
    pub channel: ChannelId,
    /// Workspace, guild or Google Chat space, when the platform has one above rooms.
    pub space: Option<String>,
    /// Chat, channel or direct-message id.
    pub room: String,
    /// Thread inside the room, when the platform has threads.
    pub thread: Option<String>,
}

impl ConversationKey {
    /// A key for a room without space or thread (Zalo chats, console).
    pub fn room(channel: ChannelId, room: impl Into<String>) -> Self {
        Self {
            channel,
            space: None,
            room: room.into(),
            thread: None,
        }
    }
}

/// Stable text form used as a storage key: parts joined by `|`, with `\` and `|` escaped and `-`
/// standing for a missing part (a literal `-` is escaped as `\-`).
impl fmt::Display for ConversationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn part(value: Option<&str>) -> String {
            match value {
                None => "-".to_string(),
                Some(v) => {
                    let escaped = v.replace('\\', "\\\\").replace('|', "\\|");
                    if escaped == "-" {
                        "\\-".to_string()
                    } else {
                        escaped
                    }
                }
            }
        }
        write!(
            f,
            "{}|{}|{}|{}",
            self.channel,
            part(self.space.as_deref()),
            part(Some(&self.room)),
            part(self.thread.as_deref())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn channel_id_round_trip() {
        let id: ChannelId = "zalo:shop".parse().unwrap();
        assert_eq!(id.platform(), "zalo");
        assert_eq!(id.instance(), "shop");
        assert_eq!(id.to_string(), "zalo:shop");
        assert_eq!(id.slug(), "zalo-shop");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"zalo:shop\"");
        assert_eq!(serde_json::from_str::<ChannelId>(&json).unwrap(), id);
    }

    #[test]
    fn channel_id_rejects_bad_values() {
        assert!("zalo".parse::<ChannelId>().is_err());
        assert!("Zalo:shop".parse::<ChannelId>().is_err());
        assert!("zalo:".parse::<ChannelId>().is_err());
        assert!("zalo:a/b".parse::<ChannelId>().is_err());
    }

    #[test]
    fn conversation_key_text_is_stable() {
        let channel: ChannelId = "gchat:corp".parse().unwrap();
        let key = ConversationKey {
            channel,
            space: Some("spaces/A".into()),
            room: "r|1".into(),
            thread: None,
        };
        assert_eq!(key.to_string(), r"gchat:corp|spaces/A|r\|1|-");
    }

    proptest! {
        #[test]
        fn conversation_key_text_is_injective(
            a in proptest::option::of(".{0,6}"), b in ".{0,6}", c in proptest::option::of(".{0,6}"),
            d in proptest::option::of(".{0,6}"), e in ".{0,6}", g in proptest::option::of(".{0,6}"),
        ) {
            let channel: ChannelId = "x:y".parse().unwrap();
            let k1 = ConversationKey { channel: channel.clone(), space: a, room: b, thread: c };
            let k2 = ConversationKey { channel, space: d, room: e, thread: g };
            prop_assert_eq!(k1 == k2, k1.to_string() == k2.to_string());
        }
    }
}
