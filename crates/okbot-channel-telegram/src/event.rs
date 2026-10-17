//! Telegram updates → [`InboundEvent`]s.

use jiff::Timestamp;
use okbot_core::{ChannelId, ConversationKey, InboundEvent, MessageId, UserId, UserRef};
use serde::Deserialize;

/// The largest photo size okbot asks for (the attachment tool reads at most 8 MiB).
const MAX_PHOTO_BYTES: u64 = 8 * 1024 * 1024;

/// One update; okbot asks only for `message`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Update {
    pub(crate) update_id: i64,
    #[serde(default)]
    pub(crate) message: Option<Message>,
}

#[derive(Debug, Clone, Deserialize)]
#[expect(clippy::struct_field_names, reason = "the Bot API's field names")]
pub(crate) struct Message {
    message_id: i64,
    #[serde(default)]
    message_thread_id: Option<i64>,
    #[serde(default)]
    is_topic_message: bool,
    #[serde(default)]
    from: Option<User>,
    chat: Chat,
    #[serde(default)]
    date: i64,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    entities: Vec<Entity>,
    #[serde(default)]
    caption_entities: Vec<Entity>,
    #[serde(default)]
    reply_to_message: Option<Box<Message>>,
    #[serde(default)]
    photo: Vec<PhotoSize>,
    #[serde(default)]
    document: Option<Document>,
    #[serde(default)]
    voice: Option<FileRef>,
    #[serde(default)]
    audio: Option<FileRef>,
    #[serde(default)]
    sticker: Option<Sticker>,
}

