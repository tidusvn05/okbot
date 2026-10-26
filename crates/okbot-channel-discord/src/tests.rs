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
    assert_eq!(BOT_PERMISSIONS, 274_878_008_320);
    assert_eq!(
        install_url(1_559_468_846_841_200_730),
        "https://discord.com/oauth2/authorize?client_id=1559468846841200730&scope=bot&permissions=274878008320&integration_type=0"
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
            url: None,
        }],
        allowed: vec![],
        cards: Vec::new(),
        select: None,
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

async fn serve(calls: Arc<Calls>) -> DiscordChannel {
    let app = axum::Router::new().fallback(fake).with_state(calls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    DiscordChannel::new(DiscordConfig {
        id: channel(),
        token: Secret::new("Bot test-token".into()),
        message_content: false,
        http_proxy: Some(addr.to_string()),
    })
}

fn interactive_prompt() -> okbot_core::Prompt {
    okbot_core::Prompt {
        id: "v1a2b3c4d".into(),
        text: "@everyone orders".into(),
        buttons: vec![okbot_core::PromptButton {
            id: "b1".into(),
            label: "Note".into(),
            style: okbot_core::ButtonStyle::Primary,
            form: Some(okbot_core::Form {
                title: "Note".into(),
                fields: vec![okbot_core::FormField {
                    id: "f1".into(),
                    label: "Text".into(),
                    style: okbot_core::FieldStyle::Short,
                    required: true,
                    placeholder: None,
                    min_len: None,
                    max_len: None,
                    value: Some("draft".into()),
                }],
            }),
            url: None,
        }],
        allowed: vec![],
        cards: vec![okbot_core::Card {
            title: Some("Order 17".into()),
            tone: okbot_core::Tone::Warning,
            text: "Unpaid".into(),
            image: None,
        }],
        select: Some(okbot_core::Select {
            placeholder: "Pick".into(),
            options: vec![
                okbot_core::SelectOption {
                    id: "o1".into(),
                    label: "One".into(),
                    description: None,
                },
                okbot_core::SelectOption {
                    id: "o2".into(),
                    label: "Two".into(),
                    description: None,
                },
            ],
        }),
    }
}

#[tokio::test]
async fn cards_go_out_as_components_v2_and_update_statelessly() {
    let calls = Arc::new(Calls::default());
    let channel = serve(calls.clone()).await;
    let to = ReplyTarget {
        conversation: ConversationKey::room(self::channel(), "500"),
        in_reply_to: None,
    };
    let prompt = interactive_prompt();
    let sent = channel.prompt(&to, &prompt).await.unwrap();

    // Another process (after a restart) closes it: no memory of the prompt.
    let fresh = serve(calls.clone()).await;
    let mut closed = prompt.clone();
    closed.buttons.clear();
    closed.select = None;
    fresh.update_prompt(&sent, &closed).await.unwrap();

    let calls = calls.0.lock().unwrap().clone();
    let post = calls.iter().find(|c| c.0 == "POST").unwrap();
    assert_eq!(
        post.2["flags"],
        (1 << 15) | 4,
        "IS_COMPONENTS_V2 | SUPPRESS_EMBEDS"
    );
    assert!(post.2.get("content").is_none());
    assert_eq!(post.2["allowed_mentions"]["parse"], json!([]), "no pings");
    let kinds: Vec<_> = post.2["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["type"].as_u64().unwrap())
        .collect();
    assert_eq!(kinds, [10, 17, 1, 1]);
    assert_eq!(post.2["components"][1]["accent_color"], 0x00EA_B308);

    let patch = calls.iter().find(|c| c.0 == "PATCH").unwrap();
    assert!(patch.1.ends_with("/channels/500/messages/901"));
    assert_eq!(patch.2["flags"], (1 << 15) | 4);
    assert_eq!(patch.2["content"], Value::Null);
    assert_eq!(patch.2["allowed_mentions"]["parse"], json!([]));
    let kinds: Vec<_> = patch.2["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["type"].as_u64().unwrap())
        .collect();
    assert_eq!(kinds, [10, 17], "closed: no rows left");
    assert!(fresh.open_prompt("v1a2b3c4d").is_none());
}

#[derive(Default)]
struct Answers {
    answers: Mutex<Vec<okbot_core::PromptAnswer>>,
    known: Option<okbot_core::Prompt>,
}

#[async_trait]
impl InboundSink for Answers {
    async fn deliver(&self, _event: InboundEvent) -> Result<(), okbot_core::SinkClosed> {
        Ok(())
    }

    async fn answer(&self, answer: okbot_core::PromptAnswer) -> Result<(), okbot_core::SinkClosed> {
        self.answers.lock().unwrap().push(answer);
        Ok(())
    }

    async fn prompt(&self, id: &str) -> Option<okbot_core::Prompt> {
        self.known.clone().filter(|p| p.id == id)
    }
}

fn component_click(id: &str, data: &Value) -> Interaction {
    serde_json::from_value(json!({
        "id": id, "application_id": "77", "type": 3, "token": "tok", "version": 1,
        "guild_id": "9", "channel": {"id": "500", "type": 0},
        "member": {"user": {"id": "7", "username": "lan", "global_name": "Lan", "discriminator": "0", "avatar": null},
                   "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false, "flags": 0},
        "message": {
            "id": "901", "channel_id": "500", "content": "",
            "author": {"id": "42", "username": "okbot", "discriminator": "0", "avatar": null, "bot": true},
            "timestamp": "2026-10-12T10:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
            "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
            "embeds": [], "pinned": false, "type": 0
        },
        "data": data,
        "entitlements": [], "authorizing_integration_owners": {}
    }))
    .unwrap()
}

#[tokio::test]
async fn menu_picks_and_forms_work_after_a_restart() {
    let calls = Arc::new(Calls::default());
    let channel = serve(calls.clone()).await;
    let _ = channel.app.set(Id::new(77));
    let answers = Arc::new(Answers {
        known: Some(interactive_prompt()),
        ..Answers::default()
    });
    let sink: Arc<dyn InboundSink> = answers.clone();

    // A menu pick on a prompt shown before the restart: recovered from the runtime.
    let pick = component_click(
        "3001",
        &json!({"custom_id": "oks:v1a2b3c4d", "component_type": 3, "values": ["o2"]}),
    );
    channel.interaction(&sink, &pick).await.unwrap();
    // A button with a form opens the modal, pre-filled.
    let press = component_click(
        "3002",
        &json!({"custom_id": "okp:v1a2b3c4d:b1", "component_type": 2}),
    );
    channel.interaction(&sink, &press).await.unwrap();
    // Neither the channel nor the runtime knows this one.
    let stale = component_click(
        "3003",
        &json!({"custom_id": "okp:gone:b1", "component_type": 2}),
    );
    channel.interaction(&sink, &stale).await.unwrap();

    let answers = answers.answers.lock().unwrap().clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].prompt, "v1a2b3c4d");
    assert_eq!(answers[0].button, "o2");
    assert_eq!(answers[0].user.id.as_str(), "7");
    assert_eq!(answers[0].sent.message_id.as_str(), "901");

    let calls = calls.0.lock().unwrap().clone();
    let callback = |id: &str| {
        calls
            .iter()
            .find(|c| c.1.ends_with(&format!("/interactions/{id}/tok/callback")))
            .unwrap_or_else(|| panic!("{calls:?}"))
            .2
            .clone()
    };
    assert_eq!(callback("3001")["type"], 6, "deferred update at once");
    let modal = callback("3002");
    assert_eq!(modal["type"], 9);
    assert_eq!(modal["data"]["custom_id"], "okf:v1a2b3c4d:b1");
    assert_eq!(
        modal["data"]["components"][0]["component"]["value"],
        "draft"
    );
    let expired = callback("3003");
    assert_eq!(expired["type"], 4);
    assert_eq!(expired["data"]["flags"], 64, "ephemeral");
}

/// Answers 400 to edits that set plain content, as Discord does on a Components V2 message.
async fn v2_only(
    State(calls): State<Arc<Calls>>,
    method: Method,
    uri: Uri,
    body: String,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if method == Method::PATCH && value["content"].is_string() {
        calls
            .0
            .lock()
            .unwrap()
            .push((method.to_string(), uri.path().to_string(), value));
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"code": 50035, "message": "Invalid Form Body"})),
        )
            .into_response();
    }
    fake(State(calls), method, uri, body).await.into_response()
}

