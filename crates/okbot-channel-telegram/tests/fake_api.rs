//! The Telegram channel against a fake Bot API.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::routing::post;
use okbot_channel_telegram::{Delivery, TelegramApi, TelegramChannel, TelegramConfig};
use okbot_core::{
    Attachment, CancellationToken, Channel, ChannelError, InboundEvent, InboundSink, MessageId,
    RenderedMessage, ReplyTarget, Secret, SentRef, SinkClosed, WebhookRequest, async_trait,
};
use serde_json::{Value, json};

const TOKEN: &str = "12345:abc-xyz";

#[derive(Default)]
struct Fake {
    updates: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
    reject_token: Mutex<bool>,
    reject_html: Mutex<bool>,
}

/// The fields of a `multipart/form-data` body: text parts as strings, file parts as
/// `{"filename", "content_type", "data"}`.
fn multipart_fields(content_type: &str, body: &[u8]) -> Value {
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .unwrap()
        .trim_matches('"');
    let body = String::from_utf8_lossy(body);
    let mut fields = serde_json::Map::new();
    for part in body.split(&format!("--{boundary}")) {
        let Some((head, value)) = part.split_once("\r\n\r\n") else {
            continue;
        };
        let value = value.strip_suffix("\r\n").unwrap_or(value);
        let attribute = |name: &str| {
            head.split(&format!("{name}=\""))
                .nth(1)
                .map(|rest| rest.split('"').next().unwrap())
        };
        let name = attribute("name").unwrap();
        let field = match attribute("filename") {
            Some(filename) => {
                let content_type = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Type: "))
                    .unwrap_or_default();
                json!({"filename": filename, "content_type": content_type, "data": value})
            }
            None => json!(value),
        };
        fields.insert(name.to_string(), field);
    }
    Value::Object(fields)
}

async fn handle(
    State(fake): State<Arc<Fake>>,
    Path((bot, method)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    raw: axum::body::Bytes,
) -> Json<Value> {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let body: Value = if content_type.starts_with("multipart/form-data") {
        multipart_fields(content_type, &raw)
    } else {
        serde_json::from_slice(&raw).unwrap()
    };
    if bot != format!("bot{TOKEN}") || *fake.reject_token.lock().unwrap() {
        return Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"}));
    }
    fake.calls
        .lock()
        .unwrap()
        .push((method.clone(), body.clone()));
    let html = body.get("parse_mode").is_some();
    if *fake.reject_html.lock().unwrap() && html {
        return Json(json!({"ok": false, "error_code": 400,
            "description": "Bad Request: can't parse entities: unclosed tag"}));
    }
    Json(match method.as_str() {
        "getMe" => {
            json!({"ok": true, "result": {"id": 999, "is_bot": true, "first_name": "Shop", "username": "ShopBot"}})
        }
        "getUpdates" => {
            let offset = body["offset"].as_i64().unwrap_or(0);
            let next: Vec<Value> = fake
                .updates
                .lock()
                .unwrap()
                .iter()
                .filter(|u| u["update_id"].as_i64().unwrap() >= offset)
                .cloned()
                .collect();
            if next.is_empty() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            json!({"ok": true, "result": next})
        }
        "sendMessage" => {
            json!({"ok": true, "result": {"message_id": 77, "date": 1, "chat": {"id": 7, "type": "private"}}})
        }
        "sendPhoto" => {
            json!({"ok": true, "result": {"message_id": 88, "date": 1, "chat": {"id": 7, "type": "private"}}})
        }
        // Message 88 is a photo, the others are text.
        "editMessageText" if body["message_id"] == 88 => json!({"ok": false, "error_code": 400,
            "description": "Bad Request: there is no text in the message to edit"}),
        "editMessageCaption" if body["message_id"] != 88 => json!({"ok": false, "error_code": 400,
            "description": "Bad Request: there is no caption in the message to edit"}),
        "editMessageText" if body["text"] == "same" => json!({"ok": false, "error_code": 400,
            "description": "Bad Request: message is not modified"}),
        "getFile" => {
            json!({"ok": true, "result": {"file_id": body["file_id"], "file_path": "photos/file_1.jpg"}})
        }
        _ => json!({"ok": true, "result": true}),
    })
}

async fn fake_api() -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let app = axum::Router::new()
        .route("/{bot}/{method}", post(handle))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, base)
}

