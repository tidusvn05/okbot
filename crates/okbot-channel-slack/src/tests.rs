use std::sync::Mutex;

use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use okbot_core::{InboundEvent, SinkClosed};
use tokio_websockets::ServerBuilder;

use super::*;

fn mention(text: &str) -> Value {
    json!({"team_id": "T1", "event_id": "Ev1", "event": {
        "type": "app_mention", "user": "U7", "text": format!("<@UBOT> {text}"),
        "ts": "1760000000.000100", "channel": "C1", "channel_type": "channel"}})
}

#[test]
fn maps_events() {
    let ch: ChannelId = "slack:acme".parse().unwrap();
    let e = event::to_inbound(&ch, &mention("giá?"), "UBOT").unwrap();
    assert_eq!(e.text, "giá?");
    assert_eq!(e.conversation.thread.as_deref(), Some("1760000000.000100"));
    assert!(e.mentions_bot && !e.is_direct);
    assert_eq!(e.event_id, "Ev1");

    let dm = json!({"event": {"type": "message", "channel_type": "im", "user": "U7", "text": "hi",
                              "ts": "1.2", "channel": "D1"}});
    let e = event::to_inbound(&ch, &dm, "UBOT").unwrap();
    assert!(e.is_direct && e.conversation.thread.is_none());

    let edited = json!({"event": {"type": "message", "subtype": "message_changed", "channel_type": "im",
                                  "user": "U7", "ts": "1.3", "channel": "D1"}});
    assert!(event::to_inbound(&ch, &edited, "UBOT").is_none());
    let channel_chatter = json!({"event": {"type": "message", "channel_type": "channel", "user": "U7",
                                           "ts": "1.4", "channel": "C1", "text": "x"}});
    assert!(event::to_inbound(&ch, &channel_chatter, "UBOT").is_none());
}

#[derive(Default)]
struct Calls(Mutex<Vec<(String, Option<String>, Value)>>);

