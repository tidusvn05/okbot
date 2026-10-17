//! Zalo updates → [`InboundEvent`]s.

use jiff::Timestamp;
use okbot_core::{
    Attachment, ChannelId, ConversationKey, InboundEvent, MessageId, UserId, UserRef,
};
use zalo_bot::{Content, Update};

/// Converts one update into an event. `None` for updates okbot does not handle.
pub(crate) fn to_inbound(
    channel: &ChannelId,
    update: &Update,
    now: Timestamp,
) -> Option<InboundEvent> {
    let message = &update.message;
    let chat_id = message.chat.id.as_str();
    let direct = !message.is_group();
    let sent_at = Timestamp::from_millisecond(message.date)
        .ok()
        .filter(|_| message.date > 0)
        .unwrap_or(now);

    let mut text = String::new();
    let mut attachments = Vec::new();
    let mut restricted = false;
    match update.content() {
        Content::Text(t) => t.clone_into(&mut text),
        Content::Image { url, caption } => {
            attachments.push(Attachment::Image {
                url: url.to_string(),
                caption: caption.map(str::to_string),
            });
            text = caption.unwrap_or("[image]").to_string();
        }
        Content::Sticker { id, url } => {
            let id = if id.is_empty() {
                url.unwrap_or_default()
            } else {
                id
            };
            attachments.push(Attachment::Sticker { id: id.to_string() });
            text = "[sticker]".into();
        }
        Content::Voice { url } => {
            if !url.is_empty() {
                attachments.push(Attachment::Audio {
                    url: url.to_string(),
                });
            }
            text = "[voice message]".into();
        }
        // Zalo withholds the content of messages from protected users (children, people with
        // disabilities, …); okbot must answer without processing them.
        Content::Unsupported => restricted = true,
        _ => return None,
    }
    let message_id = (!message.message_id.is_empty()).then(|| MessageId::new(&message.message_id));
    let event_id = message_id.as_ref().map_or_else(
        || {
            format!(
                "{}:{chat_id}:{}",
                update.event_name,
                sent_at.as_millisecond()
            )
        },
        ToString::to_string,
    );
    let user_id = if message.from.id.is_empty() {
        chat_id
    } else {
        message.from.id.as_str()
    };
    let display_name = if message.from.display_name.is_empty() {
        user_id
    } else {
        message.from.display_name.as_str()
    };
    Some(InboundEvent {
        event_id,
        conversation: ConversationKey::room(channel.clone(), chat_id),
        author: UserRef {
            id: UserId::new(user_id),
            display_name: display_name.to_string(),
            is_bot: message.from.is_bot,
        },
        text,
        attachments,
        // Zalo delivers group messages only when they mention or reply to the bot.
        mentions_bot: !direct,
        is_direct: direct,
        restricted,
        message_id,
        sent_at,
        locale: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel() -> ChannelId {
        "zalo:shop".parse().unwrap()
    }

    fn convert(body: &serde_json::Value) -> Option<InboundEvent> {
        let update = zalo_bot::webhook::parse_update(&serde_json::to_vec(body).unwrap()).unwrap();
        to_inbound(&channel(), &update, Timestamp::UNIX_EPOCH)
    }

    #[test]
    fn text_message_from_the_docs() {
        // The sample payload from https://bot.zapps.me/docs/webhook/
        let e = convert(&json!({
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
        }))
        .unwrap();
        assert_eq!(e.text, "Xin chào");
        assert_eq!(e.author.display_name, "Ted");
        assert_eq!(e.conversation.room, "6ede9afa66b88fe6d6a9");
        assert!(e.is_direct && !e.mentions_bot && !e.restricted);
        assert_eq!(e.event_id, "2d758cb5e222177a4e35");
        assert_eq!(e.sent_at.as_millisecond(), 1_750_316_131_602);
    }

    #[test]
    fn group_image_unsupported_and_unknown() {
        let e = convert(&json!({"event_name": "message.image.received", "message": {
            "from": {"id": "u1", "display_name": "An"},
            "chat": {"id": "g1", "chat_type": "GROUP"},
            "photo": "https://img/x.jpg", "caption": "giá bao nhiêu?", "message_id": "m2"}}))
        .unwrap();
        assert!(!e.is_direct && e.mentions_bot);
        assert_eq!(e.text, "giá bao nhiêu?");
        assert_eq!(e.attachments.len(), 1);

        let e = convert(&json!({"event_name": "message.unsupported.received", "message": {
            "from": {"id": "u2"}, "chat": {"id": "u2", "chat_type": "PRIVATE"}, "message_id": "m3"}}))
        .unwrap();
        assert!(e.restricted && e.text.is_empty());

        let other = json!({"event_name": "bot.added", "message": {"from": {"id": "u"}, "chat": {"id": "g"}}});
        assert!(convert(&other).is_none());
    }
}
