//! The MCP gateway end to end (ADR 0011): roles, rooms, tool lists, confirmation, audit, limits
//! and grants, with an agent that speaks MCP over loopback HTTP.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use okbot_core::{
    CancellationToken, ChannelCaps, ConversationStore, PromptAnswer, Renderer, Secret,
    ToolDecision, UserRef,
};
use okbot_runtime::{Confirm, ExtraMcp, McpRules, Member, Restrict, Roles, Runtime, RuntimeConfig};
use okbot_testkit::{FakeChannel, MemoryStore, PlainRenderer, inbound};
use serde_json::{Value, json};
use support::{Step, ToolAgent};

const KEY: &str = "ops-secret";

fn tools() -> Value {
    json!([
        {"name": "status", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}},
        {"name": "logs_tail", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}},
        {"name": "deploy", "inputSchema": {"type": "object"}, "annotations": {"destructiveHint": true}},
        {"name": "drop_db", "inputSchema": {"type": "object"}}
    ])
}

struct Bot {
    channel: Arc<FakeChannel>,
    agent: Arc<ToolAgent>,
    store: Arc<MemoryStore>,
    handle: okbot_runtime::RuntimeHandle,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Bot {
    fn start(
        server: SocketAddr,
        steps: Vec<Step>,
        channel: FakeChannel,
        configure: impl FnOnce(&mut RuntimeConfig),
    ) -> Self {
        let mut config =
            RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-gateway-tests"));
        config.mcp = vec![ExtraMcp {
            server: okbot_core::McpServer {
                name: "ops".into(),
                url: format!("http://{server}/mcp"),
                bearer_token_env: "OPS_TOKEN".into(),
            },
            token: Secret::new(KEY.into()),
            admins_only: false,
            signed: false,
            rules: McpRules::default(),
            stdio: None,
            oauth: None,
        }];
        config
            .access
            .admins
            .insert(("fake:test".parse().unwrap(), okbot_core::UserId::new("ana")));
        configure(&mut config);
        let channel = Arc::new(channel);
        let agent = Arc::new(ToolAgent::new(steps));
        let store = Arc::new(MemoryStore::new(Arc::new(okbot_core::SystemClock)));
        let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
        let runtime = Runtime::builder(config)
            .store(store.clone())
            .agent(agent.clone())
            .renderer(renderer)
            .channel(channel.clone())
            .build()
            .unwrap();
        let handle = runtime.handle();
        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let stop = stop.clone();
            async move { runtime.run(stop).await.unwrap() }
        });
        Self {
            channel,
            agent,
            store,
            handle,
            stop,
            task,
        }
    }

    async fn say(&self, room: &str, user: &str, text: &str) {
        self.channel
            .inject(inbound("fake:test", room, user, text))
            .await;
    }

    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(20), self.handle.wait_idle())
            .await
            .unwrap();
    }

    async fn finish(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}

