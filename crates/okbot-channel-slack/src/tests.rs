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
    /// Prompts the runtime still waits on (after a restart).
    Mutex<Vec<Prompt>>,
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

    async fn prompt(&self, id: &str) -> Option<Prompt> {
        self.2.lock().unwrap().iter().find(|p| p.id == id).cloned()
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
        url: None,
    };
    let prompt = okbot_core::Prompt {
        id: "abc123".into(),
        text: "Deploy?".into(),
        buttons: vec![button("allow"), button("deny")],
        allowed: vec![okbot_core::UserId::new("U7")],
        cards: Vec::new(),
        select: None,
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

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, told in order
async fn interactive_prompts_menus_forms_and_restarts() {
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
    assert_eq!(channel.caps().ui, okbot_core::UiCaps::all());
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });

    // A prompt the bot showed before a restart: only the runtime still knows it.
    let form = okbot_core::Form {
        title: "Order".into(),
        fields: vec![okbot_core::FormField {
            id: "f1".into(),
            label: "Address".into(),
            style: okbot_core::FieldStyle::Paragraph,
            required: true,
            placeholder: None,
            min_len: None,
            max_len: None,
            value: Some("12 Lý Thường Kiệt".into()),
        }],
    };
    let mut link = okbot_core::PromptButton {
        id: "b2".into(),
        label: "Docs".into(),
        style: okbot_core::ButtonStyle::Secondary,
        form: None,
        url: Some("https://example.com".into()),
    };
    let order = okbot_core::PromptButton {
        id: "b1".into(),
        label: "Order".into(),
        style: okbot_core::ButtonStyle::Primary,
        form: Some(form),
        url: None,
    };
    let prompt = Prompt {
        id: "v1a2b3c4d".into(),
        text: "Pick one".into(),
        buttons: vec![order, link.clone()],
        cards: vec![okbot_core::Card {
            title: Some("Rice".into()),
            tone: okbot_core::Tone::Info,
            text: "30 bags".into(),
            image: None,
        }],
        select: Some(okbot_core::Select {
            placeholder: "Product".into(),
            options: ["o1", "o2"]
                .map(|id| okbot_core::SelectOption {
                    id: id.into(),
                    label: id.into(),
                    description: None,
                })
                .to_vec(),
        }),
        ..Prompt::default()
    };
    sink.2.lock().unwrap().push(prompt.clone());
    let base = json!({"type": "block_actions", "trigger_id": "trig-1",
        "user": {"id": "U7", "username": "lan"}, "team": {"id": "T1"}, "channel": {"id": "C1"},
        "message": {"ts": "5.0", "thread_ts": "1.0"}});
    let with = |action: Value| {
        let mut p = base.clone();
        p["actions"] = json!([action]);
        p
    };
    // A link press is ignored; a pick answers with the option id.
    press_tx
        .send(with(json!({"action_id": "okl:v1a2b3c4d:b2"})))
        .unwrap();
    press_tx
        .send(with(
            json!({"action_id": "oks:v1a2b3c4d", "selected_option": {"value": "o2"}}),
        ))
        .unwrap();
    // A press of the form's button opens a modal with the trigger id.
    press_tx
        .send(with(json!({"action_id": "okp:v1a2b3c4d:b1"})))
        .unwrap();
    // An unknown prompt has expired.
    press_tx
        .send(with(json!({"action_id": "okp:gone:b1"})))
        .unwrap();
    let modal = loop {
        let found = calls
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.0 == "views.open")
            .cloned();
        if let Some(found) = found {
            break found.2;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(modal["trigger_id"], "trig-1");
    let view = &modal["view"];
    assert_eq!(
        view["blocks"][0]["element"]["initial_value"],
        "12 Lý Thường Kiệt"
    );
    let submission = json!({"type": "view_submission", "user": {"id": "U7", "name": "lan"},
        "view": {"callback_id": "okf", "private_metadata": view["private_metadata"],
                 "state": {"values": {"f1": {"v": {"value": "34 Hai Bà Trưng"}}}}}});
    press_tx.send(submission).unwrap();
    for _ in 0..100 {
        if sink.1.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let answers = sink.1.lock().unwrap().clone();
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].button, "o2");
    assert!(answers[0].values.is_empty());
    assert_eq!(answers[0].sent.message_id.as_str(), "5.0");
    assert_eq!(answers[0].conversation.thread.as_deref(), Some("1.0"));
    assert_eq!(answers[0].conversation.space.as_deref(), Some("T1"));
    assert_eq!(answers[1].button, "b1");
    assert_eq!(answers[1].values["f1"].expose(), "34 Hai Bà Trưng");
    assert_eq!(answers[1].conversation, answers[0].conversation);

    // Closed: the cards stay, the actions go.
    let closed = Prompt {
        buttons: Vec::new(),
        select: None,
        ..prompt.clone()
    };
    channel
        .update_prompt(&answers[0].sent, &closed)
        .await
        .unwrap();
    // Still open with a link: the actions stay.
    link.id = "b3".into();
    let still = Prompt {
        buttons: vec![link],
        ..prompt.clone()
    };
    channel
        .update_prompt(&answers[0].sent, &still)
        .await
        .unwrap();
    stop.cancel();
    run.await.unwrap().unwrap();

    let calls = calls.0.lock().unwrap().clone();
    let expired = calls.iter().find(|c| c.0 == "chat.postEphemeral").unwrap();
    assert_eq!(expired.2["text"], "⌛ This prompt has expired.");
    assert_eq!(expired.2["thread_ts"], "1.0");
    let updates: Vec<&Value> = calls
        .iter()
        .filter(|c| c.0 == "chat.update")
        .map(|c| &c.2)
        .collect();
    assert_eq!(updates.len(), 2);
    let kinds = |u: &Value| -> Vec<String> {
        u["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["type"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        kinds(updates[0]),
        ["section", "divider", "header", "section"]
    );
    assert_eq!(updates[0]["ts"], "5.0");
    assert_eq!(updates[0]["text"], "Pick one\n\nRice\n\n30 bags");
    assert_eq!(kinds(updates[1]).last().unwrap(), "actions");
}

