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
    let dm = to_inbound(
        &channel(),
        &message(7, "hello", None, &[]),
        me,
        &HashSet::new(),
    )
    .unwrap();
    assert!(dm.is_direct && !dm.mentions_bot);
    assert_eq!(dm.conversation.room, "500");
    assert_eq!(dm.author.display_name, "Lan");
    assert_eq!(dm.event_id, "900");

    let mention = to_inbound(
        &channel(),
        &message(7, "<@42> giá bao nhiêu?", Some(1), &[42]),
        me,
        &HashSet::new(),
    )
    .unwrap();
    assert!(!mention.is_direct && mention.mentions_bot);
    assert_eq!(mention.text, "giá bao nhiêu?");
    assert_eq!(mention.conversation.space.as_deref(), Some("1"));

    let other = to_inbound(
        &channel(),
        &message(7, "chatting", Some(1), &[]),
        me,
        &HashSet::new(),
    )
    .unwrap();
    assert!(!other.mentions_bot);
    assert!(
        to_inbound(
            &channel(),
            &message(42, "my own", None, &[]),
            me,
            &HashSet::new()
        )
        .is_none()
    );
}

#[test]
fn a_mention_of_the_bots_own_role_counts() {
    let me = Id::new(42);
    let role: twilight_model::guild::Role = serde_json::from_value(json!({
        "id": "77", "name": "okbot", "color": 0, "colors": {"primary_color": 0, "secondary_color": null, "tertiary_color": null}, "hoist": false, "managed": true, "mentionable": false,
        "permissions": "0", "position": 1, "flags": 0, "tags": {"bot_id": "42"}
    }))
    .unwrap();
    let other: twilight_model::guild::Role = serde_json::from_value(json!({
        "id": "78", "name": "staff", "color": 0, "colors": {"primary_color": 0, "secondary_color": null, "tertiary_color": null}, "hoist": false, "managed": false, "mentionable": true,
        "permissions": "0", "position": 2, "flags": 0
    }))
    .unwrap();
    let roles: HashSet<_> = own_roles([&role, &other], me).collect();
    assert_eq!(roles, HashSet::from([Id::new(77)]));

    let mut m = message(7, "<@&77> freach?", Some(1), &[]);
    m.mention_roles = vec![Id::new(77)];
    let event = to_inbound(&channel(), &m, me, &roles).unwrap();
    assert!(event.mentions_bot);
    assert_eq!(event.text, "freach?");

    let mut m = message(7, "<@&78> hi", Some(1), &[]);
    m.mention_roles = vec![Id::new(78)];
    assert!(!to_inbound(&channel(), &m, me, &roles).unwrap().mentions_bot);
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

#[tokio::test]
async fn opens_a_direct_chat_with_the_same_key_as_inbound_dms() {
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
    let key = channel.open_direct(&UserId::new("7")).await.unwrap();
    assert_eq!(key, ConversationKey::room(self::channel(), "901"));
    let calls = calls.0.lock().unwrap().clone();
    let open = calls
        .iter()
        .find(|c| c.0 == "POST" && c.1.ends_with("/users/@me/channels"))
        .unwrap();
    assert_eq!(open.2["recipient_id"], "7");
    assert!(channel.open_direct(&UserId::new("x")).await.is_err());
}

#[test]
fn install_url_has_the_needed_permissions() {
    assert_eq!(BOT_PERMISSIONS, 274_877_975_552);
    assert_eq!(
        install_url(1_559_468_846_841_200_730),
        "https://discord.com/oauth2/authorize?client_id=1559468846841200730&scope=bot&permissions=274877975552&integration_type=0"
    );
}

#[derive(Default)]
struct Sink(Mutex<Vec<InboundEvent>>);

#[async_trait]
impl InboundSink for Sink {
    async fn deliver(&self, event: InboundEvent) -> Result<(), okbot_core::SinkClosed> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

#[tokio::test]
async fn slash_commands_register_and_arrive_as_text() {
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
    channel
        .set_commands(&[okbot_core::CommandInfo {
            name: "status".into(),
            description: "Status".into(),
            descriptions: Vec::new(),
            params: vec![okbot_core::CommandParam {
                name: "company".into(),
                required: true,
                rest: false,
            }],
        }])
        .await
        .unwrap();
    let app_id = Id::new(77);
    let _ = channel.app.set(app_id);
    channel.register_commands(app_id).await;

    let interaction: Interaction = serde_json::from_value(json!({
        "id": "3001", "application_id": "77", "type": 2, "token": "tok", "version": 1, "locale": "ja",
        "guild_id": "9", "channel": {"id": "500", "type": 0},
        "member": {"user": {"id": "7", "username": "lan", "global_name": "Lan", "discriminator": "0", "avatar": null},
                   "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false, "flags": 0},
        "data": {"id": "11", "name": "status", "type": 1,
                 "options": [{"name": "company", "type": 3, "value": "free ch"}]},
        "entitlements": [], "authorizing_integration_owners": {}
    }))
    .unwrap();
    let collected = Arc::new(Sink::default());
    let sink: Arc<dyn InboundSink> = collected.clone();
    channel.interaction(&sink, &interaction).await.unwrap();

    let calls = calls.0.lock().unwrap().clone();
    let put = calls.iter().find(|c| c.0 == "PUT").unwrap();
    assert!(put.1.ends_with("/applications/77/commands"), "{}", put.1);
    assert_eq!(put.2[0]["name"], "status");
    let ack = calls
        .iter()
        .find(|c| c.1.ends_with("/interactions/3001/tok/callback"))
        .unwrap_or_else(|| panic!("{calls:?}"));
    assert_eq!(ack.2["type"], 4);
    assert_eq!(ack.2["data"]["content"], "› `/status \"free ch\"`");

    let event = collected.0.lock().unwrap()[0].clone();
    assert_eq!(event.text, "/status \"free ch\"");
    assert_eq!(event.author.id.as_str(), "7");
    assert_eq!(event.conversation.room, "500");
    assert!(event.mentions_bot && !event.is_direct);
    assert_eq!(event.locale.as_deref(), Some("ja"));
}

#[tokio::test]
async fn prompts_never_show_link_previews() {
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
        in_reply_to: None,
    };
    let prompt = okbot_core::Prompt {
        id: "p1".into(),
        text: "[Sign in](https://claude.ai/oauth)".into(),
        buttons: vec![okbot_core::PromptButton {
            id: "ok".into(),
            label: "OK".into(),
            style: okbot_core::ButtonStyle::Primary,
            form: None,
        }],
        allowed: vec![],
    };
    let sent = channel.prompt(&to, &prompt).await.unwrap();
    channel
        .close_prompt(
            &sent,
            &RenderedMessage {
                text: "done".into(),
            },
            false,
        )
        .await
        .unwrap();
    let calls = calls.0.lock().unwrap().clone();
    let post = calls.iter().find(|c| c.0 == "POST").unwrap();
    assert_eq!(post.2["flags"], 4, "SUPPRESS_EMBEDS");
    let patch = calls.iter().find(|c| c.0 == "PATCH").unwrap();
    assert_eq!(patch.2["flags"], 4);
}