fn channel() -> FakeChannel {
    FakeChannel::new("fake:test", ChannelCaps::basic())
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

fn roles(pairs: &[(&str, &str)]) -> Roles {
    let mut roles = Roles::default();
    for (role, user) in pairs {
        roles.add(
            role,
            Member::Account("fake:test".parse().unwrap(), okbot_core::UserId::new(*user)),
        );
    }
    roles
}

#[tokio::test]
async fn tools_are_filtered_refused_and_audited() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, calls) = support::mcp_server(tools()).await;
    let steps = vec![
        Step::List("ops"),
        Step::Call("ops", "status", json!({})),
        Step::Call("ops", "deploy", json!({"env": "prod"})),
        Step::Call("ops", "drop_db", json!({})),
    ];
    let bot = Bot::start(server, steps, channel(), |c| {
        c.roles = roles(&[("ops", "olga")]);
        c.mcp[0].rules = McpRules {
            tools: vec!["status".into(), "logs_*".into(), "deploy".into()],
            restrict: vec![Restrict {
                tools: vec!["deploy".into()],
                roles: vec!["ops".into()],
            }],
            ..McpRules::default()
        };
    });

    // A member sees and calls only what everyone may.
    bot.say("room", "bob", "go").await;
    bot.idle().await;
    let refused = json!("This tool is not available to this person here.");
    assert_eq!(
        bot.agent.results(),
        [
            json!(["status", "logs_tail"]),
            json!("status ok"),
            refused.clone(),
            refused.clone()
        ]
    );
    // The `ops` role adds `deploy`; `drop_db` is never offered.
    bot.say("room", "olga", "go").await;
    bot.idle().await;
    assert_eq!(
        bot.agent.results()[4..],
        [
            json!(["status", "logs_tail", "deploy"]),
            json!("status ok"),
            json!("deploy ok"),
            refused
        ]
    );

    // The server got the secret; the agent never had it.
    let received = calls.lock().unwrap().clone();
    let tools: Vec<&str> = received.iter().map(|c| c.tool.as_str()).collect();
    assert_eq!(tools, ["status", "status", "deploy"]);
    assert!(received.iter().all(|c| c.bearer == format!("Bearer {KEY}")));
    for turn in bot.agent.turns() {
        let ops = turn.mcp.iter().find(|s| s.name == "ops").unwrap();
        assert!(ops.url.starts_with("http://127.0.0.1:") && ops.url.ends_with("/mcp/ops"));
        assert!(turn.env.iter().all(|(_, v)| v.expose() != KEY));
    }

    let audit = bot.store.recorded_tool_calls();
    let summary: Vec<(&str, &str, ToolDecision)> = audit
        .iter()
        .map(|c| (c.person.as_str(), c.tool.as_str(), c.decision))
        .collect();
    assert_eq!(
        summary,
        [
            ("fake:test/bob", "status", ToolDecision::Allowed),
            ("fake:test/bob", "deploy", ToolDecision::Denied),
            ("fake:test/bob", "drop_db", ToolDecision::Denied),
            ("fake:test/olga", "status", ToolDecision::Allowed),
            ("fake:test/olga", "deploy", ToolDecision::Allowed),
            ("fake:test/olga", "drop_db", ToolDecision::Denied),
        ]
    );
    assert!(
        audit
            .iter()
            .all(|c| c.run_id.is_some() && c.error.is_none())
    );
    bot.finish().await;
}

#[tokio::test]
async fn servers_by_room_and_role_and_tokens_end_with_the_turn() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, _) = support::mcp_server(tools()).await;
    let bot = Bot::start(server, vec![Step::List("ops")], channel(), |c| {
        c.roles = roles(&[("ops", "olga")]);
        c.mcp[0].rules.roles = vec!["ops".into()];
        c.mcp[0].rules.rooms = vec![("fake:test".parse().unwrap(), "ops-room".into())];
    });
    bot.say("ops-room", "bob", "hi").await;
    bot.say("general", "olga", "hi").await;
    bot.say("ops-room", "olga", "hi").await;
    bot.idle().await;
    let results = bot.agent.results();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == json!("not offered"))
            .count(),
        2,
        "{results:?}"
    );
    assert!(results.contains(&json!(["status", "logs_tail", "deploy", "drop_db"])));

    // The token of a finished turn opens nothing.
    let turn = bot
        .agent
        .turns()
        .into_iter()
        .find(|t| t.mcp.iter().any(|s| s.name == "ops"))
        .unwrap();
    let url = &turn.mcp.iter().find(|s| s.name == "ops").unwrap().url;
    let token = turn
        .env
        .iter()
        .find(|(k, _)| k == "OPS_TOKEN")
        .unwrap()
        .1
        .expose()
        .clone();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    assert_eq!(support::rpc(url, &token, &list).await, Err(401));
    assert_eq!(support::rpc(url, "made-up", &list).await, Err(401));
    bot.finish().await;
}