#[tokio::test]
async fn closing_an_unknown_v2_prompt_retries_with_components() {
    let calls = Arc::new(Calls::default());
    let app = axum::Router::new()
        .fallback(v2_only)
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let channel = DiscordChannel::new(DiscordConfig {
        id: channel(),
        token: Secret::new("Bot test-token".into()),
        message_content: false,
        http_proxy: Some(addr.to_string()),
    });
    let sent = SentRef {
        target: ReplyTarget {
            conversation: ConversationKey::room(self::channel(), "500"),
            in_reply_to: None,
        },
        message_id: MessageId::new("901"),
    };
    channel
        .close_prompt(
            &sent,
            &RenderedMessage {
                text: "✅ Done".into(),
            },
            false,
        )
        .await
        .unwrap();
    let calls = calls.0.lock().unwrap().clone();
    let patches: Vec<_> = calls.iter().filter(|c| c.0 == "PATCH").collect();
    assert_eq!(patches.len(), 2, "{calls:?}");
    assert_eq!(patches[1].2["flags"], (1 << 15) | 4);
    assert_eq!(
        patches[1].2["components"],
        json!([{"type": 10, "content": "✅ Done"}])
    );
}

/// A fake Discord for chart images: records multipart bodies as text, answers `GET` of a message
/// with its `attachments`, and, with `refuse_gallery`, answers 400 to edits with a media gallery.
struct ImageApi {
    calls: Mutex<Vec<(String, String, String)>>,
    attachments: Value,
    refuse_gallery: bool,
}

