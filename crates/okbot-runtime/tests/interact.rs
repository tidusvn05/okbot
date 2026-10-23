//! Signed run tokens and prompts for `[[mcp]]` servers, end to end over loopback HTTP.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

mod support;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::HeaderMap;
use okbot_core::{
    CancellationToken, ChannelCaps, PromptAnswer, Renderer, Secret, SentRef, UserRef,
};
use okbot_runtime::identity;
use okbot_runtime::{ExtraMcp, HttpOptions, Runtime, RuntimeConfig};
use okbot_testkit::{FakeChannel, ManualClock, MemoryStore, PlainRenderer, inbound};
use serde_json::{Value, json};

const KEY: &str = "server-secret";

type Seen = Arc<Mutex<Vec<(HeaderMap, String)>>>;
type Calls = Arc<Mutex<Vec<support::Received>>>;

/// A stand-in MCP server: records tool calls and signed callbacks, answers them with `reply`.
async fn callback_server(reply: Value) -> (SocketAddr, Seen, Calls) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = seen.clone();
    let callbacks = axum::Router::new().route(
        "/answers",
        axum::routing::post(move |headers: HeaderMap, body: String| {
            let state = state.clone();
            let reply = reply.clone();
            async move {
                state.lock().unwrap().push((headers, body));
                axum::Json(reply)
            }
        }),
    );
    let tools = json!([{"name": "deploy", "inputSchema": {"type": "object"}}]);
    let (addr, calls) = support::mcp_server_with(tools, callbacks).await;
    (addr, seen, calls)
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not reached");
}

/// A runtime with one signed `[[mcp]]` server at `server`, admin `ana` and loopback HTTP.
fn start(
    server: SocketAddr,
) -> (
    Arc<FakeChannel>,
    Arc<support::ToolAgent>,
    okbot_runtime::RuntimeHandle,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let mut config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-interact-tests"));
    config.mcp = vec![ExtraMcp {
        server: okbot_core::McpServer {
            name: "ops".into(),
            url: format!("http://{server}/mcp"),
            bearer_token_env: "OPS_TOKEN".into(),
        },
        token: Secret::new(KEY.into()),
        admins_only: false,
        signed: true,
        rules: okbot_runtime::McpRules::default(),
        stdio: None,
        oauth: None,
    }];
    config
        .access
        .admins
        .insert(("fake:test".parse().unwrap(), okbot_core::UserId::new("ana")));
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let agent = Arc::new(support::ToolAgent::new(vec![support::Step::Call(
        "ops",
        "deploy",
        json!({}),
    )]));
    let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
    let runtime = Runtime::builder(config)
        .store(Arc::new(MemoryStore::new(Arc::new(ManualClock::default()))))
        .agent(agent.clone())
        .renderer(renderer)
        .channel(channel.clone())
        .http(HttpOptions::new(SocketAddr::from(([127, 0, 0, 1], 0))))
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });

    (channel, agent, handle, stop, task)
}

/// The server holds the key, so it can sign claims of its own (here for another room); okbot
/// never issued them.
fn minted_elsewhere(claims: &identity::Claims) -> String {
    identity::sign(
        KEY.as_bytes(),
        &identity::Claims {
            conversation: okbot_testkit::conversation("fake:test", "other-room"),
            jti: "minted".into(),
            ..claims.clone()
        },
    )
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, told in order
async fn signed_token_prompt_and_signed_callback() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, seen, calls) = callback_server(json!({"text": "**done**"})).await;
    let (channel, agent, handle, stop, task) = start(server);

    // Through the gateway, the server gets a signed token naming the requester; the agent holds
    // neither it nor the secret.
    channel
        .inject(inbound("fake:test", "room", "ana", "deploy"))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    let turn = &agent.turns()[0];
    let (_, given) = turn.env.iter().find(|(k, _)| k == "OPS_TOKEN").unwrap();
    let given = given.expose().clone();
    let token = calls.lock().unwrap()[0]
        .bearer
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    assert_ne!(token, KEY);
    assert_ne!(given, token);
    assert_ne!(given, KEY);
    let now = jiff::Timestamp::now().as_second();
    let claims = identity::verify(KEY.as_bytes(), &token, now).unwrap();
    assert_eq!(claims.user, "fake:test/ana");
    assert!(claims.admin);
    assert_eq!(claims.conversation.room, "room");
    let interact = claims.interact.clone().unwrap();

    // The server shows a prompt in that conversation with the token.
    let http = reqwest::Client::new();
    let post =
        |body: Value, bearer: String| http.post(&interact).bearer_auth(bearer).json(&body).send();
    let prompt = json!({
        "text": "Deploy **v2**?",
        "buttons": [
            {"id": "ok", "label": "Confirm", "style": "danger",
             "form": {"title": "Confirm", "fields": [{"id": "otp", "label": "Code"}]}},
            {"id": "no", "label": "Cancel", "style": "secondary"}
        ],
        "callback": format!("http://{server}/answers"),
        "answer_by": "admins"
    });
    let refused = post(prompt.clone(), KEY.into()).await.unwrap();
    assert_eq!(refused.status(), 401, "the plain secret is not a run token");
    let refused = post(prompt.clone(), minted_elsewhere(&claims))
        .await
        .unwrap();
    assert_eq!(refused.status(), 401, "only issued tokens");
    let mut elsewhere = prompt.clone();
    elsewhere["callback"] = json!("http://127.0.0.1:1/steal");
    assert_eq!(post(elsewhere, token.clone()).await.unwrap().status(), 400);
    let shown: Value = post(prompt, token.clone())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = shown["prompt"].as_str().unwrap().to_string();
    let shown = &channel.prompts()[0];
    assert_eq!(shown.id, id);
    assert_eq!(shown.allowed, [okbot_core::UserId::new("ana")]);

    // A typed answer cannot skip the form behind a button.
    channel
        .inject(inbound(
            "fake:test",
            "room",
            "ana",
            &format!("/answer {id} 1"),
        ))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    assert!(
        channel
            .sent_texts()
            .last()
            .unwrap()
            .contains("with its button"),
        "{:?}",
        channel.sent_texts()
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing reached the callback"
    );

    // An answer goes to the callback, signed, and its reply replaces the buttons.
    channel
        .answer(PromptAnswer {
            prompt: id.clone(),
            button: "ok".into(),
            values: BTreeMap::from([("otp".to_string(), Secret::new("123456".to_string()))]),
            user: UserRef {
                id: okbot_core::UserId::new("ana"),
                display_name: "Ana".into(),
                is_bot: false,
            },
            conversation: claims.conversation.clone(),
            sent: SentRef {
                target: okbot_core::ReplyTarget {
                    conversation: claims.conversation.clone(),
                    in_reply_to: None,
                },
                message_id: okbot_core::MessageId::new("prompt-1"),
            },
        })
        .await;
    eventually(|| channel.prompt_updates().iter().any(|(_, _, keep)| !keep)).await;
    let (headers, body) = seen.lock().unwrap()[0].clone();
    let timestamp: i64 = headers["x-okbot-timestamp"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        headers["x-okbot-signature"].to_str().unwrap(),
        identity::sign_body(KEY.as_bytes(), timestamp, body.as_bytes())
    );
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["button"], "ok");
    assert_eq!(body["values"]["otp"], "123456");
    assert_eq!(body["user"]["id"], "fake:test/ana");
    assert_eq!(body["user"]["admin"], true);
    let last = channel.prompt_updates().last().unwrap().clone();
    assert!(last.1.contains("done") && !last.2, "{last:?}");
    assert!(
        !channel.sent_texts().iter().any(|t| t.contains("123456")),
        "form values never reach the chat"
    );
    assert!(!agent.turns().iter().any(|t| t.prompt.contains("123456")));
    assert_eq!(agent.results(), [json!("deploy ok")]);

    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one story, told in order