fn answer(prompt: &okbot_core::Prompt, user: &str, button: &str, room: &str) -> PromptAnswer {
    let conversation = okbot_testkit::conversation("fake:test", room);
    PromptAnswer {
        prompt: prompt.id.clone(),
        button: button.into(),
        values: std::collections::BTreeMap::new(),
        user: UserRef {
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
            message_id: okbot_core::MessageId::new("prompt-1"),
        },
    }
}

#[tokio::test]
async fn destructive_calls_wait_for_the_requester() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, calls) = support::mcp_server(tools()).await;
    let steps = vec![
        Step::Call("ops", "status", json!({})),
        Step::Call("ops", "deploy", json!({"env": "prod"})),
    ];
    let bot = Bot::start(server, steps, channel(), |c| {
        c.mcp[0].rules.confirm = Confirm::Destructive;
    });

    // Allowed: only the requester's press counts.
    bot.say("room", "bob", "deploy").await;
    eventually(|| !bot.channel.prompts().is_empty()).await;
    let prompt = bot.channel.prompts()[0].clone();
    assert_eq!(prompt.allowed, [okbot_core::UserId::new("bob")]);
    assert!(
        prompt.text.contains("deploy") && prompt.text.contains("prod"),
        "{}",
        prompt.text
    );
    bot.channel
        .answer(answer(&prompt, "eve", "allow", "room"))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "only status ran (read-only)"
    );
    bot.channel
        .answer(answer(&prompt, "bob", "allow", "room"))
        .await;
    bot.idle().await;
    assert_eq!(
        bot.agent.results(),
        [json!("status ok"), json!("deploy ok")]
    );
    let updates = bot.channel.prompt_updates();
    assert!(
        updates.last().unwrap().1.contains("Allowed by bob"),
        "{updates:?}"
    );

    // Declined: the call never reaches the server.
    bot.say("room", "bob", "deploy").await;
    eventually(|| bot.channel.prompts().len() == 2).await;
    let prompt = bot.channel.prompts()[1].clone();
    bot.channel
        .answer(answer(&prompt, "bob", "deny", "room"))
        .await;
    bot.idle().await;
    let last = bot.agent.results().last().unwrap().clone();
    assert!(last.as_str().unwrap().contains("did not approve"), "{last}");
    let deploys = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.tool == "deploy")
        .count();
    assert_eq!(deploys, 1);
    let decisions: Vec<ToolDecision> = bot
        .store
        .recorded_tool_calls()
        .iter()
        .filter(|c| c.tool == "deploy")
        .map(|c| c.decision)
        .collect();
    assert_eq!(decisions, [ToolDecision::Allowed, ToolDecision::Declined]);
    bot.finish().await;
}

#[tokio::test]
async fn without_buttons_approval_is_a_command() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, calls) = support::mcp_server(tools()).await;
    let steps = vec![Step::Call("ops", "drop_db", json!({}))];
    let bot = Bot::start(server, steps, channel().without_buttons(), |c| {
        c.mcp[0].rules.confirm = Confirm::Destructive;
    });
    bot.say("room", "bob", "drop it").await;
    eventually(|| {
        bot.channel
            .sent_texts()
            .iter()
            .any(|t| t.contains("/approve"))
    })
    .await;
    let asked = bot
        .channel
        .sent_texts()
        .into_iter()
        .find(|t| t.contains("/approve"))
        .unwrap();
    let code = asked
        .split("`/approve ")
        .nth(1)
        .unwrap()
        .split('`')
        .next()
        .unwrap()
        .to_string();
    // Someone else cannot approve; the requester can, while the turn waits in the queue.
    bot.say("room", "eve", &format!("/approve {code}")).await;
    eventually(|| {
        bot.channel
            .sent_texts()
            .iter()
            .any(|t| t.contains("not yours"))
    })
    .await;
    bot.say("room", "bob", "/approve nope").await;
    eventually(|| {
        bot.channel
            .sent_texts()
            .iter()
            .any(|t| t.contains("no such question"))
    })
    .await;
    bot.say("room", "bob", &format!("/approve {code}")).await;
    bot.idle().await;
    assert_eq!(bot.agent.results(), [json!("drop_db ok")]);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        bot.channel
            .sent_texts()
            .iter()
            .any(|t| t.contains("Allowed by bob"))
    );
    assert!(
        !bot.agent.turns()[0].prompt.contains("/approve"),
        "the approval is not part of the conversation"
    );
    bot.finish().await;
}