#[derive(Default)]
struct Collect(
    Mutex<Vec<InboundEvent>>,
    Mutex<Vec<okbot_core::PromptAnswer>>,
    /// Prompts the runtime still waits on (interactive replies outlive restarts).
    Mutex<Vec<okbot_core::Prompt>>,
);

#[async_trait]
impl InboundSink for Collect {
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }

    async fn answer(&self, answer: okbot_core::PromptAnswer) -> Result<(), SinkClosed> {
        self.1.lock().unwrap().push(answer);
        Ok(())
    }

    async fn prompt(
        &self,
        id: &str,
        _conversation: &okbot_core::ConversationKey,
    ) -> Option<okbot_core::Prompt> {
        self.2.lock().unwrap().iter().find(|p| p.id == id).cloned()
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "reads better at the call sites"
)]
fn update(id: i64, extra: Value) -> Value {
    let mut message = json!({
        "message_id": id * 10,
        "from": {"id": 7, "is_bot": false, "first_name": "Lan"},
        "chat": {"id": 7, "type": "private"},
        "date": 1_750_000_000,
    });
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"update_id": id, "message": message})
}

fn config(base: &str, delivery: Delivery) -> TelegramConfig {
    TelegramConfig {
        id: "telegram:shop".parse().unwrap(),
        token: Secret::new(TOKEN.into()),
        delivery,
        api_base: base.into(),
    }
}

fn calls(fake: &Fake, method: &str) -> Vec<Value> {
    fake.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == method)
        .map(|(_, b)| b.clone())
        .collect()
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out");
}

#[tokio::test]
async fn polls_delivers_replies_and_edits() {
    let (fake, base) = fake_api().await;
    fake.updates.lock().unwrap().extend([
        update(1, json!({"text": "Xin chào"})),
        update(
            2,
            json!({"caption": "ảnh", "photo": [{"file_id": "p1", "file_size": 10}]}),
        ),
    ]);
    let channel = Arc::new(TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (channel, sink, stop) = (channel.clone(), sink.clone(), stop.clone());
        async move { channel.run(sink, stop).await }
    });
    wait_for(|| sink.0.lock().unwrap().len() == 2).await;
    // Each update arrives once: the next poll acknowledges it with a higher offset.
    wait_for(|| calls(&fake, "getUpdates").len() >= 3).await;
    assert_eq!(sink.0.lock().unwrap().len(), 2);
    assert_eq!(calls(&fake, "getUpdates")[1]["offset"], 3);

    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events[0].text, "Xin chào");
    assert_eq!(events[1].text, "ảnh");
    let Attachment::Image { url, caption } = &events[1].attachments[0] else {
        panic!("{:?}", events[1].attachments);
    };
    assert_eq!(url, &format!("{base}/file/bot{TOKEN}/photos/file_1.jpg"));
    assert_eq!(caption.as_deref(), Some("ảnh"));

    let to = ReplyTarget::to_event(&events[0]);
    channel.typing(&to).await.unwrap();
    let message = |t: &str| RenderedMessage { text: t.into() };
    let sent = channel
        .send(&to, &message("<b>Chào</b> Lan"))
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_str(), "77");
    let send = &calls(&fake, "sendMessage")[0];
    assert_eq!(send["chat_id"], 7);
    assert_eq!(send["parse_mode"], "HTML");
    assert!(
        send.get("reply_parameters").is_none(),
        "no quote in a direct chat"
    );
    assert_eq!(calls(&fake, "sendChatAction")[0]["action"], "typing");

    channel.edit(&sent, &message("<b>Xong</b>")).await.unwrap();
    assert_eq!(calls(&fake, "editMessageText")[0]["message_id"], 77);
    channel.edit(&sent, &message("same")).await.unwrap();

    stop.cancel();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn group_replies_quote_and_bad_html_falls_back_to_text() {
    let (fake, base) = fake_api().await;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let mut to = ReplyTarget {
        conversation: okbot_core::ConversationKey::room(
            "telegram:shop".parse().unwrap(),
            "-100123",
        ),
        in_reply_to: Some(MessageId::new("5")),
    };
    to.conversation.thread = Some("9".into());
    *fake.reject_html.lock().unwrap() = true;
    channel
        .send(
            &to,
            &RenderedMessage {
                text: "<b>a &amp; b".into(),
            },
        )
        .await
        .unwrap();
    let sends = calls(&fake, "sendMessage");
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["reply_parameters"]["message_id"], 5);
    assert_eq!(sends[0]["message_thread_id"], 9);
    assert_eq!(sends[1]["text"], "a & b");
    assert!(sends[1].get("parse_mode").is_none());

    let sent = SentRef {
        target: to,
        message_id: MessageId::new("77"),
    };
    channel
        .edit(
            &sent,
            &RenderedMessage {
                text: "<i>x".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(calls(&fake, "editMessageText")[1]["text"], "x");
}

#[tokio::test]
async fn rejected_token_stops_the_channel() {
    let (fake, base) = fake_api().await;
    *fake.reject_token.lock().unwrap() = true;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let result = channel
        .run(Arc::new(Collect::default()), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(ChannelError::Auth(_))), "{result:?}");
}