async fn a_callback_reply_can_show_the_next_step() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // A form's values come back as a plan to confirm, under the same prompt id.
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let state = seen.clone();
    let callbacks = axum::Router::new().route(
        "/answers",
        axum::routing::post(move |headers: HeaderMap, body: String| {
            let state = state.clone();
            async move {
                let button = serde_json::from_str::<Value>(&body).unwrap()["button"].clone();
                state.lock().unwrap().push((headers, body));
                axum::Json(if button == "fill" {
                    json!({"text": "📝 filled", "next": {"text": "Plan: **v2**", "buttons": [
                        {"id": "go", "label": "Confirm", "style": "danger"}]}})
                } else {
                    json!({"text": "done"})
                })
            }
        }),
    );
    let tools = json!([{"name": "deploy", "inputSchema": {"type": "object"}}]);
    let (server, calls) = support::mcp_server_with(tools, callbacks).await;
    let (channel, _agent, handle, stop, task) = start(server);
    channel
        .inject(inbound("fake:test", "room", "ana", "deploy"))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    let token = calls.lock().unwrap()[0]
        .bearer
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    let claims =
        identity::verify(KEY.as_bytes(), &token, jiff::Timestamp::now().as_second()).unwrap();
    let prompt = json!({
        "text": "Deploy what?",
        "buttons": [{"id": "fill", "label": "Enter",
                     "form": {"title": "Deploy", "fields": [{"id": "f1", "label": "Version"}]}}],
        "callback": format!("http://{server}/answers"),
        "answer_by": "admins"
    });
    let shown: Value = reqwest::Client::new()
        .post(claims.interact.clone().unwrap())
        .bearer_auth(&token)
        .json(&prompt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = shown["prompt"].as_str().unwrap().to_string();
    let press = |button: &str, values: BTreeMap<String, Secret<String>>| PromptAnswer {
        prompt: id.clone(),
        button: button.into(),
        values,
        user: UserRef {
            id: okbot_core::UserId::new("ana"),
            display_name: "Ana".into(),
            is_bot: false,
        },
        conversation: claims.conversation.clone(),
        sent: SentRef {
            target: okbot_core::ReplyTarget {
                conversation: claims.conversation.clone(),
                in_reply_to: None,
            },
            message_id: okbot_core::MessageId::new("prompt-1"),
        },
    };

    channel
        .answer(press(
            "fill",
            BTreeMap::from([("f1".to_string(), Secret::new("v2".to_string()))]),
        ))
        .await;
    eventually(|| channel.prompts().len() == 2).await;
    let next = &channel.prompts()[1];
    assert_eq!(next.id, id, "the same prompt goes on");
    assert!(next.text.contains("Plan"), "{next:?}");
    assert_eq!(next.allowed, [okbot_core::UserId::new("ana")]);
    let updates = channel.prompt_updates();
    assert!(
        updates.iter().any(|u| u.1.contains("filled") && !u.2),
        "the form's message is closed: {updates:?}"
    );

    // The new button answers; the old one no longer does.
    channel.answer(press("fill", BTreeMap::new())).await;
    channel.answer(press("go", BTreeMap::new())).await;
    eventually(|| seen.lock().unwrap().len() == 2).await;
    let body: Value = serde_json::from_str(&seen.lock().unwrap()[1].1).unwrap();
    assert_eq!(body["button"], "go");
    eventually(|| {
        channel
            .prompt_updates()
            .last()
            .is_some_and(|u| u.1.contains("done"))
    })
    .await;

    stop.cancel();
    task.await.unwrap();
}