#[tokio::test]
async fn unanswered_approval_ends_with_the_turn() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, calls) = support::mcp_server(tools()).await;
    let steps = vec![Step::Call("ops", "deploy", json!({}))];
    let bot = Bot::start(server, steps, channel(), |c| {
        c.mcp[0].rules.confirm = Confirm::Always;
        c.agent_timeout = Duration::from_millis(600);
    });
    bot.say("room", "bob", "deploy").await;
    bot.idle().await;
    assert!(calls.lock().unwrap().is_empty());
    let updates = bot.channel.prompt_updates();
    assert!(
        updates.iter().any(|u| u.1.contains("Not approved in time")),
        "{updates:?}"
    );
    bot.finish().await;
}

#[tokio::test]
async fn daily_limits_spare_admins() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, _) = support::mcp_server(tools()).await;
    let bot = Bot::start(server, vec![], channel(), |c| {
        c.limits.turns_per_day = 2;
    });
    for _ in 0..3 {
        bot.say("room", "bob", "hi").await;
        bot.idle().await;
    }
    for _ in 0..3 {
        bot.say("room", "ana", "hi").await;
        bot.idle().await;
    }
    assert_eq!(bot.agent.turns().len(), 5, "bob's third turn did not run");
    let limited = bot
        .channel
        .sent_texts()
        .iter()
        .filter(|t| t.contains("today's limit"))
        .count();
    assert_eq!(limited, 1);
    let usage = bot
        .store
        .usage_since(
            "fake:test/bob",
            jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1),
        )
        .await
        .unwrap();
    assert_eq!(usage.turns, 2);
    bot.finish().await;
}

#[tokio::test]
async fn tokens_limit() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, _) = support::mcp_server(tools()).await;
    // Each fake turn reports 110 tokens.
    let bot = Bot::start(server, vec![], channel(), |c| {
        c.limits.tokens_per_day = 200;
    });
    for _ in 0..3 {
        bot.say("room", "bob", "hi").await;
        bot.idle().await;
    }
    assert_eq!(bot.agent.turns().len(), 2);
    bot.finish().await;
}

#[tokio::test]
async fn admins_grant_roles_for_a_while() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, _) = support::mcp_server(tools()).await;
    let bot = Bot::start(server, vec![Step::List("ops")], channel(), |c| {
        c.roles = roles(&[("ops", "olga")]);
        c.mcp[0].rules.roles = vec!["ops".into()];
    });
    let last = |bot: &Bot| bot.channel.sent_texts().last().unwrap().clone();

    bot.say("room", "bob", "/grant ops fake:test/bob 2h").await;
    bot.idle().await;
    assert!(last(&bot).contains("Only administrators"), "{}", last(&bot));
    bot.say("room", "ana", "/grant admins fake:test/bob 2h")
        .await;
    bot.idle().await;
    assert!(last(&bot).contains("cannot be granted"), "{}", last(&bot));
    bot.say("room", "ana", "/grant ops fake:test/bob forever")
        .await;
    bot.idle().await;
    assert!(last(&bot).starts_with("Use `/grant"), "{}", last(&bot));

    bot.say("room", "bob", "hi").await;
    bot.idle().await;
    assert_eq!(bot.agent.results(), [json!("not offered")]);

    bot.say("room", "ana", "/grant ops fake:test/bob 2h").await;
    bot.idle().await;
    assert!(last(&bot).starts_with("✓"), "{}", last(&bot));
    bot.say("room", "bob", "/roles").await;
    bot.idle().await;
    assert_eq!(last(&bot), "Your roles: ops.");
    bot.say("room", "bob", "hi").await;
    bot.idle().await;
    assert_eq!(bot.agent.results().len(), 2);
    assert_ne!(bot.agent.results()[1], json!("not offered"));

    bot.say("room", "ana", "/roles").await;
    bot.idle().await;
    assert!(
        last(&bot).contains("`ops` → `fake:test/bob`"),
        "{}",
        last(&bot)
    );
    bot.say("room", "ana", "/revoke ops fake:test/bob").await;
    bot.idle().await;
    assert!(last(&bot).starts_with("✓"), "{}", last(&bot));
    bot.say("room", "bob", "hi").await;
    bot.idle().await;
    assert_eq!(bot.agent.results()[2], json!("not offered"));
    bot.finish().await;
}