#[derive(Debug, Clone, Deserialize)]
struct User {
    id: i64,
    #[serde(default)]
    is_bot: bool,
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    language_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Entity {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    user: Option<User>,
}

#[derive(Debug, Clone, Deserialize)]
struct PhotoSize {
    file_id: String,
    #[serde(default)]
    file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct Document {
    file_id: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FileRef {
    file_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Sticker {
    file_unique_id: String,
    #[serde(default)]
    emoji: Option<String>,
}

/// The bot itself, from `getMe`.
#[derive(Debug, Clone)]
pub(crate) struct Bot {
    pub(crate) id: i64,
    pub(crate) username: String,
}

/// A file the channel resolves to a download URL before delivering the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingFile {
    /// A photo (or an image sent as a document).
    Image {
        file_id: String,
        caption: Option<String>,
    },
    /// Any other document.
    File {
        file_id: String,
        name: Option<String>,
    },
    /// Voice or audio.
    Audio { file_id: String },
}

/// Whether `word` is `@username` (any case), possibly followed by punctuation (`@bot,`).
fn is_mention(word: &str, username: &str) -> bool {
    !username.is_empty()
        && word
            .trim_end_matches([',', '.', ':', ';', '!', '?'])
            .strip_prefix('@')
            .is_some_and(|name| name.eq_ignore_ascii_case(username))
}

/// Removes mentions of the bot from `text` (whitespace is normalized, as in a chat line).
fn strip_username(text: &str, username: &str) -> String {
    if !text.split_whitespace().any(|w| is_mention(w, username)) {
        return text.trim().to_string();
    }
    text.split_whitespace()
        .filter(|w| !is_mention(w, username))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `/cmd@bot args` → `/cmd args`. `None` when the command names another bot.
fn command_for_me(text: &str, username: &str) -> Option<String> {
    let Some(command) = text.strip_prefix('/') else {
        return Some(text.to_string());
    };
    let (head, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let name = match head.split_once('@') {
        Some((name, bot)) if bot.eq_ignore_ascii_case(username) => name,
        Some(_) => return None,
        None => head,
    };
    Some(if args.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {}", args.trim_start())
    })
}

fn display_name(user: &User) -> String {
    let name = match &user.last_name {
        Some(last) if !last.is_empty() => format!("{} {last}", user.first_name),
        _ => user.first_name.clone(),
    };
    if name.trim().is_empty() {
        user.username.clone().unwrap_or_else(|| user.id.to_string())
    } else {
        name
    }
}

/// The files of a message and the text that stands for them when there is no caption.
fn media(message: &Message, caption: Option<&String>) -> (Vec<PendingFile>, Option<&'static str>) {
    let caption = caption.cloned();
    let mut files = Vec::new();
    let mut placeholder = None;
    if !message.photo.is_empty() {
        // Sizes come smallest first; take the largest the attachment tool can read.
        let photo = message
            .photo
            .iter()
            .rev()
            .find(|p| p.file_size.is_none_or(|s| s <= MAX_PHOTO_BYTES))
            .unwrap_or(&message.photo[0]);
        files.push(PendingFile::Image {
            file_id: photo.file_id.clone(),
            caption: caption.clone(),
        });
        placeholder = Some("[image]");
    } else if let Some(doc) = &message.document {
        if doc
            .mime_type
            .as_deref()
            .is_some_and(|m| m.starts_with("image/"))
        {
            files.push(PendingFile::Image {
                file_id: doc.file_id.clone(),
                caption: caption.clone(),
            });
            placeholder = Some("[image]");
        } else {
            files.push(PendingFile::File {
                file_id: doc.file_id.clone(),
                name: doc.file_name.clone(),
            });
            placeholder = Some("[file]");
        }
    } else if let Some(audio) = message.voice.as_ref().or(message.audio.as_ref()) {
        files.push(PendingFile::Audio {
            file_id: audio.file_id.clone(),
        });
        placeholder = Some("[voice message]");
    } else if message.sticker.is_some() {
        placeholder = Some("[sticker]");
    }

    (files, placeholder)
}

/// Converts one update into an event and the files to resolve. `None` for updates okbot does not
/// handle: edits, channel posts, messages from the bot itself, commands for other bots.
pub(crate) fn to_inbound(
    channel: &ChannelId,
    update: &Update,
    me: &Bot,
    now: Timestamp,
) -> Option<(InboundEvent, Vec<PendingFile>)> {
    let message = update.message.as_ref()?;
    let from = message.from.as_ref()?;
    if from.id == me.id {
        return None;
    }
    let direct = match message.chat.kind.as_str() {
        "private" => true,
        "group" | "supergroup" => false,
        _ => return None,
    };

    let caption = message.caption.clone().filter(|c| !c.is_empty());
    let (files, placeholder) = media(message, caption.as_ref());
    let raw = message
        .text
        .clone()
        .or(caption)
        .or_else(|| placeholder.map(str::to_string))?;
    let entities = if message.text.is_some() {
        &message.entities
    } else {
        &message.caption_entities
    };
    let mentioned = entities
        .iter()
        .any(|e| e.kind == "text_mention" && e.user.as_ref().is_some_and(|u| u.id == me.id))
        || raw.split_whitespace().any(|w| is_mention(w, &me.username))
        || raw
            .split_whitespace()
            .next()
            .and_then(|w| w.strip_prefix('/'))
            .and_then(|c| c.split_once('@'))
            .is_some_and(|(_, bot)| bot.eq_ignore_ascii_case(&me.username))
        || message
            .reply_to_message
            .as_ref()
            .and_then(|r| r.from.as_ref())
            .is_some_and(|u| u.id == me.id);
    let text = command_for_me(&raw, &me.username)?;
    let text = strip_username(&text, &me.username);

    let mut attachments = Vec::new();
    if let Some(sticker) = &message.sticker {
        attachments.push(okbot_core::Attachment::Sticker {
            id: sticker
                .emoji
                .clone()
                .unwrap_or_else(|| sticker.file_unique_id.clone()),
        });
    }
    let chat = message.chat.id.to_string();
    let thread = message
        .is_topic_message
        .then_some(message.message_thread_id)
        .flatten()
        .map(|t| t.to_string());
    let sent_at = Timestamp::from_second(message.date)
        .ok()
        .filter(|_| message.date > 0)
        .unwrap_or(now);
    let event = InboundEvent {
        // Message ids are unique per chat only.
        event_id: format!("{chat}:{}", message.message_id),
        conversation: ConversationKey {
            channel: channel.clone(),
            space: None,
            room: chat,
            thread,
        },
        author: UserRef {
            id: UserId::new(from.id.to_string()),
            display_name: display_name(from),
            is_bot: from.is_bot,
        },
        text,
        attachments,
        mentions_bot: !direct && mentioned,
        is_direct: direct,
        restricted: false,
        message_id: Some(MessageId::new(message.message_id.to_string())),
        sent_at,
        locale: from.language_code.clone(),
    };
    Some((event, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn me() -> Bot {
        Bot {
            id: 999,
            username: "ShopBot".into(),
        }
    }

    fn convert(update: &serde_json::Value) -> Option<(InboundEvent, Vec<PendingFile>)> {
        let update: Update = serde_json::from_value(update.clone()).unwrap();
        to_inbound(
            &"telegram:shop".parse().unwrap(),
            &update,
            &me(),
            Timestamp::UNIX_EPOCH,
        )
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "reads better at the call sites"
    )]
    fn message(chat: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
        let mut m = json!({
            "message_id": 42,
            "from": {"id": 7, "is_bot": false, "first_name": "Lan", "last_name": "Nguyễn", "language_code": "vi"},
            "chat": chat,
            "date": 1_750_000_000,
        });
        m.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"update_id": 1, "message": m})
    }

    #[test]
    fn private_text() {
        let (e, files) = convert(&message(
            json!({"id": 7, "type": "private"}),
            json!({"text": "Xin chào"}),
        ))
        .unwrap();
        assert_eq!(e.text, "Xin chào");
        assert_eq!(e.author.display_name, "Lan Nguyễn");
        assert_eq!(e.author.id.as_str(), "7");
        assert_eq!(e.conversation.room, "7");
        assert_eq!(e.event_id, "7:42");
        assert!(e.is_direct && !e.mentions_bot);
        assert_eq!(e.sent_at.as_second(), 1_750_000_000);
        assert_eq!(e.locale.as_deref(), Some("vi"));
        assert_eq!(files, []);
    }

    #[test]
    fn group_mentions_replies_and_commands() {
        let group = json!({"id": -100_123, "type": "supergroup"});
        let (e, _) = convert(&message(
            group.clone(),
            json!({"text": "@shopbot giá bao nhiêu?"}),
        ))
        .unwrap();
        assert!(e.mentions_bot && !e.is_direct);
        assert_eq!(e.text, "giá bao nhiêu?");
        assert_eq!(e.conversation.room, "-100123");

        let (e, _) = convert(&message(group.clone(), json!({"text": "chào cả nhà"}))).unwrap();
        assert!(!e.mentions_bot);

        let reply = json!({"text": "còn hàng không?", "reply_to_message": {
            "message_id": 41, "from": {"id": 999, "is_bot": true, "first_name": "Shop"},
            "chat": {"id": -100_123, "type": "supergroup"}, "date": 1}});
        assert!(
            convert(&message(group.clone(), reply))
                .unwrap()
                .0
                .mentions_bot
        );

        let (e, _) = convert(&message(
            group.clone(),
            json!({"text": "/tasks@ShopBot all"}),
        ))
        .unwrap();
        assert_eq!(e.text, "/tasks all");
        assert!(e.mentions_bot);
        assert!(convert(&message(group.clone(), json!({"text": "/tasks@OtherBot"}))).is_none());

        let topic = json!({"text": "hi", "is_topic_message": true, "message_thread_id": 5});
        assert_eq!(
            convert(&message(group, topic))
                .unwrap()
                .0
                .conversation
                .thread
                .as_deref(),
            Some("5")
        );
    }

    #[test]
    fn photos_documents_and_ignored_updates() {
        let private = json!({"id": 7, "type": "private"});
        let photo = json!({"caption": "cái này", "photo": [
            {"file_id": "small", "file_size": 1000},
            {"file_id": "big", "file_size": 2_000_000},
            {"file_id": "huge", "file_size": 20_000_000}]});
        let (e, files) = convert(&message(private.clone(), photo)).unwrap();
        assert_eq!(e.text, "cái này");
        assert_eq!(
            files,
            [PendingFile::Image {
                file_id: "big".into(),
                caption: Some("cái này".into())
            }]
        );

        let doc = json!({"document": {"file_id": "d1", "file_name": "bang-gia.pdf", "mime_type": "application/pdf"}});
        let (e, files) = convert(&message(private.clone(), doc)).unwrap();
        assert_eq!(e.text, "[file]");
        assert_eq!(
            files,
            [PendingFile::File {
                file_id: "d1".into(),
                name: Some("bang-gia.pdf".into())
            }]
        );

        // The bot's own messages, channel posts and updates without a message are skipped.
        let own = json!({"update_id": 2, "message": {"message_id": 1, "from": {"id": 999, "is_bot": true},
            "chat": {"id": 7, "type": "private"}, "text": "x"}});
        assert!(convert(&own).is_none());
        let post = message(json!({"id": -1, "type": "channel"}), json!({"text": "x"}));
        assert!(convert(&post).is_none());
        assert!(convert(&json!({"update_id": 3})).is_none());
        // A message with nothing okbot reads (a location, say).
        assert!(convert(&message(private, json!({"location": {}}))).is_none());
    }

    #[test]
    fn username_stripping_keeps_other_text() {
        assert_eq!(strip_username("hi @ShopBot,  there", "shopbot"), "hi there");
        assert_eq!(strip_username("ask @shopbotx", "shopbot"), "ask @shopbotx");
        assert_eq!(strip_username("  multi\nline  ", "shopbot"), "multi\nline");
        assert_eq!(command_for_me("/start", "b").as_deref(), Some("/start"));
    }
}