/// A fake Web API for chart uploads: records `(path, authorization, content type, body)`;
/// `missing_scope` makes `files.getUploadURLExternal` fail like an app without `files:write`.
/// One request: path, authorization, content type, body.
type Request = (String, Option<String>, String, Vec<u8>);

#[derive(Default)]
struct Uploads {
    calls: Mutex<Vec<Request>>,
    base: Mutex<String>,
    missing_scope: bool,
}

async fn upload_api(
    State(state): State<Arc<Uploads>>,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::Json<Value> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let path = uri.path().trim_start_matches('/').to_string();
    state.calls.lock().unwrap().push((
        path.clone(),
        header("authorization"),
        header("content-type").unwrap_or_default(),
        body.to_vec(),
    ));
    let base = state.base.lock().unwrap().clone();
    axum::Json(match path.as_str() {
        "files.getUploadURLExternal" if state.missing_scope => {
            json!({"ok": false, "error": "missing_scope", "needed": "files:write"})
        }
        "files.getUploadURLExternal" => {
            json!({"ok": true, "upload_url": format!("{base}/upload/F1?sig=secret"), "file_id": "F1"})
        }
        "chat.postMessage" => json!({"ok": true, "ts": "1760000000.000300"}),
        _ => json!({"ok": true}),
    })
}

fn chart_prompt() -> Prompt {
    Prompt {
        id: "p1".into(),
        text: "Sales".into(),
        cards: vec![okbot_core::Card {
            title: Some("Weekly sales".into()),
            tone: okbot_core::Tone::default(),
            text: "Top: Phở bò".into(),
            image: Some(okbot_core::Image {
                name: "chart.png".into(),
                alt: "Bar chart, Weekly sales".into(),
                width: 1200,
                height: 600,
                png: b"\x89PNG fake".to_vec(),
            }),
        }],
        ..Prompt::default()
    }
}

async fn upload_channel(missing_scope: bool) -> (Arc<Uploads>, SlackChannel) {
    let state = Arc::new(Uploads {
        missing_scope,
        ..Uploads::default()
    });
    let app = axum::Router::new()
        .fallback(upload_api)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    state.base.lock().unwrap().clone_from(&base);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let channel = SlackChannel::new(SlackConfig {
        id: "slack:acme".parse().unwrap(),
        app_token: Secret::new("xapp-1".into()),
        bot_token: Secret::new("xoxb-1".into()),
        api_base: base,
    });
    (state, channel)
}

fn thread_target() -> ReplyTarget {
    let mut conversation = okbot_core::ConversationKey::room("slack:acme".parse().unwrap(), "C1");
    conversation.thread = Some("1.0".into());
    ReplyTarget {
        conversation,
        in_reply_to: None,
    }
}

#[tokio::test]
async fn chart_images_are_uploaded_before_the_prompt() {
    let (state, channel) = upload_channel(false).await;
    let sent = channel
        .prompt(&thread_target(), &chart_prompt())
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_str(), "1760000000.000300");

    let calls = state.calls.lock().unwrap().clone();
    let paths: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        paths,
        [
            "files.getUploadURLExternal",
            "upload/F1",
            "files.completeUploadExternal",
            "chat.postMessage"
        ]
    );
    let (_, auth, content_type, body) = &calls[0];
    assert_eq!(auth.as_deref(), Some("Bearer xoxb-1"));
    assert!(content_type.starts_with("application/x-www-form-urlencoded"));
    let form = String::from_utf8(body.clone()).unwrap();
    assert!(form.contains("filename=chart.png"), "{form}");
    assert!(form.contains("length=9"), "{form}");
    assert!(form.contains("alt_txt=Bar+chart%2C+Weekly+sales"), "{form}");

    let (_, auth, content_type, body) = &calls[1];
    assert_eq!(auth, &None, "the signed upload URL gets no token");
    assert!(content_type.starts_with("multipart/form-data"));
    assert!(body.windows(9).any(|w| w == b"\x89PNG fake"));

    let complete: Value = serde_json::from_slice(&calls[2].3).unwrap();
    assert_eq!(calls[2].1.as_deref(), Some("Bearer xoxb-1"));
    assert_eq!(
        complete,
        json!({"files": [{"id": "F1", "title": "Weekly sales"}], "channel_id": "C1",
               "thread_ts": "1.0"})
    );
    let post: Value = serde_json::from_slice(&calls[3].3).unwrap();
    assert_eq!(post["thread_ts"], "1.0");
    assert!(post["blocks"].is_array());

    // A prompt read back from the store has no bytes: nothing is uploaded again.
    let mut stored = chart_prompt();
    stored.cards[0].image.as_mut().unwrap().png.clear();
    channel.update_prompt(&sent, &stored).await.unwrap();
    let calls = state.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[4].0, "chat.update");
}

#[tokio::test]
async fn chart_upload_without_files_write_fails_the_prompt() {
    let (state, channel) = upload_channel(true).await;
    let error = channel
        .prompt(&thread_target(), &chart_prompt())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing_scope"), "{error}");
    let calls = state.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        1,
        "no prompt posted: the runtime falls back to text"
    );
}