#[tokio::test]
async fn webhook_registers_and_checks_the_secret() {
    let (fake, base) = fake_api().await;
    let channel = Arc::new(TelegramChannel::new(config(
        &base,
        Delivery::Webhook {
            public_url: "https://bot.example.com/telegram/shop".into(),
            secret: Secret::new("s3cret-value".into()),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (channel, sink, stop) = (channel.clone(), sink.clone(), stop.clone());
        async move { channel.run(sink, stop).await }
    });
    wait_for(|| !calls(&fake, "setWebhook").is_empty()).await;
    let set = &calls(&fake, "setWebhook")[0];
    assert_eq!(set["url"], "https://bot.example.com/telegram/shop");
    assert_eq!(set["secret_token"], "s3cret-value");

    let handler = channel.webhook().unwrap();
    assert_eq!(handler.path(), "telegram/shop");
    let request = |secret: &str| WebhookRequest {
        method: "POST".into(),
        path: String::new(),
        headers: vec![("x-telegram-bot-api-secret-token".into(), secret.into())],
        body: serde_json::to_vec(&update(5, json!({"text": "hello"}))).unwrap(),
    };
    assert_eq!(handler.handle(request("wrong")).await.status, 403);
    assert!(sink.0.lock().unwrap().is_empty());
    assert_eq!(handler.handle(request("s3cret-value")).await.status, 200);
    assert_eq!(sink.0.lock().unwrap()[0].text, "hello");

    stop.cancel();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn errors_never_contain_the_token() {
    // Nothing listens on port 9: the request fails in transport.
    let api = TelegramApi::new("http://127.0.0.1:9", Secret::new(TOKEN.into()));
    let error = api.get_me().await.unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(!text.contains("abc-xyz"), "{text}");
    assert!(!format!("{api:?}").contains("abc-xyz"));
}

#[tokio::test]
async fn command_menu_lists_parameters() {
    let (fake, base) = fake_api().await;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    channel
        .set_commands(&[okbot_core::CommandInfo {
            name: "status".into(),
            description: "Company status".into(),
            descriptions: vec![("ja".into(), "会社の状態".into())],
            params: vec![
                okbot_core::CommandParam {
                    name: "company".into(),
                    required: true,
                    rest: false,
                },
                okbot_core::CommandParam {
                    name: "deep".into(),
                    required: false,
                    rest: false,
                },
            ],
        }])
        .await
        .unwrap();
    let set = calls(&fake, "setMyCommands");
    assert_eq!(
        set[0]["commands"],
        json!([{"command": "status", "description": "<company> [deep] — Company status"}])
    );
    assert_eq!(set[1]["language_code"], "ja");
    assert_eq!(
        set[1]["commands"][0]["description"],
        "<company> [deep] — 会社の状態"
    );
}

#[tokio::test]
async fn a_users_direct_chat_is_their_id() {
    let (_fake, base) = fake_api().await;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let key = channel
        .open_direct(&okbot_core::UserId::new("12345"))
        .await
        .unwrap();
    assert_eq!(
        key,
        okbot_core::ConversationKey::room("telegram:shop".parse().unwrap(), "12345")
    );
    assert!(
        channel
            .open_direct(&okbot_core::UserId::new("@lan"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn prompts_use_inline_buttons() {
    let (fake, base) = fake_api().await;
    let channel = Arc::new(TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (channel, sink, stop) = (channel.clone(), sink.clone(), stop.clone());
        async move { channel.run(sink, stop).await }
    });
    wait_for(|| !calls(&fake, "getUpdates").is_empty()).await;
    assert_eq!(
        calls(&fake, "getUpdates")[0]["allowed_updates"],
        json!(["message", "callback_query"])
    );

    let to = ReplyTarget {
        conversation: okbot_core::ConversationKey::room("telegram:shop".parse().unwrap(), "7"),
        in_reply_to: None,
    };
    let button = |id: &str, form: Option<okbot_core::Form>| okbot_core::PromptButton {
        id: id.into(),
        label: id.to_uppercase(),
        style: okbot_core::ButtonStyle::Primary,
        form,
        url: None,
    };
    let mut prompt = okbot_core::Prompt {
        id: "abc123".into(),
        text: "Deploy?".into(),
        buttons: vec![button("allow", None), button("deny", None)],
        allowed: vec![okbot_core::UserId::new("7")],
        cards: Vec::new(),
        select: None,
    };
    let sent = channel.prompt(&to, &prompt).await.unwrap();
    let send = calls(&fake, "sendMessage")[0].clone();
    assert_eq!(
        send["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
        "okp:abc123:allow"
    );

    // Someone else's press is refused with a notice; the requester's reaches the runtime.
    let press = |id: i64, user: i64, data: &str| {
        json!({"update_id": id, "callback_query": {
            "id": format!("q{id}"),
            "from": {"id": user, "is_bot": false, "first_name": "U"},
            "message": {"message_id": 77, "date": 1, "chat": {"id": 7, "type": "private"}},
            "data": data,
        }})
    };
    fake.updates.lock().unwrap().extend([
        press(10, 8, "okp:abc123:allow"),
        press(11, 7, "okp:zzz:allow"),
        press(12, 7, "okp:abc123:deny"),
    ]);
    wait_for(|| calls(&fake, "answerCallbackQuery").len() == 3).await;
    let notices: Vec<Value> = calls(&fake, "answerCallbackQuery")
        .iter()
        .map(|c| c["text"].clone())
        .collect();
    assert!(notices[0].as_str().unwrap().contains("not yours"));
    assert!(notices[1].as_str().unwrap().contains("expired"));
    assert_eq!(notices[2], Value::Null);
    let answers = sink.1.lock().unwrap().clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].prompt, "abc123");
    assert_eq!(answers[0].button, "deny");
    assert_eq!(answers[0].user.id.as_str(), "7");
    assert_eq!(answers[0].conversation, to.conversation);

    // Closing replaces the text; buttons go unless kept.
    let text = RenderedMessage {
        text: "Deploy? ⏳".into(),
    };
    channel.close_prompt(&sent, &text, true).await.unwrap();
    channel.close_prompt(&sent, &text, false).await.unwrap();
    let edits = calls(&fake, "editMessageText");
    assert_eq!(edits[0]["reply_markup"], send["reply_markup"]);
    assert_eq!(edits[1]["reply_markup"], json!({"inline_keyboard": []}));

    // Forms cannot be shown on Telegram.
    prompt.buttons[0].form = Some(okbot_core::Form {
        title: "Code".into(),
        fields: vec![],
    });
    assert!(matches!(
        channel.prompt(&to, &prompt).await,
        Err(ChannelError::Unsupported(_))
    ));
    stop.cancel();
    run.await.unwrap().unwrap();
}

/// An interactive prompt: a menu, a button and a link button.
fn size_prompt() -> okbot_core::Prompt {
    let option = |id: &str, label: &str| okbot_core::SelectOption {
        id: id.into(),
        label: label.into(),
        description: None,
    };
    okbot_core::Prompt {
        id: "v1a2b3c4d".into(),
        text: "Which size?".into(),
        buttons: vec![
            okbot_core::PromptButton {
                id: "b1".into(),
                label: "Cancel".into(),
                style: okbot_core::ButtonStyle::Secondary,
                form: None,
                url: None,
            },
            okbot_core::PromptButton {
                id: "b2".into(),
                label: "Size chart".into(),
                style: okbot_core::ButtonStyle::Primary,
                form: None,
                url: Some("https://example.com/sizes".into()),
            },
        ],
        select: Some(okbot_core::Select {
            placeholder: "Size".into(),
            options: vec![option("o1", "Small"), option("o2", "Large")],
        }),
        ..okbot_core::Prompt::default()
    }
}

#[tokio::test]
async fn interactive_prompts_survive_restarts() {
    let (fake, base) = fake_api().await;
    // The channel that showed the prompt is gone: this one never saw it.
    let channel = Arc::new(TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    )));
    let ui = channel.caps().ui;
    assert!(ui.buttons && ui.select && !ui.forms && ui.cards && ui.images);
    let prompt = size_prompt();
    let sink = Arc::new(Collect::default());
    sink.2.lock().unwrap().push(prompt.clone());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (channel, sink, stop) = (channel.clone(), sink.clone(), stop.clone());
        async move { channel.run(sink, stop).await }
    });
    wait_for(|| !calls(&fake, "getUpdates").is_empty()).await;

    // A press in a forum topic, on the message the old process sent.
    let press = |id: i64, data: &str| {
        json!({"update_id": id, "callback_query": {
            "id": format!("q{id}"),
            "from": {"id": 8, "is_bot": false, "first_name": "U"},
            "message": {"message_id": 55, "date": 1, "chat": {"id": -100, "type": "supergroup"},
                "message_thread_id": 3, "is_topic_message": true},
            "data": data,
        }})
    };
    fake.updates.lock().unwrap().extend([
        press(20, "okp:v1a2b3c4d:o2"),
        press(21, "okp:v1a2b3c4d:b2"),
        press(22, "okp:unknown1:o1"),
    ]);
    wait_for(|| calls(&fake, "answerCallbackQuery").len() == 3).await;
    let notices: Vec<Value> = calls(&fake, "answerCallbackQuery")
        .iter()
        .map(|c| c["text"].clone())
        .collect();
    assert_eq!(notices[0], Value::Null);
    assert!(
        notices[1].as_str().unwrap().contains("expired"),
        "a link button never answers"
    );
    assert!(notices[2].as_str().unwrap().contains("expired"));
    let answers = sink.1.lock().unwrap().clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].prompt, "v1a2b3c4d");
    assert_eq!(answers[0].button, "o2", "an option answers with its id");
    assert_eq!(answers[0].user.id.as_str(), "8");
    assert_eq!(answers[0].conversation.room, "-100");
    assert_eq!(answers[0].conversation.thread.as_deref(), Some("3"));
    assert_eq!(answers[0].sent.message_id.as_str(), "55");

    // The runtime replaces the message: still open first, then closed.
    let sent = answers[0].sent.clone();
    let mut next = prompt.clone();
    next.text = "Which size? (Large picked by U)".into();
    channel.update_prompt(&sent, &next).await.unwrap();
    next.select = None;
    next.buttons.remove(0);
    next.text = "Size: Large".into();
    channel.update_prompt(&sent, &next).await.unwrap();
    let edits = calls(&fake, "editMessageText");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0]["chat_id"], -100);
    assert_eq!(edits[0]["message_id"], 55);
    let rows = edits[0]["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 3, "two options and a row of buttons");
    assert_eq!(rows[0][0]["callback_data"], "okp:v1a2b3c4d:o1");
    assert_eq!(rows[2][1]["url"], "https://example.com/sizes");
    assert_eq!(edits[1]["text"], "Size: Large");
    assert_eq!(
        edits[1]["reply_markup"]["inline_keyboard"],
        json!([[{"text": "Size chart", "url": "https://example.com/sizes"}]]),
        "only the link is left"
    );

    // Closed: the channel forgot it, and the runtime no longer waits on it.
    sink.2.lock().unwrap().clear();
    fake.updates
        .lock()
        .unwrap()
        .push_back(press(23, "okp:v1a2b3c4d:o1"));
    wait_for(|| calls(&fake, "answerCallbackQuery").len() == 4).await;
    assert_eq!(sink.1.lock().unwrap().len(), 1);
    stop.cancel();
    run.await.unwrap().unwrap();
}

