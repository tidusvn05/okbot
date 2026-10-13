//! The Zalo channel against a fake Zalo Bot API.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::routing::post;
use okbot_channel_zalo::{Delivery, ZaloChannel, ZaloConfig};
use okbot_core::{
    CancellationToken, Channel, ChannelError, InboundEvent, InboundSink, RenderedMessage,
    ReplyTarget, Secret, SinkClosed, WebhookRequest, async_trait,
};
use serde_json::{Value, json};

const TOKEN: &str = "12345:abc-xyz";

#[derive(Default)]
struct Fake {
    updates: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
    reject_token: Mutex<bool>,
}

async fn handle(
    State(fake): State<Arc<Fake>>,
    Path((bot, method)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    if bot != format!("bot{TOKEN}") || *fake.reject_token.lock().unwrap() {
        return Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"}));
    }
    fake.calls
        .lock()
        .unwrap()
        .push((method.clone(), body.clone()));
    Json(match method.as_str() {
        "getMe" => {
            json!({"ok": true, "result": {"id": "1", "account_name": "bot.Shop", "account_type": "BASIC", "can_join_groups": false}})
        }
        "getUpdates" => {
            let next = fake.updates.lock().unwrap().pop_front();
            if let Some(update) = next {
                json!({"ok": true, "result": update})
            } else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                json!({"ok": false, "error_code": 408, "description": "Request timeout"})
            }
        }
        "sendMessage" => json!({"ok": true, "result": {"message_id": "sent-1", "date": 1}}),
        "setWebhook" => {
            json!({"ok": true, "result": {"url": body["url"], "verification": {"ok": false, "outcome": "webhook.err.dns", "hint": "x"}}})
        }
        _ => json!({"ok": true, "result": {}}),
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
struct Collect(Mutex<Vec<InboundEvent>>);

#[async_trait]
impl InboundSink for Collect {
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

fn update(text: &str) -> Value {
    json!({"event_name": "message.text.received", "message": {
        "from": {"id": "u1", "display_name": "Lan", "is_bot": false},
        "chat": {"id": "u1", "chat_type": "PRIVATE"},
        "text": text, "message_id": format!("m-{text}"), "date": 1_750_000_000_000_i64}})
}

fn config(base: &str, delivery: Delivery) -> ZaloConfig {
    ZaloConfig {
        id: "zalo:shop".parse().unwrap(),
        token: Secret::new(TOKEN.into()),
        delivery,
        api_base: base.into(),
    }
}

fn methods(fake: &Fake) -> Vec<String> {
    fake.calls
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}

#[tokio::test]
async fn polls_delivers_and_replies() {
    let (fake, base) = fake_api().await;
    fake.updates.lock().unwrap().push_back(update("Xin chào"));
    let channel = Arc::new(ZaloChannel::new(config(
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
    for _ in 0..100 {
        if !sink.0.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let event = sink.0.lock().unwrap()[0].clone();
    assert_eq!(event.text, "Xin chào");
    assert_eq!(
        methods(&fake)[0],
        "deleteWebhook",
        "polling first removes any webhook"
    );

    let to = ReplyTarget::to_event(&event);
    channel.typing(&to).await.unwrap();
    let sent = channel
        .send(
            &to,
            &RenderedMessage {
                text: "**Chào** Lan".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_str(), "sent-1");
    let calls = fake.calls.lock().unwrap().clone();
    let (_, send) = calls.iter().find(|(m, _)| m == "sendMessage").unwrap();
    assert_eq!(send["chat_id"], "u1");
    assert_eq!(send["parse_mode"], "markdown");
    let (_, action) = calls.iter().find(|(m, _)| m == "sendChatAction").unwrap();
    assert_eq!(action["action"], "typing");

    stop.cancel();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn rejected_token_stops_the_channel() {
    let (fake, base) = fake_api().await;
    *fake.reject_token.lock().unwrap() = true;
    let channel = ZaloChannel::new(config(
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
    let channel = Arc::new(ZaloChannel::new(config(
        &base,
        Delivery::Webhook {
            public_url: "https://bot.example.com/zalo/shop".into(),
            secret: Secret::new("s3cret-value".into()),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (channel, sink, stop) = (channel.clone(), sink.clone(), stop.clone());
        async move { channel.run(sink, stop).await }
    });
    for _ in 0..100 {
        if methods(&fake).contains(&"setWebhook".to_string()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let calls = fake.calls.lock().unwrap().clone();
    let (_, set) = calls.iter().find(|(m, _)| m == "setWebhook").unwrap();
    assert_eq!(set["url"], "https://bot.example.com/zalo/shop");
    assert_eq!(set["secret_token"], "s3cret-value");

    let handler = channel.webhook().unwrap();
    assert_eq!(handler.path(), "zalo/shop");
    let request = |secret: &str| WebhookRequest {
        method: "POST".into(),
        path: String::new(),
        headers: vec![("x-bot-api-secret-token".into(), secret.into())],
        body: serde_json::to_vec(&json!({"ok": true, "result": update("hello")})).unwrap(),
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
    let api = okbot_channel_zalo::client("http://127.0.0.1:9", &Secret::new(TOKEN.into()));
    let error = api.get_me().await.unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(!text.contains("abc-xyz"), "{text}");
    assert!(format!("{api:?}").find("abc-xyz").is_none());
}