async fn web(
    State(state): State<(Arc<Calls>, String)>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> axum::Json<Value> {
    let (calls, ws_url) = state;
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = uri.path().trim_start_matches('/').to_string();
    calls.0.lock().unwrap().push((method.clone(), auth, body));
    axum::Json(match method.as_str() {
        "apps.connections.open" => json!({"ok": true, "url": ws_url}),
        "auth.test" => json!({"ok": true, "user_id": "UBOT"}),
        "chat.postMessage" => json!({"ok": true, "ts": "1760000000.000200"}),
        _ => json!({"ok": true}),
    })
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

#[tokio::test]
async fn socket_mode_end_to_end() {
    // A fake Socket Mode server: hello, one mention, then waits for the ack.
    let ws = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}/", ws.local_addr().unwrap());
    let acked = Arc::new(Mutex::new(Vec::<String>::new()));
    let acked_server = acked.clone();
    tokio::spawn(async move {
        let (stream, _) = ws.accept().await.unwrap();
        let (_, mut socket) = ServerBuilder::new().accept(stream).await.unwrap();
        socket
            .send(Message::text(json!({"type": "hello"}).to_string()))
            .await
            .unwrap();
        let envelope =
            json!({"envelope_id": "env-1", "type": "events_api", "payload": mention("hello")});
        socket
            .send(Message::text(envelope.to_string()))
            .await
            .unwrap();
        while let Some(Ok(frame)) = socket.next().await {
            if let Some(text) = frame.as_text() {
                acked_server.lock().unwrap().push(text.to_string());
            }
        }
    });
    let calls = Arc::new(Calls::default());
    let app = axum::Router::new()
        .fallback(web)
        .with_state((calls.clone(), ws_url));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let channel = Arc::new(SlackChannel::new(SlackConfig {
        id: "slack:acme".parse().unwrap(),
        app_token: Secret::new("xapp-1".into()),
        bot_token: Secret::new("xoxb-1".into()),
        api_base: base,
    }));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });
    for _ in 0..100 {
        if !sink.0.lock().unwrap().is_empty() && !acked.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        acked.lock().unwrap()[0],
        json!({"envelope_id": "env-1"}).to_string()
    );
    let event = sink.0.lock().unwrap()[0].clone();
    assert_eq!(event.text, "hello");

    let to = ReplyTarget::to_event(&event);
    let sent = channel
        .send(
            &to,
            &RenderedMessage {
                text: "_Thinking…_".into(),
            },
        )
        .await
        .unwrap();
    channel
        .edit(
            &sent,
            &RenderedMessage {
                text: "*answer*".into(),
            },
        )
        .await
        .unwrap();
    stop.cancel();
    run.await.unwrap().unwrap();

    let calls = calls.0.lock().unwrap().clone();
    let auth = |m: &str| calls.iter().find(|c| c.0 == m).unwrap().1.clone().unwrap();
    assert_eq!(auth("apps.connections.open"), "Bearer xapp-1");
    assert_eq!(auth("chat.postMessage"), "Bearer xoxb-1");
    let post = calls.iter().find(|c| c.0 == "chat.postMessage").unwrap();
    assert_eq!(post.2["thread_ts"], "1760000000.000100");
    let update = calls.iter().find(|c| c.0 == "chat.update").unwrap();
    assert_eq!(
        (update.2["ts"].as_str(), update.2["text"].as_str()),
        (Some("1760000000.000200"), Some("*answer*"))
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, told in order
async fn prompts_are_block_kit_buttons() {
    // A fake Socket Mode server that sends presses once the test says so.
    let ws = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}/", ws.local_addr().unwrap());
    let (press_tx, mut press_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let (stream, _) = ws.accept().await.unwrap();
        let (_, mut socket) = ServerBuilder::new().accept(stream).await.unwrap();
        loop {
            tokio::select! {
                press = press_rx.recv() => {
                    let Some(payload) = press else { break };
                    let envelope = json!({"envelope_id": "e", "type": "interactive", "payload": payload});
                    socket.send(Message::text(envelope.to_string())).await.unwrap();
                }
                frame = socket.next() => if frame.is_none() { break },
            }
        }
    });
    let calls = Arc::new(Calls::default());
    let app = axum::Router::new()
        .fallback(web)
        .with_state((calls.clone(), ws_url));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let channel = Arc::new(SlackChannel::new(SlackConfig {
        id: "slack:acme".parse().unwrap(),
        app_token: Secret::new("xapp-1".into()),
        bot_token: Secret::new("xoxb-1".into()),
        api_base: base,
    }));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });

    let mut conversation = okbot_core::ConversationKey::room("slack:acme".parse().unwrap(), "C1");
    conversation.thread = Some("1.0".into());
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
        allowed: vec![okbot_core::UserId::new("U7")],
    };
    let sent = channel.prompt(&to, &prompt).await.unwrap();
    let press = |user: &str, action: &str| {
        json!({"type": "block_actions", "user": {"id": user, "username": "lan"},
               "actions": [{"action_id": action, "value": "x"}]})
    };
    press_tx.send(press("U8", "okp:abc123:allow")).unwrap();
    press_tx.send(press("U7", "okp:abc123:deny")).unwrap();
    for _ in 0..100 {
        if !sink.1.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let answers = sink.1.lock().unwrap().clone();
    assert_eq!(answers.len(), 1, "only the allowed person's press");
    assert_eq!(
        (answers[0].prompt.as_str(), answers[0].button.as_str()),
        ("abc123", "deny")
    );
    assert_eq!(answers[0].conversation, conversation);
    assert_eq!(answers[0].sent.message_id.as_str(), "1760000000.000200");

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

    let calls = calls.0.lock().unwrap().clone();
    let post = calls.iter().find(|c| c.0 == "chat.postMessage").unwrap();
    assert_eq!(
        post.2["blocks"][1]["elements"][1]["action_id"],
        "okp:abc123:deny"
    );
    assert_eq!(post.2["thread_ts"], "1.0");
    let ephemeral = calls.iter().find(|c| c.0 == "chat.postEphemeral").unwrap();
    assert_eq!(ephemeral.2["user"], "U8");
    let update = calls.iter().find(|c| c.0 == "chat.update").unwrap();
    assert_eq!(
        update.2["blocks"].as_array().unwrap().len(),
        1,
        "buttons removed"
    );
}