async fn image_handler(
    State(api): State<Arc<ImageApi>>,
    method: Method,
    uri: Uri,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let body = String::from_utf8_lossy(&body).into_owned();
    api.calls
        .lock()
        .unwrap()
        .push((method.to_string(), uri.path().to_string(), body.clone()));
    if method == Method::PATCH && api.refuse_gallery && body.contains("\"type\":12") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"code": 50035, "message": "Invalid Form Body"})),
        )
            .into_response();
    }
    axum::Json(json!({
        "id": "901", "channel_id": "500", "content": "",
        "author": {"id": "42", "username": "okbot", "discriminator": "0", "avatar": null, "bot": true},
        "timestamp": "2026-10-12T10:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
        "mention_everyone": false, "mentions": [], "mention_roles": [],
        "attachments": api.attachments, "embeds": [], "pinned": false, "type": 0, "flags": 32768
    }))
    .into_response()
}

async fn serve_images(api: Arc<ImageApi>) -> DiscordChannel {
    let app = axum::Router::new().fallback(image_handler).with_state(api);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    DiscordChannel::new(DiscordConfig {
        id: channel(),
        token: Secret::new("Bot test-token".into()),
        message_content: false,
        http_proxy: Some(addr.to_string()),
    })
}

fn image_api(attachments: Value, refuse_gallery: bool) -> Arc<ImageApi> {
    Arc::new(ImageApi {
        calls: Mutex::new(Vec::new()),
        attachments,
        refuse_gallery,
    })
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-chart-bytes";

fn chart_prompt(png: &[u8]) -> okbot_core::Prompt {
    let mut prompt = interactive_prompt();
    prompt.cards[0].image = Some(okbot_core::Image {
        name: "chart.png".into(),
        alt: "Sales by month: Jan 3, Feb 5".into(),
        width: 800,
        height: 450,
        png: png.to_vec(),
    });
    prompt
}

/// The JSON part of a multipart body, or the body itself.
fn payload(body: &str) -> Value {
    if let Ok(v) = serde_json::from_str(body) {
        return v;
    }
    let start = body.find("{\"").unwrap();
    let end = body[start..].find("\r\n--").unwrap() + start;
    serde_json::from_str(&body[start..end]).unwrap()
}

fn sent_901() -> SentRef {
    SentRef {
        target: ReplyTarget {
            conversation: ConversationKey::room(self::channel(), "500"),
            in_reply_to: None,
        },
        message_id: MessageId::new("901"),
    }
}

fn chart_attachment() -> Value {
    json!([{"id": "1001", "filename": "chart.png", "size": 24, "content_type": "image/png",
            "url": "https://cdn.discordapp.com/attachments/500/1001/chart.png",
            "proxy_url": "https://media.discordapp.net/attachments/500/1001/chart.png"}])
}

#[tokio::test]
async fn a_card_image_is_uploaded_and_shown_in_its_card() {
    let api = image_api(json!([]), false);
    let channel = serve_images(api.clone()).await;
    let to = ReplyTarget {
        conversation: ConversationKey::room(self::channel(), "500"),
        in_reply_to: None,
    };
    channel.prompt(&to, &chart_prompt(PNG)).await.unwrap();
    let calls = api.calls.lock().unwrap().clone();
    let post = calls.iter().find(|c| c.0 == "POST").unwrap();
    assert!(post.2.contains("filename=\"chart.png\""), "{}", post.2);
    assert!(post.2.contains("fake-chart-bytes"), "the PNG goes along");
    let json = payload(&post.2);
    assert_eq!(json["flags"], (1 << 15) | 4);
    assert_eq!(json["attachments"][0]["filename"], "chart.png");
    assert_eq!(
        json["components"][1]["components"],
        json!([
            {"type": 10, "content": "### Order 17"},
            {"type": 14, "divider": true},
            {"type": 10, "content": "Unpaid"},
            {"type": 12, "items": [{
                "media": {"url": "attachment://chart.png", "content_type": null},
                "description": "Sales by month: Jan 3, Feb 5"
            }]}
        ])
    );
}

#[tokio::test]
async fn an_update_keeps_the_picture_the_message_was_sent_with() {
    let api = image_api(chart_attachment(), false);
    let channel = serve_images(api.clone()).await;
    // Read back from the store: the image has no PNG data.
    let mut closed = chart_prompt(&[]);
    closed.buttons.clear();
    closed.select = None;
    channel.update_prompt(&sent_901(), &closed).await.unwrap();
    let calls = api.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|c| c.0 == "GET" && c.1.ends_with("/channels/500/messages/901"))
    );
    let patch = calls.iter().find(|c| c.0 == "PATCH").unwrap();
    let json: Value = serde_json::from_str(&patch.2).expect("no multipart: nothing uploaded");
    assert!(json.get("attachments").is_none(), "existing files are kept");
    assert_eq!(
        json["components"][1]["components"][3]["items"][0]["media"]["url"],
        "attachment://chart.png"
    );
}

