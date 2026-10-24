//! Two bots of a local team, wired as `okbot run` wires them (ADR 0014): a person asks the
//! assistant, whose agent delegates through the gateway's `team` server; the research bot's
//! runtime answers over its Unix socket; the answer comes back into the assistant's turn.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use okbot_core::{
    AgentBackend, AgentCaps, AgentError, AgentReply, AgentTurn, CancellationToken, ChannelCaps,
    Clock, ConversationStore, Peers, Renderer, SystemClock, ToolPolicy, async_trait,
};
use okbot_peer::manifest::{Link, Manifest, Member};
use okbot_peer::{ManifestSource, Node, NodeConfig, SigningKey, TeamConfig};
use okbot_runtime::{Runtime, RuntimeConfig, TeamPolicy};
use okbot_testkit::{FakeChannel, MemoryStore, PlainRenderer, inbound};
use serde_json::{Value, json};

/// An agent that delegates its prompt's last line when it has the `team` server, and otherwise
/// answers `researched: <last line>`.
#[derive(Default)]
struct Agent {
    turns: Mutex<Vec<AgentTurn>>,
}

async fn rpc(url: &str, token: &str, message: Value) -> Value {
    let reply = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(&message)
        .send()
        .await
        .unwrap();
    reply.json().await.unwrap_or(Value::Null)
}

fn last_line(prompt: &str) -> String {
    prompt
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('<'))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[async_trait]
impl AgentBackend for Agent {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn caps(&self) -> AgentCaps {
        AgentCaps {
            mcp: true,
            tool_policies: vec![ToolPolicy::Web, ToolPolicy::McpOnly],
            ..AgentCaps::default()
        }
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        self.turns.lock().unwrap().push(turn.clone());
        let ask = last_line(&turn.prompt);
        let team = turn.mcp.iter().find(|s| s.name == "team");
        let text = match team {
            Some(server) => {
                let token = turn
                    .env
                    .iter()
                    .find(|(k, _)| *k == server.bearer_token_env)
                    .unwrap()
                    .1
                    .expose()
                    .clone();
                let reply = rpc(
                    &server.url,
                    &token,
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                        "name": "delegate",
                        "arguments": {"to": "skill:web", "task": ask, "wait_seconds": 20}}}),
                )
                .await;
                reply
                    .pointer("/result/content/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or("no answer")
                    .to_string()
            }
            None => format!("researched: {ask}"),
        };
        Ok(AgentReply {
            text,
            backend: "agent".into(),
            model: None,
            session_id: None,
            usage: None,
            duration: Duration::from_millis(1),
        })
    }
}

struct Bot {
    channel: Arc<FakeChannel>,
    agent: Arc<Agent>,
    store: Arc<MemoryStore>,
    handle: okbot_runtime::RuntimeHandle,
}

async fn start(root: &Path, name: &str, manifest: &Path, accept: bool) -> Bot {
    let dir = root.join(name);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(MemoryStore::new(clock.clone()));
    let node = Node::new(
        NodeConfig {
            key: SigningKey::for_bot(&dir.join("state")).unwrap(),
            box_key: okbot_peer::BoxKey::for_bot(&dir.join("state")).unwrap(),
            state_dir: dir.join("state"),
            sockets: root.join("run"),
            teams: vec![TeamConfig {
                id: "local".into(),
                member: name.into(),
                source: ManifestSource::File(manifest.to_path_buf()),
                accept,
                max_depth: 2,
                max_concurrent: 2,
                task_timeout: Duration::from_secs(60),
            }],
            external: None,
        },
        store.clone(),
        clock,
    )
    .unwrap();
    assert_eq!(node.start().await, Vec::<String>::new());
    let mut config = RuntimeConfig::new(name, dir.join("work"));
    config.teams = vec![TeamPolicy {
        id: "local".into(),
        roles: HashMap::new(),
        delegate_roles: vec!["everyone".into()],
        confirm: false,
        wait: Duration::from_secs(20),
        chain_tokens: None,
        act_as_person: false,
    }];
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let agent = Arc::new(Agent::default());
    let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
    let mut builder = Runtime::builder(config)
        .store(store.clone())
        .agent(agent.clone())
        .renderer(renderer)
        .channel(channel.clone());
    for peer_channel in node.channels() {
        builder = builder.channel(peer_channel);
    }
    let peers: Arc<dyn Peers> = Arc::new(node);
    let runtime = builder.peers(peers).build().unwrap();
    let handle = runtime.handle();
    tokio::spawn(async move { runtime.run(CancellationToken::new()).await.unwrap() });
    if accept {
        let socket = okbot_peer::socket_path(&root.join("run"), &dir);
        for _ in 0..200 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    Bot {
        channel,
        agent,
        store,
        handle,
    }
}

#[tokio::test]
async fn a_person_asks_the_assistant_and_the_research_bot_answers() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::new("local");
    manifest.team.version = 1;
    for (name, roles, skills) in [("assistant", "main", ""), ("research", "member", "web")] {
        SigningKey::for_bot(&root.path().join(name).join("state")).unwrap();
        manifest.members.insert(
            name.into(),
            Member {
                dir: Some(root.path().join(name)),
                roles: vec![roles.into()],
                skills: (!skills.is_empty())
                    .then(|| skills.into())
                    .into_iter()
                    .collect(),
                description: format!("the {name} bot"),
                ..Member::default()
            },
        );
    }
    manifest.links = vec![Link {
        from: vec!["role:main".into()],
        to: vec!["*".into()],
    }];
    let file = root.path().join("local.toml");
    std::fs::write(&file, manifest.to_toml()).unwrap();

    let research = start(root.path(), "research", &file, true).await;
    let assistant = start(root.path(), "assistant", &file, false).await;

    assistant
        .channel
        .inject(inbound("fake:test", "room", "lan", "What changed in v2?"))
        .await;
    tokio::time::timeout(Duration::from_secs(30), assistant.handle.wait_idle())
        .await
        .unwrap();

    let answer = assistant.channel.sent_texts().last().cloned().unwrap();
    assert!(answer.contains("Answer from `research`"), "{answer}");
    assert!(
        answer.contains("researched: What changed in v2?"),
        "{answer}"
    );

    // The research bot ran one turn, in its peer conversation, told who asks.
    let turns = research.agent.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].system.contains("another bot of your team `local`"));
    assert!(
        turns[0].mcp.iter().all(|s| s.name != "team"),
        "no team server: research may call no one"
    );

    let inbound = research.store.peer_tasks(10).await.unwrap();
    let outbound = assistant.store.peer_tasks(10).await.unwrap();
    assert_eq!(inbound.len(), 1);
    assert_eq!(outbound.len(), 1);
    assert_eq!(inbound[0].id, outbound[0].id);
    assert_eq!(
        inbound[0].trace, outbound[0].trace,
        "one trace across both bots"
    );
    assert_eq!(inbound[0].state, okbot_core::PeerTaskState::Completed);
    research.handle.wait_idle().await;
}
