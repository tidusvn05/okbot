//! Google Chat interaction events → [`InboundEvent`]s. Both the classic Chat app format
//! (`{"type": "MESSAGE", "message": …}`) and the Workspace add-on format
//! (`{"chat": {"messagePayload": …}}`) are accepted.

use jiff::Timestamp;
use okbot_core::{ChannelId, ConversationKey, InboundEvent, MessageId, UserId, UserRef};
use serde_json::Value;

/// `(message, space)` of a message event, or `None` for other events.
fn message_parts(event: &Value) -> Option<(&Value, &Value)> {
    if let Some(payload) = event.pointer("/chat/messagePayload") {
        return Some((&payload["message"], &payload["space"]));
    }
    if event["type"].as_str() == Some("MESSAGE") {
        let message = &event["message"];
        let space = if event["space"].is_object() {
            &event["space"]
        } else {
            &message["space"]
        };
        return Some((message, space));
    }
    None
}

/// Converts an event. Messages from other bots are kept (the runtime ignores them).
pub(crate) fn to_inbound(
    channel: &ChannelId,
    event: &Value,
    now: Timestamp,
) -> Option<InboundEvent> {
    let (message, space) = message_parts(event)?;
    let space_name = space["name"]
        .as_str()
        .or_else(|| message["space"]["name"].as_str())?;
    let direct = matches!(space["spaceType"].as_str(), Some("DIRECT_MESSAGE"))
        || matches!(space["type"].as_str(), Some("DM"))
        || space["singleUserBotDm"].as_bool() == Some(true);
    let sender = &message["sender"];
    let user = sender["name"].as_str().unwrap_or("users/unknown");
    // `argumentText` is the text without the app's @mention.
    let text = message["argumentText"]
        .as_str()
        .or_else(|| message["text"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let thread = message["thread"]["name"].as_str().map(str::to_string);
    let name = message["name"].as_str().map(str::to_string);
    let sent_at = message["createTime"]
        .as_str()
        .and_then(|t| t.parse::<Timestamp>().ok())
        .unwrap_or(now);
    Some(InboundEvent {
        event_id: name
            .clone()
            .unwrap_or_else(|| format!("{space_name}:{}", sent_at.as_millisecond())),
        conversation: ConversationKey {
            channel: channel.clone(),
            space: None,
            room: space_name.to_string(),
            // Each thread of a space is its own conversation; direct messages are one.
            thread: if direct { None } else { thread },
        },
        author: UserRef {
            id: UserId::new(user),
            display_name: sender["displayName"].as_str().unwrap_or(user).to_string(),
            is_bot: sender["type"].as_str() == Some("BOT"),
        },
        text,
        attachments: Vec::new(),
        // In spaces, Chat delivers messages to an app only when it is @mentioned.
        mentions_bot: !direct,
        is_direct: direct,
        restricted: false,
        message_id: name.map(MessageId::new),
        sent_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel() -> ChannelId {
        "gchat:corp".parse().unwrap()
    }

    #[test]
    fn classic_space_message() {
        let event = json!({
            "type": "MESSAGE",
            "message": {
                "name": "spaces/AAA/messages/M1",
                "sender": {"name": "users/111", "displayName": "An", "type": "HUMAN"},
                "text": "@okbot giá gói Pro?", "argumentText": " giá gói Pro?",
                "thread": {"name": "spaces/AAA/threads/T1"},
                "createTime": "2026-10-12T03:00:00Z"
            },
            "space": {"name": "spaces/AAA", "type": "ROOM", "spaceType": "SPACE"}
        });
        let e = to_inbound(&channel(), &event, Timestamp::UNIX_EPOCH).unwrap();
        assert_eq!(e.text, "giá gói Pro?");
        assert_eq!(e.conversation.room, "spaces/AAA");
        assert_eq!(
            e.conversation.thread.as_deref(),
            Some("spaces/AAA/threads/T1")
        );
        assert!(e.mentions_bot && !e.is_direct);
        assert_eq!(e.event_id, "spaces/AAA/messages/M1");
        assert_eq!(e.author.id.as_str(), "users/111");
    }

    #[test]
    fn add_on_direct_message() {
        let event = json!({"chat": {"messagePayload": {
            "space": {"name": "spaces/DM1", "spaceType": "DIRECT_MESSAGE"},
            "message": {"name": "spaces/DM1/messages/M2", "text": "hello",
                        "sender": {"name": "users/222", "displayName": "Bình", "type": "HUMAN"},
                        "thread": {"name": "spaces/DM1/threads/X"}}}}});
        let e = to_inbound(&channel(), &event, Timestamp::UNIX_EPOCH).unwrap();
        assert!(e.is_direct);
        assert_eq!(e.conversation.thread, None);
        assert_eq!(e.text, "hello");
    }

    #[test]
    fn other_events_are_ignored() {
        let added = json!({"type": "ADDED_TO_SPACE", "space": {"name": "spaces/A"}});
        assert!(to_inbound(&channel(), &added, Timestamp::UNIX_EPOCH).is_none());
    }
}
