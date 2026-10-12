//! Slack Events API payloads → [`InboundEvent`]s.

use jiff::Timestamp;
use okbot_core::{ChannelId, ConversationKey, InboundEvent, MessageId, UserId, UserRef};
use serde_json::Value;

/// Converts an `events_api` payload. Edits, bot messages and the bot's own messages are ignored.
pub(crate) fn to_inbound(channel: &ChannelId, payload: &Value, me: &str) -> Option<InboundEvent> {
    let event = &payload["event"];
    let kind = event["type"].as_str()?;
    let direct = event["channel_type"].as_str() == Some("im");
    match kind {
        "app_mention" => {}
        "message" if direct => {}
        _ => return None,
    }
    if event.get("subtype").is_some_and(|s| !s.is_null())
        || event.get("bot_id").is_some_and(|b| !b.is_null())
    {
        return None;
    }
    let user = event["user"].as_str()?;
    if user == me {
        return None;
    }
    let room = event["channel"].as_str()?;
    let ts = event["ts"].as_str()?;
    let text = event["text"]
        .as_str()
        .unwrap_or_default()
        .replace(&format!("<@{me}>"), "")
        .trim()
        .to_string();
    let sent_at = ts
        .split('.')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|s| Timestamp::from_second(s).ok())
        .unwrap_or_else(Timestamp::now);
    Some(InboundEvent {
        event_id: payload["event_id"].as_str().unwrap_or(ts).to_string(),
        conversation: ConversationKey {
            channel: channel.clone(),
            space: payload["team_id"].as_str().map(str::to_string),
            room: room.to_string(),
            // Channel mentions are answered in a thread, which is the conversation.
            thread: if direct {
                None
            } else {
                Some(event["thread_ts"].as_str().unwrap_or(ts).to_string())
            },
        },
        author: UserRef {
            id: UserId::new(user),
            display_name: user.to_string(),
            is_bot: false,
        },
        text,
        attachments: Vec::new(),
        mentions_bot: kind == "app_mention",
        is_direct: direct,
        restricted: false,
        message_id: Some(MessageId::new(ts)),
        sent_at,
    })
}
