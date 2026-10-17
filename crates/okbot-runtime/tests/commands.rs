//! `[[command]]`: chat commands that call an `[[mcp]]` tool directly, end to end over loopback HTTP.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use okbot_core::{AgentCaps, CancellationToken, ChannelCaps, Renderer, Secret, ToolPolicy};
use okbot_runtime::identity;
use okbot_runtime::{CommandArg, ExtraMcp, McpCommand, Runtime, RuntimeConfig};
use okbot_testkit::{FakeAgent, FakeChannel, ManualClock, MemoryStore, PlainRenderer, inbound};
use serde_json::{Value, json};

const KEY: &str = "server-secret";

type Calls = Arc<Mutex<Vec<(identity::Claims, Value)>>>;

/// A stand-in MCP server with one tool, `status {company, deep?: boolean}`, answering over an
/// event stream like rmcp does. Records the verified claims and arguments of each call.
async fn mcp_server() -> (SocketAddr, Calls) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let state = calls.clone();
    let handler = move |headers: HeaderMap, body: String| {
        let state = state.clone();
        async move {
            let bearer = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default();
            let Some(claims) = identity::verify(KEY.as_bytes(), bearer, 0) else {
                return StatusCode::UNAUTHORIZED.into_response();
            };
            let msg: Value = serde_json::from_str(&body).unwrap();
            let id = msg["id"].clone();
            let result = match msg["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                                       "serverInfo": {"name": "fake", "version": "0"}})
                }
                "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
                "tools/list" => json!({"tools": [{"name": "status", "inputSchema": {
                    "type": "object",
                    "properties": {"company": {"type": "string"}, "deep": {"type": "boolean"}},
                    "required": ["company"]}}]}),
                "tools/call" => {
                    let args = msg["params"]["arguments"].clone();
                    state.lock().unwrap().push((claims, args.clone()));
                    if args["company"] == "broken" {
                        json!({"content": [{"type": "text", "text": "no such company"}], "isError": true})
                    } else {
                        json!({"content": [{"type": "text", "text": format!("**{}** ok", args["company"].as_str().unwrap())}]})
                    }
                }
                other => panic!("unexpected {other}"),
            };
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
            (
                [
                    (header::CONTENT_TYPE, "text/event-stream"),
                    (header::HeaderName::from_static("mcp-session-id"), "s1"),
                ],
                format!("event: message\ndata: {reply}\n\n"),
            )
                .into_response()
        }
    };
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(handler).delete(|| async { StatusCode::OK }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, calls)
}

fn start(
    server: SocketAddr,
) -> (
    Arc<FakeChannel>,
    Arc<FakeAgent>,
    okbot_runtime::RuntimeHandle,
    CancellationToken,
) {
    let mut config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-command-tests"));
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
    let command = |name: &str, args: &[&str], admins_only| McpCommand {
        name: name.into(),
        description: "Status of a company".into(),
        descriptions: vec![("ja".into(), "会社の状態".into())],
        mcp: "ops".into(),
        tool: "status".into(),
        args: args.iter().map(|a| CommandArg::parse(a)).collect(),
        with: serde_json::Map::new(),
        admins_only,
    };
    config.commands = vec![
        command("status", &["company", "deep?"], false),
        command("deploy", &["company"], true),
        command("missing", &[], false),
    ];
    config.commands[2].tool = "nope".into();
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
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });
    (channel, agent, handle, stop)
}

async fn say(
    channel: &FakeChannel,
    handle: &okbot_runtime::RuntimeHandle,
    user: &str,
    text: &str,
) -> String {
    let before = channel.sent_texts().len();
    channel
        .inject(inbound("fake:test", "room", user, text))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    channel.sent_texts()[before..].join("\n")
}

#[tokio::test]
async fn commands_call_the_tool_without_the_agent() {
    let (server, calls) = mcp_server().await;
    let (channel, agent, handle, stop) = start(server);

    assert_eq!(
        say(&channel, &handle, "bob", "/status freach").await,
        "**freach** ok"
    );
    assert_eq!(
        say(&channel, &handle, "bob", "/Status@okbot acme yes").await,
        "**acme** ok"
    );
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls[0].1, json!({"company": "freach"}));
    assert_eq!(
        calls[1].1,
        json!({"company": "acme", "deep": true}),
        "typed from the schema"
    );
    let (claims, _) = &calls[0];
    assert_eq!(claims.user, "fake:test/bob");
    assert_eq!(claims.command.as_deref(), Some("status"));
    assert!(!claims.admin);
    assert!(agent.turns().is_empty(), "the agent never ran");

    // Tool errors, usage, permissions, a tool the server does not offer.
    assert!(
        say(&channel, &handle, "bob", "/status broken")
            .await
            .starts_with("⚠️ no such company")
    );
    let usage = say(&channel, &handle, "bob", "/status").await;
    assert!(
        usage.contains("Missing `company`") && usage.contains("/status <company> [deep]"),
        "{usage}"
    );
    assert!(
        say(&channel, &handle, "bob", "/status a deep=maybe")
            .await
            .contains("boolean")
    );
    assert!(
        say(&channel, &handle, "bob", "/deploy acme")
            .await
            .contains("Only administrators")
    );
    assert_eq!(
        say(&channel, &handle, "ana", "/deploy acme").await,
        "**acme** ok"
    );
    assert!(
        say(&channel, &handle, "bob", "/missing")
            .await
            .contains("not available to you")
    );

    // `/help` lists what the person may run.
    let help = say(&channel, &handle, "bob", "/help").await;
    assert!(
        help.contains("`/status <company> [deep]` — Status of a company"),
        "{help}"
    );
    assert!(!help.contains("/deploy"));
    assert!(
        say(&channel, &handle, "ana", "/help")
            .await
            .contains("/deploy")
    );
    // The person's reported language picks the description.
    let mut ja = inbound("fake:test", "room", "bob", "/help");
    ja.locale = Some("ja-JP".into());
    let before = channel.sent_texts().len();
    channel.inject(ja).await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    assert!(
        channel.sent_texts()[before..]
            .join("\n")
            .contains("`/status <company> [deep]` — 会社の状態")
    );

    // Anything else still goes to the agent.
    say(&channel, &handle, "bob", "/unknown thing").await;
    assert_eq!(agent.turns().len(), 1);
    stop.cancel();
}

#[tokio::test]
async fn an_unreachable_server_is_reported() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let (channel, _agent, handle, stop) = start(addr);
    let reply = say(&channel, &handle, "bob", "/status freach").await;
    assert!(
        reply.contains("`/status` failed") && reply.contains("cannot be reached"),
        "{reply}"
    );
    stop.cancel();
}
