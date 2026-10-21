//! The channel against a fake Google (OAuth, Chat API, Pub/Sub, JWKS).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, Method, Uri};
use base64::Engine as _;
use okbot_core::{
    CancellationToken, Channel, InboundEvent, InboundSink, RenderedMessage, ReplyTarget,
    SinkClosed, WebhookRequest, async_trait,
};
use serde_json::{Value, json};

use super::*;

/// `(method, path and query, authorization, body)`.
type Call = (String, String, Option<String>, Value);

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Call>>,
    pulls: Mutex<Vec<Value>>,
    jwks: Mutex<Value>,
    sa_jwks: Mutex<Value>,
}

async fn token(
    State(fake): State<Arc<Fake>>,
    Form(form): Form<Vec<(String, String)>>,
) -> Json<Value> {
    let get = |k: &str| {
        form.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert_eq!(
        get("grant_type"),
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );
    let claims = jwt::verify(&get("assertion"), &fake.sa_jwks.lock().unwrap())
        .expect("signed by the service account");
    assert_eq!(claims["iss"], "okbot@test.iam.gserviceaccount.com");
    assert!(claims["scope"].as_str().unwrap().contains("chat.bot"));
    Json(json!({"access_token": "at-1", "expires_in": 3600}))
}

async fn any(
    State(fake): State<Arc<Fake>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let path = uri.to_string();
    fake.calls
        .lock()
        .unwrap()
        .push((method.to_string(), path.clone(), auth, body));
    if path.ends_with(":pull") {
        let next = fake.pulls.lock().unwrap().pop();
        return Json(match next {
            Some(event) => json!({"receivedMessages": [{"ackId": "ack-1", "message": {
                "data": base64::engine::general_purpose::STANDARD.encode(event.to_string())}}]}),
            None => json!({}),
        });
    }
    if path == "/jwks" {
        return Json(fake.jwks.lock().unwrap().clone());
    }
    Json(json!({"name": "spaces/AAA/messages/out-1"}))
}

struct Env {
    fake: Arc<Fake>,
    base: String,
    google_key: Vec<u8>,
    account: ServiceAccount,
}

async fn env() -> Env {
    let (sa_der, sa_jwks) = jwt::testing::key("sa-key");
    let (google_der, google_jwks) = jwt::testing::key("google-key");
    let fake = Arc::new(Fake::default());
    *fake.jwks.lock().unwrap() = google_jwks;
    *fake.sa_jwks.lock().unwrap() = sa_jwks;
    let app = axum::Router::new()
        .route("/token", axum::routing::post(token))
        .fallback(any)
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(&sa_der)
    );
    let account = ServiceAccount::from_json(
        &json!({"type": "service_account", "client_email": "okbot@test.iam.gserviceaccount.com",
                "private_key": pem, "private_key_id": "sa-key", "project_id": "test"})
        .to_string(),
    )
    .unwrap();
    Env {
        fake,
        base,
        google_key: google_der,
        account,
    }
}

fn config(env: &Env, delivery: Delivery) -> GChatConfig {
    GChatConfig {
        id: "gchat:corp".parse().unwrap(),
        account: env.account.clone(),
        delivery,
        endpoints: Endpoints {
            chat: env.base.clone(),
            pubsub: env.base.clone(),
            token: Some(format!("{}/token", env.base)),
            jwks: Some(format!("{}/jwks", env.base)),
        },
    }
}

#[derive(Default)]
struct Collect(
    Mutex<Vec<InboundEvent>>,
    Mutex<Vec<okbot_core::PromptAnswer>>,
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
}

fn message_event(text: &str) -> Value {
    json!({"type": "MESSAGE",
           "message": {"name": "spaces/AAA/messages/in-1", "argumentText": text,
                       "sender": {"name": "users/1", "displayName": "An", "type": "HUMAN"},
                       "thread": {"name": "spaces/AAA/threads/T"}},
           "space": {"name": "spaces/AAA", "spaceType": "SPACE"}})
}

