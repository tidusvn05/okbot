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

/// A stand-in MCP server with two tools, `status {company, deep?: boolean}` (read-only) and
/// `restart {company}`, answering over an event stream like rmcp does. Records the verified
/// claims and arguments of each call. `status board` answers with a view (ADR 0017).
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
                    "required": ["company"]},
                    "annotations": {"readOnlyHint": true}},
                    {"name": "restart", "inputSchema": {"type": "object",
                        "properties": {"company": {"type": "string"}}}}]}),
                "tools/call" => {
                    let args = msg["params"]["arguments"].clone();
                    state.lock().unwrap().push((claims, args.clone()));
                    if msg["params"]["name"] == "restart" {
                        json!({"content": [{"type": "text", "text": format!("restarted {}", args["company"].as_str().unwrap())}]})
                    } else if args["company"] == "board" {
                        let view = json!({
                            "cards": [{"title": "freach", "fields": [{"label": "Pods", "value": "3/3"}]}],
                            "buttons": [
                                {"label": "Refresh", "command": "/status freach"},
                                {"label": "Restart", "command": "/restart freach", "style": "danger"}
                            ]
                        });
                        json!({"content": [{"type": "text", "text": view.to_string()}],
                               "structuredContent": {"okbot_view": view}})
                    } else if args["company"] == "textview" {
                        // A text is never read as a view (it may repeat a third party's words).
                        json!({"content": [{"type": "text",
                            "text": "{\"okbot_view\": {\"text\": \"x\"}}"}]})
                    } else if args["company"] == "crossview" {
                        // A button for another server's command: the view is refused.
                        json!({"content": [{"type": "text", "text": "cross text"}],
                               "structuredContent": {"okbot_view": {"buttons": [
                                   {"label": "Acknowledge", "command": "/elsewhere prod"}]}}})
                    } else if args["company"] == "badview" {
                        json!({"content": [],
                               "structuredContent": {"okbot_view": {"cards": "nope"}}})
                    } else if args["company"] == "broken" {
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

type Started = (
    Arc<FakeChannel>,
    Arc<FakeAgent>,
    okbot_runtime::RuntimeHandle,
    CancellationToken,
);

fn start(server: SocketAddr) -> Started {
    start_with(server, ChannelCaps::basic(), None, |_| {})
}

fn start_with(
    server: SocketAddr,
    caps: ChannelCaps,
    clock: Option<Arc<ManualClock>>,
    configure: impl FnOnce(&mut RuntimeConfig),
) -> Started {
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
        rules: okbot_runtime::McpRules::default(),
        stdio: None,
        oauth: None,
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
        roles: Vec::new(),
    };
    config.commands = vec![
        command("status", &["company", "deep?"], false),
        command("deploy", &["company"], true),
        command("missing", &[], false),
        command("restart", &["company"], false),
        command("elsewhere", &["company"], false),
    ];
    config.commands[2].tool = "nope".into();
    config.commands[3].tool = "restart".into();
    config.commands[4].mcp = "other".into();
    config
        .access
        .admins
        .insert(("fake:test".parse().unwrap(), okbot_core::UserId::new("ana")));
    configure(&mut config);
    let channel = Arc::new(FakeChannel::new("fake:test", caps));
    let agent = Arc::new(
        FakeAgent::echo("fake")
            .with_caps(AgentCaps {
                mcp: true,
                ..AgentCaps::default()
            })
            .enforcing(&[ToolPolicy::Web, ToolPolicy::McpOnly]),
    );
    let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
    let mut builder = Runtime::builder(config)
        .store(Arc::new(MemoryStore::new(Arc::new(ManualClock::default()))))
        .agent(agent.clone())
        .renderer(renderer)
        .channel(channel.clone());
    if let Some(clock) = clock {
        builder = builder.clock(clock);
    }
    let runtime = builder.build().unwrap();
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

async fn press(channel: &FakeChannel, user: &str, prompt: &str, button: &str) {
    let conversation = okbot_testkit::conversation("fake:test", "room");
    channel
        .answer(okbot_core::PromptAnswer {
            prompt: prompt.into(),
            button: button.into(),
            values: std::collections::BTreeMap::new(),
            user: okbot_core::UserRef {
                id: okbot_core::UserId::new(user),
                display_name: user.into(),
                is_bot: false,
            },
            conversation: conversation.clone(),
            sent: okbot_core::SentRef {
                target: okbot_core::ReplyTarget {
                    conversation,
                    in_reply_to: None,
                },
                message_id: okbot_core::MessageId::new("p-1"),
            },
        })
        .await;
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn results_show_views_whose_buttons_run_commands() {
    let (server, calls) = mcp_server().await;
    let caps = ChannelCaps {
        ui: okbot_core::UiCaps::all(),
        ..ChannelCaps::basic()
    };
    // Text replies (the default): command views do not need the agent's interactive mode.
    let (channel, agent, handle, stop) = start_with(server, caps, None, |c| {
        c.mcp[0].rules.confirm = okbot_runtime::config::Confirm::Destructive;
    });
    let calls_now = || calls.lock().unwrap().len();

    assert_eq!(say(&channel, &handle, "bob", "/status board").await, "");
    let board = channel.prompts()[0].clone();
    assert_eq!(board.cards[0].title.as_deref(), Some("freach"));
    assert!(board.cards[0].text.contains("**Pods:** 3/3"));
    let ids: Vec<&str> = board.buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2"]);
    // The labels cannot hide what the buttons run.
    assert!(
        board
            .text
            .contains("Buttons run: `/status freach` · `/restart freach`"),
        "{}",
        board.text
    );

    // Refresh runs `/status freach` as the presser; the tool only reads, so no approval.
    press(&channel, "eve", &board.id, "c1").await;
    eventually(|| calls_now() == 2).await;
    let call = calls.lock().unwrap()[1].clone();
    assert_eq!(call.1, json!({"company": "freach"}));
    assert_eq!(call.0.user, "fake:test/eve");
    assert_eq!(call.0.command.as_deref(), Some("status"));
    eventually(|| channel.sent_texts().iter().any(|t| t == "**freach** ok")).await;
    assert!(
        channel
            .prompt_updates()
            .iter()
            .any(|(_, text, keep)| text.contains("→ ✅ eve: Refresh") && !keep),
        "{:?}",
        channel.prompt_updates()
    );

    // Restart may change things: the press asks the presser first, nobody else may answer.
    say(&channel, &handle, "bob", "/status board").await;
    let board = channel.prompts().last().unwrap().clone();
    press(&channel, "bob", &board.id, "c2").await;
    eventually(|| channel.prompts().len() == 3).await;
    let ask = channel.prompts()[2].clone();
    assert!(ask.text.contains("/restart freach"), "{}", ask.text);
    assert_eq!(ask.allowed, [okbot_core::UserId::new("bob")]);
    let before = calls_now();
    press(&channel, "eve", &ask.id, "c1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(calls_now(), before, "not eve's to approve");
    press(&channel, "bob", &ask.id, "c1").await;
    eventually(|| calls_now() == before + 1).await;
    eventually(|| channel.sent_texts().iter().any(|t| t == "restarted freach")).await;

    // Typed, the same command runs at once (ADR 0011).
    assert_eq!(
        say(&channel, &handle, "bob", "/restart acme").await,
        "restarted acme"
    );
    // A view okbot cannot show is reported, not echoed.
    let bad = say(&channel, &handle, "bob", "/status badview").await;
    assert_eq!(
        bad,
        "⚠️ `/status` answered with a view that cannot be shown."
    );
    // A button for another server's command makes the view invalid: the text is shown.
    assert_eq!(
        say(&channel, &handle, "bob", "/status crossview").await,
        "cross text"
    );
    // A text that looks like a view is only text.
    let prompts = channel.prompts().len();
    assert_eq!(
        say(&channel, &handle, "bob", "/status textview").await,
        "{\"okbot_view\": {\"text\": \"x\"}}"
    );
    assert_eq!(channel.prompts().len(), prompts);
    assert!(agent.turns().is_empty(), "the agent never ran");
    stop.cancel();
}

#[tokio::test]
async fn an_admin_approves_a_press_that_runs_as_the_presser() {
    let (server, calls) = mcp_server().await;
    let caps = ChannelCaps {
        ui: okbot_core::UiCaps::all(),
        ..ChannelCaps::basic()
    };
    let clock = Arc::new(ManualClock::default());
    let (channel, _agent, handle, stop) = start_with(server, caps, Some(clock.clone()), |c| {
        c.mcp[0].rules.confirm = okbot_runtime::config::Confirm::Destructive;
        c.mcp[0].rules.confirm_by = okbot_runtime::config::ConfirmBy::Admins;
    });
    let calls_now = || calls.lock().unwrap().len();
    let ask_for_restart = async || {
        say(&channel, &handle, "bob", "/status board").await;
        let board = channel.prompts().last().unwrap().clone();
        let before = channel.prompts().len();
        press(&channel, "bob", &board.id, "c2").await;
        eventually(|| channel.prompts().len() == before + 1).await;
        channel.prompts().last().unwrap().clone()
    };

    let ask = ask_for_restart().await;
    assert!(
        ask.text
            .contains("bob pressed a button that runs `/restart freach`")
            && ask.text.contains("as bob"),
        "{}",
        ask.text
    );
    assert_eq!(ask.allowed, [okbot_core::UserId::new("ana")]);
    // Bob pressed, but only an admin approves.
    let before = calls_now();
    press(&channel, "bob", &ask.id, "c1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(calls_now(), before);
    // Ana's approval runs it as Bob, with Bob's rights.
    press(&channel, "ana", &ask.id, "c1").await;
    eventually(|| calls_now() == before + 1).await;
    let (claims, args) = calls.lock().unwrap().last().unwrap().clone();
    assert_eq!(args, json!({"company": "freach"}));
    assert_eq!(claims.user, "fake:test/bob");
    assert!(!claims.admin);

    // An approval waits 10 minutes, not a week.
    let ask = ask_for_restart().await;
    clock.advance(Duration::from_secs(11 * 60));
    let before = calls_now();
    press(&channel, "ana", &ask.id, "c1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(calls_now(), before, "an expired approval runs nothing");

    // An admin removed while the approval is open can no longer approve it (ADR 0018).
    let ask = ask_for_restart().await;
    handle.reload_access(okbot_runtime::AccessLists::default());
    let before = calls_now();
    press(&channel, "ana", &ask.id, "c1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(calls_now(), before, "a revoked admin approves nothing");
    stop.cancel();
}