#[tokio::test]
async fn an_update_without_the_file_drops_the_gallery() {
    // The message has no file left.
    let api = image_api(json!([]), false);
    let channel = serve_images(api.clone()).await;
    channel
        .update_prompt(&sent_901(), &chart_prompt(&[]))
        .await
        .unwrap();
    // Discord refuses the reference: the edit goes again without it.
    let refusing = image_api(chart_attachment(), true);
    let other = serve_images(refusing.clone()).await;
    other
        .update_prompt(&sent_901(), &chart_prompt(&[]))
        .await
        .unwrap();

    let calls = api.calls.lock().unwrap().clone();
    let patches: Vec<_> = calls.iter().filter(|c| c.0 == "PATCH").collect();
    assert_eq!(patches.len(), 1);
    assert!(!patches[0].2.contains("\"type\":12"));
    let calls = refusing.calls.lock().unwrap().clone();
    let patches: Vec<_> = calls.iter().filter(|c| c.0 == "PATCH").collect();
    assert_eq!(patches.len(), 2, "{calls:?}");
    assert!(patches[0].2.contains("\"type\":12"));
    assert!(!patches[1].2.contains("\"type\":12"));
}

#[tokio::test]
async fn an_update_with_a_new_picture_replaces_the_file() {
    let api = image_api(chart_attachment(), false);
    let channel = serve_images(api.clone()).await;
    channel
        .update_prompt(&sent_901(), &chart_prompt(PNG))
        .await
        .unwrap();
    let calls = api.calls.lock().unwrap().clone();
    assert!(!calls.iter().any(|c| c.0 == "GET"));
    let patch = calls.iter().find(|c| c.0 == "PATCH").unwrap();
    assert!(patch.2.contains("fake-chart-bytes"));
    let json = payload(&patch.2);
    assert_eq!(
        json["attachments"],
        json!([{"id": 0, "filename": "chart.png"}]),
        "only the new file stays"
    );
    assert_eq!(
        json["components"][1]["components"][3]["items"][0]["media"]["url"],
        "attachment://chart.png"
    );
}
