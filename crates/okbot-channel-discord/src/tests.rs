use std::sync::Mutex;

use axum::extract::State;
use axum::http::{Method, Uri};
use serde_json::{Value, json};

use super::*;

fn message(author: u64, content: &str, guild: Option<u64>, mentions: &[u64]) -> Message {
    let user = |id: u64| {
        json!({"id": id.to_string(), "username": format!("user{id}"), "global_name": "Lan",
                                "discriminator": "0", "avatar": null, "bot": false, "public_flags": 0})
    };
    let mut value = json!({
        "id": "900", "channel_id": "500", "author": user(author), "content": content,
        "timestamp": "2026-10-12T10:00:00.000000+00:00", "edited_timestamp": null,
        "tts": false, "mention_everyone": false,
        "mentions": mentions.iter().map(|id| user(*id)).collect::<Vec<_>>(),
        "mention_roles": [], "attachments": [], "embeds": [], "pinned": false, "type": 0
    });
    if let Some(g) = guild {
        value["guild_id"] = json!(g.to_string());
    }
    serde_json::from_value(value).unwrap()
}

fn channel() -> ChannelId {
    "discord:main".parse().unwrap()
}

#[test]
fn maps_direct_and_mentioned_messages() {
    let me = Id::new(42);
    let dm = to_inbound(&channel(), &message(7, "hello", None, &[]), me).unwrap();
    assert!(dm.is_direct && !dm.mentions_bot);
    assert_eq!(dm.conversation.room, "500");
    assert_eq!(dm.author.display_name, "Lan");
    assert_eq!(dm.event_id, "900");

    let mention = to_inbound(
        &channel(),
        &message(7, "<@42> giá bao nhiêu?", Some(1), &[42]),
        me,
    )
    .unwrap();
    assert!(!mention.is_direct && mention.mentions_bot);
    assert_eq!(mention.text, "giá bao nhiêu?");
    assert_eq!(mention.conversation.space.as_deref(), Some("1"));

    let other = to_inbound(&channel(), &message(7, "chatting", Some(1), &[]), me).unwrap();
    assert!(!other.mentions_bot);
    assert!(to_inbound(&channel(), &message(42, "my own", None, &[]), me).is_none());
}

#[derive(Default)]
struct Calls(Mutex<Vec<(String, String, Value)>>);

async fn fake(
    State(calls): State<Arc<Calls>>,
    method: Method,
    uri: Uri,
    body: String,
) -> axum::Json<Value> {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    calls
        .0
        .lock()
        .unwrap()
        .push((method.to_string(), uri.path().to_string(), body));
    axum::Json(json!({
        "id": "901", "channel_id": "500", "content": "x",
        "author": {"id": "42", "username": "okbot", "discriminator": "0", "avatar": null, "bot": true},
        "timestamp": "2026-10-12T10:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
        "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
        "embeds": [], "pinned": false, "type": 0
    }))
}

#[tokio::test]
async fn sends_edits_and_types_without_pinging() {
    let calls = Arc::new(Calls::default());
    let app = axum::Router::new().fallback(fake).with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let channel = DiscordChannel::new(DiscordConfig {
        id: channel(),
        token: Secret::new("Bot test-token".into()),
        message_content: false,
        http_proxy: Some(addr.to_string()),
    });
    let to = ReplyTarget {
        conversation: ConversationKey::room(self::channel(), "500"),
        in_reply_to: Some(MessageId::new("900")),
    };
    channel.typing(&to).await.unwrap();
    let sent = channel
        .send(
            &to,
            &RenderedMessage {
                text: "@everyone hi".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_str(), "901");
    channel
        .edit(
            &sent,
            &RenderedMessage {
                text: "edited".into(),
            },
        )
        .await
        .unwrap();

    let calls = calls.0.lock().unwrap().clone();
    let post = calls
        .iter()
        .find(|c| c.0 == "POST" && c.1.ends_with("/channels/500/messages"))
        .unwrap();
    assert_eq!(post.2["content"], "@everyone hi");
    assert_eq!(post.2["allowed_mentions"]["parse"], json!([]), "no pings");
    assert_eq!(post.2["message_reference"]["message_id"], "900");
    let patch = calls.iter().find(|c| c.0 == "PATCH").unwrap();
    assert!(patch.1.ends_with("/channels/500/messages/901"));
    assert_eq!(patch.2["content"], "edited");
    assert!(
        calls
            .iter()
            .any(|c| c.0 == "POST" && c.1.ends_with("/channels/500/typing"))
    );
}

#[test]
fn install_url_has_the_needed_permissions() {
    assert_eq!(BOT_PERMISSIONS, 274_877_975_552);
    assert_eq!(
        install_url(1_559_468_846_841_200_730),
        "https://discord.com/oauth2/authorize?client_id=1559468846841200730&scope=bot&permissions=274877975552&integration_type=0"
    );
}
