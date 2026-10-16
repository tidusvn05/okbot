//! Signed run tokens and prompts for `[[mcp]]` servers, end to end over loopback HTTP.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::HeaderMap;
use okbot_core::{
    AgentCaps, CancellationToken, ChannelCaps, PromptAnswer, Renderer, Secret, SentRef, ToolPolicy,
    UserRef,
};
use okbot_runtime::identity;
use okbot_runtime::{ExtraMcp, HttpOptions, Runtime, RuntimeConfig};
use okbot_testkit::{FakeAgent, FakeChannel, ManualClock, MemoryStore, PlainRenderer, inbound};
use serde_json::{Value, json};

const KEY: &str = "server-secret";

/// A stand-in MCP server: records signed callbacks and answers with `reply`.
async fn callback_server(reply: Value) -> (SocketAddr, Arc<Mutex<Vec<(HeaderMap, String)>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = seen.clone();
    let app = axum::Router::new().route(
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, seen)
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
    Arc<FakeAgent>,
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
    }];
    config
        .access
        .admins
        .insert(("fake:test".parse().unwrap(), okbot_core::UserId::new("ana")));
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let agent = Arc::new(
        FakeAgent::echo("fake")
            .with_caps(AgentCaps {
                mcp: true,
                ..AgentCaps::default()
            })
            .enforcing(&[ToolPolicy::Web, ToolPolicy::McpOnly]),
    );
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

#[tokio::test]
async fn signed_token_prompt_and_signed_callback() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, seen) = callback_server(json!({"text": "**done**"})).await;
    let (channel, agent, handle, stop, task) = start(server);

    // The agent gets a signed token naming the requester, not the secret.
    channel
        .inject(inbound("fake:test", "room", "ana", "deploy"))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    let turn = &agent.turns()[0];
    let (_, token) = turn.env.iter().find(|(k, _)| k == "OPS_TOKEN").unwrap();
    let token = token.expose().clone();
    assert_ne!(token, KEY);
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

    stop.cancel();
    task.await.unwrap();
}