#[tokio::test]
async fn store_records_runs_for_the_person() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, _) = support::mcp_server(tools()).await;
    let bot = Bot::start(server, vec![], channel(), |c| {
        c.link_person(
            "lan",
            vec![
                (
                    "fake:test".parse().unwrap(),
                    okbot_core::UserId::new("lan-1"),
                ),
                (
                    "fake:test".parse().unwrap(),
                    okbot_core::UserId::new("lan-2"),
                ),
            ],
        );
    });
    bot.say("room", "lan-1", "hi").await;
    bot.say("room", "lan-2", "hi").await;
    bot.idle().await;
    let people: Vec<Option<String>> = bot.store.runs().into_iter().map(|r| r.person).collect();
    assert_eq!(
        people,
        [Some("person:lan".into()), Some("person:lan".into())]
    );
    bot.finish().await;
}

/// A local server in `sh`: answers `initialize`, lists two tools and reports its environment. The
/// request id is read wherever it is (key order depends on serde_json features).
#[cfg(unix)]
const LOCAL_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^[^}]*"id":\([^,}]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"0"}}}\n' "$id" ;;
    *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"env","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}},{"name":"wipe","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s|%s"}]}}\n' "$id" "$GREETING" "$CARGO_PKG_NAME" ;;
  esac
done
"#;

#[cfg(unix)]
#[tokio::test]
async fn local_servers_run_per_session_with_their_own_environment() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let steps = vec![
        Step::List("local"),
        Step::Call("local", "env", json!({})),
        Step::Call("local", "wipe", json!({})),
    ];
    let work = tempfile::tempdir().unwrap();
    let work_dir = work.path().to_path_buf();
    // No HTTP server: the extra server is local.
    let bot = Bot::start("127.0.0.1:9".parse().unwrap(), steps, channel(), |c| {
        c.work_dir.clone_from(&work_dir);
        c.mcp = vec![ExtraMcp {
            server: okbot_core::McpServer {
                name: "local".into(),
                url: String::new(),
                bearer_token_env: "OKBOT_MCP_LOCAL_TOKEN".into(),
            },
            token: Secret::new(String::new()),
            admins_only: false,
            signed: false,
            rules: McpRules {
                deny_tools: vec!["wipe".into()],
                ..McpRules::default()
            },
            stdio: Some(okbot_runtime::StdioServer {
                command: vec!["sh".into(), "-c".into(), LOCAL_SERVER.into()],
                env: vec![("GREETING".into(), Secret::new("hello".into()))],
            }),
            oauth: None,
        }];
    });
    bot.say("room", "bob", "go").await;
    bot.idle().await;
    assert_eq!(
        bot.agent.results(),
        [
            json!(["env"]),
            json!("hello|"),
            json!("This tool is not available to this person here.")
        ]
    );
    // The process and its directory are gone with the turn.
    let local = work_dir.join("mcp");
    eventually(|| std::fs::read_dir(&local).map_or(true, |mut d| d.next().is_none())).await;
    bot.finish().await;
}