#[tokio::test]
async fn pubsub_delivery_and_replies() {
    let env = env().await;
    env.fake
        .pulls
        .lock()
        .unwrap()
        .push(message_event("giá gói Pro?"));
    let channel = Arc::new(GChatChannel::new(config(
        &env,
        Delivery::PubSub {
            subscription: "projects/p/subscriptions/s".into(),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });
    for _ in 0..100 {
        if !sink.0.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let event = sink.0.lock().unwrap()[0].clone();
    assert_eq!(event.text, "giá gói Pro?");
    assert_eq!(
        event.conversation.thread.as_deref(),
        Some("spaces/AAA/threads/T")
    );

    let to = ReplyTarget::to_event(&event);
    let sent = channel
        .send(
            &to,
            &RenderedMessage {
                text: "*Thinking…*".into(),
            },
        )
        .await
        .unwrap();
    channel
        .edit(
            &sent,
            &RenderedMessage {
                text: "Gói Pro giá 250k".into(),
            },
        )
        .await
        .unwrap();
    stop.cancel();
    run.await.unwrap().unwrap();

    let calls = env.fake.calls.lock().unwrap().clone();
    let find = |m: &str, p: &str| {
        calls
            .iter()
            .find(|c| c.0 == m && c.1.starts_with(p))
            .cloned()
            .unwrap()
    };
    let ack = find("POST", "/v1/projects/p/subscriptions/s:acknowledge");
    assert_eq!(ack.3["ackIds"][0], "ack-1");
    let create = find(
        "POST",
        "/v1/spaces/AAA/messages?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
    );
    assert_eq!(create.2.as_deref(), Some("Bearer at-1"));
    assert_eq!(create.3["thread"]["name"], "spaces/AAA/threads/T");
    let patch = find("PATCH", "/v1/spaces/AAA/messages/out-1?updateMask=text");
    assert_eq!(patch.3["text"], "Gói Pro giá 250k");
}

#[tokio::test]
async fn webhook_accepts_only_google_signed_requests() {
    let env = env().await;
    let url = "https://bot.example.com/gchat/corp";
    let channel = Arc::new(GChatChannel::new(config(
        &env,
        Delivery::Http {
            audience: Audience::EndpointUrl(url.into()),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });
    let handler = channel.webhook().unwrap();
    assert_eq!(handler.path(), "gchat/corp");
    let now = jiff::Timestamp::now().as_second();
    let sign = |claims: Value| jwt::sign(&claims, &env.google_key, Some("google-key")).unwrap();
    let good = sign(
        json!({"iss": "https://accounts.google.com", "email": CHAT_ISSUER,
                           "email_verified": true, "aud": url, "iat": now, "exp": now + 300}),
    );
    let wrong_aud = sign(
        json!({"iss": "https://accounts.google.com", "email": CHAT_ISSUER,
                                "aud": "https://other", "iat": now, "exp": now + 300}),
    );
    let request = |auth: Option<String>| WebhookRequest {
        method: "POST".into(),
        path: String::new(),
        headers: auth
            .map(|t| vec![("authorization".to_string(), format!("Bearer {t}"))])
            .unwrap_or_default(),
        body: message_event("hello").to_string().into_bytes(),
    };
    // Wait until run() has set the sink.
    for _ in 0..100 {
        if channel.shared.sink.get().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(handler.handle(request(None)).await.status, 401);
    assert_eq!(handler.handle(request(Some(wrong_aud))).await.status, 401);
    assert_eq!(
        handler
            .handle(request(Some("not.a.jwt".into())))
            .await
            .status,
        401
    );
    assert!(sink.0.lock().unwrap().is_empty());
    let ok = handler.handle(request(Some(good))).await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body, b"{}");
    assert_eq!(sink.0.lock().unwrap()[0].text, "hello");
    stop.cancel();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn prompts_are_card_buttons() {
    let env = env().await;
    let channel = Arc::new(GChatChannel::new(config(
        &env,
        Delivery::PubSub {
            subscription: "projects/p/subscriptions/s".into(),
        },
    )));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });
    let mut conversation =
        okbot_core::ConversationKey::room("gchat:corp".parse().unwrap(), "spaces/AAA");
    conversation.thread = Some("spaces/AAA/threads/T".into());
    let to = ReplyTarget {
        conversation: conversation.clone(),
        in_reply_to: None,
    };
    let button = |id: &str| okbot_core::PromptButton {
        id: id.into(),
        label: id.into(),
        style: okbot_core::ButtonStyle::Primary,
        form: None,
    };
    let prompt = okbot_core::Prompt {
        id: "abc123".into(),
        text: "Deploy?".into(),
        buttons: vec![button("allow"), button("deny")],
        allowed: vec![okbot_core::UserId::new("users/7")],
    };
    let sent = channel.prompt(&to, &prompt).await.unwrap();
    let click = |user: &str, button: &str| {
        json!({"type": "CARD_CLICKED", "user": {"name": user, "displayName": "U"},
               "action": {"actionMethodName": "okp", "parameters": [
                   {"key": "prompt", "value": "abc123"}, {"key": "button", "value": button}]},
               "space": {"name": "spaces/AAA"}})
    };
    env.fake
        .pulls
        .lock()
        .unwrap()
        .extend([click("users/7", "deny"), click("users/8", "allow")]);
    for _ in 0..100 {
        if env.fake.pulls.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let answers = sink.1.lock().unwrap().clone();
    assert_eq!(answers.len(), 1, "only the allowed person's click");
    assert_eq!(answers[0].button, "deny");
    assert_eq!(answers[0].conversation, conversation);
    channel
        .close_prompt(
            &sent,
            &RenderedMessage {
                text: "Deploy? ❌".into(),
            },
            false,
        )
        .await
        .unwrap();
    stop.cancel();
    run.await.unwrap().unwrap();

    let calls = env.fake.calls.lock().unwrap().clone();
    let create = calls
        .iter()
        .find(|c| c.0 == "POST" && c.1.starts_with("/v1/spaces/AAA/messages"))
        .unwrap();
    let buttons =
        &create.3["cardsV2"][0]["card"]["sections"][0]["widgets"][0]["buttonList"]["buttons"];
    assert_eq!(buttons[1]["onClick"]["action"]["function"], "okp");
    assert_eq!(
        buttons[1]["onClick"]["action"]["parameters"][1]["value"],
        "deny"
    );
    let patch = calls.iter().find(|c| c.0 == "PATCH").unwrap();
    assert!(patch.1.ends_with("updateMask=text,cardsV2"), "{}", patch.1);
    assert_eq!(patch.3["cardsV2"], json!([]));
}
