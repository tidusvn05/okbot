//! Zalo events → [`InboundEvent`]s.

use jiff::Timestamp;
use okbot_core::{
    Attachment, ChannelId, ConversationKey, InboundEvent, MessageId, UserId, UserRef,
};
use serde_json::Value;

/// Converts one update (`{"event_name": …, "message": …}`, optionally wrapped in
/// `{"ok": true, "result": …}`) into an event. `None` for updates okbot does not handle.
pub(crate) fn to_inbound(
    channel: &ChannelId,
    update: &Value,
    now: Timestamp,
) -> Option<InboundEvent> {
    let update = if update.get("event_name").is_some() {
        update
    } else {
        update.get("result")?
    };
    let name = update["event_name"].as_str()?;
    let message = &update["message"];
    let chat_id = message["chat"]["id"].as_str()?;
    let from = &message["from"];
    let user_id = from["id"].as_str().unwrap_or(chat_id);
    let message_id = message["message_id"].as_str().map(MessageId::new);
    let direct = message["chat"]["chat_type"].as_str() != Some("GROUP");
    let sent_at = message["date"]
        .as_i64()
        .and_then(|ms| Timestamp::from_millisecond(ms).ok())
        .unwrap_or(now);

    let mut text = message["text"].as_str().unwrap_or_default().to_string();
    let mut attachments = Vec::new();
    let mut restricted = false;
    match name {
        "message.text.received" => {}
        "message.image.received" => {
            let caption = message["caption"].as_str().map(str::to_string);
            if let Some(url) = message["photo"].as_str() {
                attachments.push(Attachment::Image {
                    url: url.to_string(),
                    caption: caption.clone(),
                });
            }
            if text.is_empty() {
                text = caption.unwrap_or_else(|| "[image]".into());
            }
        }
        "message.sticker.received" => {
            let id = message["sticker"]
                .as_str()
                .or_else(|| message["url"].as_str())
                .unwrap_or_default();
            attachments.push(Attachment::Sticker { id: id.to_string() });
            text = "[sticker]".into();
        }
        "message.voice.received" => {
            if let Some(url) = message["voice_url"].as_str() {
                attachments.push(Attachment::Audio {
                    url: url.to_string(),
                });
            }
            text = "[voice message]".into();
        }
        // Zalo withholds the content of messages from protected users (children, people with
        // disabilities, …); okbot must answer without processing them.
        "message.unsupported.received" => {
            restricted = true;
            text.clear();
        }
        _ => return None,
    }
    let event_id = message_id.as_ref().map_or_else(
        || format!("{name}:{chat_id}:{}", sent_at.as_millisecond()),
        ToString::to_string,
    );
    Some(InboundEvent {
        event_id,
        conversation: ConversationKey::room(channel.clone(), chat_id),
        author: UserRef {
            id: UserId::new(user_id),
            display_name: from["display_name"].as_str().unwrap_or(user_id).to_string(),
            is_bot: from["is_bot"].as_bool().unwrap_or(false),
        },
        text,
        attachments,
        // Zalo delivers group messages only when they mention or reply to the bot.
        mentions_bot: !direct,
        is_direct: direct,
        restricted,
        message_id,
        sent_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel() -> ChannelId {
        "zalo:shop".parse().unwrap()
    }

    #[test]
    fn text_message_from_the_docs() {
        // The sample payload from https://bot.zapps.me/docs/webhook/
        let body = json!({
          "ok": true,
          "result": {
            "message": {
              "from": {"id": "6ede9afa66b88fe6d6a9", "display_name": "Ted", "is_bot": false},
              "chat": {"id": "6ede9afa66b88fe6d6a9", "chat_type": "PRIVATE"},
              "text": "Xin chào",
              "message_id": "2d758cb5e222177a4e35",
              "date": 1_750_316_131_602_i64
            },
            "event_name": "message.text.received"
          }
        });
        let e = to_inbound(&channel(), &body, Timestamp::UNIX_EPOCH).unwrap();
        assert_eq!(e.text, "Xin chào");
        assert_eq!(e.author.display_name, "Ted");
        assert_eq!(e.conversation.room, "6ede9afa66b88fe6d6a9");
        assert!(e.is_direct && !e.mentions_bot && !e.restricted);
        assert_eq!(e.event_id, "2d758cb5e222177a4e35");
        assert_eq!(e.sent_at.as_millisecond(), 1_750_316_131_602);
    }

    #[test]
    fn group_image_unsupported_and_unknown() {
        let group = json!({"event_name": "message.image.received", "message": {
            "from": {"id": "u1", "display_name": "An"},
            "chat": {"id": "g1", "chat_type": "GROUP"},
            "photo": "https://img/x.jpg", "caption": "giá bao nhiêu?", "message_id": "m2"}});
        let e = to_inbound(&channel(), &group, Timestamp::UNIX_EPOCH).unwrap();
        assert!(!e.is_direct && e.mentions_bot);
        assert_eq!(e.text, "giá bao nhiêu?");
        assert_eq!(e.attachments.len(), 1);

        let hidden = json!({"event_name": "message.unsupported.received", "message": {
            "from": {"id": "u2"}, "chat": {"id": "u2", "chat_type": "PRIVATE"}, "message_id": "m3"}});
        let e = to_inbound(&channel(), &hidden, Timestamp::UNIX_EPOCH).unwrap();
        assert!(e.restricted && e.text.is_empty());

        let other = json!({"event_name": "bot.added", "message": {"chat": {"id": "g"}}});
        assert!(to_inbound(&channel(), &other, Timestamp::UNIX_EPOCH).is_none());
    }
}