/// A prompt with a chart on its card, and `body` as the card's text.
fn chart_prompt(body: &str) -> okbot_core::Prompt {
    let mut prompt = size_prompt();
    prompt.text = "Weekly sales".into();
    prompt.cards = vec![okbot_core::Card {
        title: Some("Top <5>".into()),
        tone: okbot_core::Tone::Success,
        text: body.into(),
        image: Some(okbot_core::Image {
            name: "chart.png".into(),
            alt: "Bar chart, Top 5".into(),
            width: 1200,
            height: 800,
            png: b"PNGDATA".to_vec(),
        }),
    }];
    prompt
}

fn room_target() -> ReplyTarget {
    ReplyTarget {
        conversation: okbot_core::ConversationKey::room("telegram:shop".parse().unwrap(), "-100"),
        in_reply_to: Some(MessageId::new("5")),
    }
}

#[tokio::test]
async fn charts_go_as_a_photo_with_caption_and_buttons() {
    let (fake, base) = fake_api().await;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let to = room_target();
    let prompt = chart_prompt("<i>Phở</i> leads");
    let sent = channel.prompt(&to, &prompt).await.unwrap();
    assert_eq!(
        sent.message_id.as_str(),
        "88",
        "the photo carries the buttons"
    );
    assert_eq!(
        calls(&fake, "sendMessage"),
        Vec::<Value>::new(),
        "one message"
    );
    let photo = &calls(&fake, "sendPhoto")[0];
    assert_eq!(
        photo["photo"],
        json!({"filename": "chart.png", "content_type": "image/png", "data": "PNGDATA"})
    );
    assert_eq!(
        photo["caption"],
        "Weekly sales\n\n<b>✅ Top &lt;5&gt;</b>\n<i>Phở</i> leads"
    );
    assert_eq!(photo["parse_mode"], "HTML");
    assert_eq!(photo["chat_id"], "-100");
    assert_eq!(
        serde_json::from_str::<Value>(photo["reply_parameters"].as_str().unwrap()).unwrap()["message_id"],
        5
    );
    let markup: Value = serde_json::from_str(photo["reply_markup"].as_str().unwrap()).unwrap();
    assert_eq!(
        markup["inline_keyboard"][0][0]["callback_data"],
        "okp:v1a2b3c4d:o1"
    );

    // Closing after a click: the prompt comes back from the store without the PNG. The caption
    // is edited; the photo stays.
    let mut closed = prompt.clone();
    closed.cards[0].image.as_mut().unwrap().png.clear();
    closed.select = None;
    closed.buttons.clear();
    closed.text = "Size: Large".into();
    channel.update_prompt(&sent, &closed).await.unwrap();
    assert_eq!(calls(&fake, "editMessageText"), Vec::<Value>::new());
    let edit = &calls(&fake, "editMessageCaption")[0];
    assert_eq!(edit["message_id"], 88);
    assert!(
        edit["caption"]
            .as_str()
            .unwrap()
            .starts_with("Size: Large\n\n")
    );
    assert_eq!(edit["reply_markup"], json!({"inline_keyboard": []}));
    assert_eq!(edit["parse_mode"], "HTML");

    // A close with a rendered message (not from the prompt) also reaches the caption.
    channel
        .close_prompt(
            &sent,
            &RenderedMessage {
                text: "Done".into(),
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(calls(&fake, "editMessageText").len(), 1, "tried first");
    assert_eq!(calls(&fake, "editMessageCaption")[1]["caption"], "Done");
}

#[tokio::test]
async fn long_chart_text_goes_in_its_own_message() {
    let (fake, base) = fake_api().await;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let to = room_target();
    let prompt = chart_prompt(&"x".repeat(1100));
    let sent = channel.prompt(&to, &prompt).await.unwrap();
    let photo = &calls(&fake, "sendPhoto")[0];
    assert_eq!(photo["caption"], "<b>✅ Top &lt;5&gt;</b>");
    assert!(photo.get("reply_markup").is_none());
    let text = &calls(&fake, "sendMessage")[0];
    assert!(text["text"].as_str().unwrap().ends_with(&"x".repeat(1100)));
    assert_eq!(
        text["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
        "okp:v1a2b3c4d:o1"
    );
    assert_eq!(
        sent.message_id.as_str(),
        "77",
        "the message with the buttons"
    );

    // Its update edits the text message; a short text tries the caption first and falls back.
    let mut next = prompt.clone();
    next.cards[0].image.as_mut().unwrap().png.clear();
    next.text = "Picked".into();
    channel.update_prompt(&sent, &next).await.unwrap();
    assert_eq!(calls(&fake, "editMessageCaption"), Vec::<Value>::new());
    assert_eq!(calls(&fake, "editMessageText")[0]["message_id"], 77);
    next.cards[0].text = "short".into();
    channel.update_prompt(&sent, &next).await.unwrap();
    assert_eq!(calls(&fake, "editMessageCaption").len(), 1);
    assert_eq!(calls(&fake, "editMessageText").len(), 2);
}

#[tokio::test]
async fn cards_without_charts_are_text() {
    let (fake, base) = fake_api().await;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let mut prompt = chart_prompt("<i>Phở</i> leads");
    prompt.cards[0].image = None;
    channel.prompt(&room_target(), &prompt).await.unwrap();
    assert_eq!(calls(&fake, "sendPhoto"), Vec::<Value>::new());
    assert_eq!(
        calls(&fake, "sendMessage")[0]["text"],
        "Weekly sales\n\n<b>✅ Top &lt;5&gt;</b>\n<i>Phở</i> leads"
    );
}

#[tokio::test]
async fn a_failed_photo_is_an_error() {
    let (fake, base) = fake_api().await;
    *fake.reject_token.lock().unwrap() = true;
    let channel = TelegramChannel::new(config(
        &base,
        Delivery::Polling {
            timeout: Duration::from_secs(1),
        },
    ));
    let error = channel
        .prompt(&room_target(), &chart_prompt("body"))
        .await
        .unwrap_err();
    assert!(!format!("{error} {error:?}").contains("abc-xyz"));
    assert!(
        calls(&fake, "sendMessage").is_empty(),
        "the runtime decides what to send"
    );
}