#[tokio::test]
async fn only_tool_methods_pass_and_long_calls_are_not_asked_about() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, calls) = support::mcp_server(tools()).await;
    let steps = vec![
        Step::Raw(
            "ops",
            json!({"jsonrpc": "2.0", "id": 5, "method": "resources/read", "params": {"uri": "x"}}),
        ),
        Step::Raw(
            "ops",
            json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "drop_db"}}),
        ),
        Step::Raw("ops", json!({"jsonrpc": "2.0", "id": 6, "method": "ping"})),
        Step::Call(
            "ops",
            "deploy",
            json!({"note": "a".repeat(2000), "env": "prod"}),
        ),
    ];
    let bot = Bot::start(server, steps, channel(), |c| {
        c.mcp[0].rules.confirm = Confirm::Destructive;
    });
    bot.say("room", "bob", "go").await;
    bot.idle().await;
    let results = bot.agent.results();
    assert_eq!(results[0]["error"]["message"], "not offered through okbot");
    assert_eq!(
        results[1],
        json!(400),
        "a tool call without an id is refused"
    );
    assert_eq!(results[2]["result"], json!({}), "ping passes");
    assert!(
        results[3]
            .as_str()
            .unwrap()
            .contains("too long for a person to review"),
        "{}",
        results[3]
    );
    assert!(bot.channel.prompts().is_empty(), "nobody was asked");
    assert!(
        calls.lock().unwrap().is_empty(),
        "nothing reached the server"
    );
    let audit = bot.store.recorded_tool_calls();
    assert_eq!(audit[0].decision, ToolDecision::Declined);
    assert_eq!(audit[0].error.as_deref(), Some("too long to review"));
    bot.finish().await;
}

#[tokio::test]
async fn automation_turns_ask_before_any_call_that_is_not_read_only() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut listed = tools();
    // Additive, not destructive: `confirm = "destructive"` alone would let it through.
    listed
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "send_mail",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": false, "destructiveHint": false}}));
    let (server, calls) = support::mcp_server(listed).await;
    let steps = vec![
        Step::Call("ops", "status", json!({})),
        Step::Call("ops", "send_mail", json!({"to": "x@example.com"})),
    ];
    // The server's own rules let send_mail through and would ask the admins.
    let bot = Bot::start(server, steps, channel(), |c| {
        c.mcp[0].rules.confirm = Confirm::Destructive;
        c.mcp[0].rules.confirm_by = okbot_runtime::ConfirmBy::Admins;
    });
    let mut event = inbound(
        "fake:test",
        "room",
        "bob",
        "(Automation `x`: 1 new item(s).)",
    );
    event.event_id = "automation:x:1:ab".into();
    bot.channel.inject(event).await;
    eventually(|| !bot.channel.prompts().is_empty()).await;
    let prompt = bot.channel.prompts()[0].clone();
    assert!(prompt.text.contains("send_mail"), "{}", prompt.text);
    assert_eq!(
        prompt.allowed,
        [okbot_core::UserId::new("bob")],
        "the person it runs as decides"
    );
    bot.channel
        .answer(answer(&prompt, "bob", "deny", "room"))
        .await;
    bot.idle().await;
    let results = bot.agent.results();
    assert_eq!(
        results[0],
        json!("status ok"),
        "read-only calls need no approval"
    );
    assert!(results[1].as_str().unwrap().contains("did not approve"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(bot.agent.turns()[0].tools, okbot_core::ToolPolicy::McpOnly);

    // In a person's own turn the server's rules apply.
    bot.say("room", "bob", "mail it").await;
    bot.idle().await;
    assert_eq!(
        bot.agent.results()[2..],
        [json!("status ok"), json!("send_mail ok")]
    );
    bot.finish().await;
}
