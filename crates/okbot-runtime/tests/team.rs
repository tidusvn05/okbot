//! Peers and teams in the runtime (ADR 0014): what a task from another bot may do, and
//! delegating through the gateway's `team` server.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use okbot_core::{
    Budget, CancellationToken, ChannelCaps, ConversationKey, ConversationStore, Delegation,
    DelegationContext, Origin, PeerCaller, PeerError, PeerInfo, PeerPrompt, PeerTask,
    PeerTaskState, Peers, PromptAnswer, Renderer, Secret, ToolDecision, UserRef, async_trait,
};
use okbot_runtime::{ExtraMcp, McpRules, Member, Roles, Runtime, RuntimeConfig, TeamPolicy};
use okbot_testkit::{FakeChannel, MemoryStore, PlainRenderer, inbound};
use serde_json::json;
use support::{Step, ToolAgent};

/// Peers that answer from a script.
#[derive(Default)]
struct FakePeers {
    callers: Mutex<HashMap<ConversationKey, PeerCaller>>,
    delegations: Mutex<Vec<Delegation>>,
    /// What `delegate` returns.
    reply: Mutex<Option<PeerTask>>,
    answers: Mutex<Vec<(String, String)>>,
    cancelled: Mutex<Vec<()>>,
    failed: Mutex<Vec<(ConversationKey, Option<String>, String)>>,
    usage: Mutex<Vec<(Option<String>, u64)>>,
    progress: Mutex<Vec<String>>,
}

fn task(state: PeerTaskState, text: &str) -> PeerTask {
    PeerTask {
        team: "local".into(),
        peer: "research".into(),
        id: "t-1".into(),
        state,
        text: Some(text.into()),
        prompt: None,
        tokens: None,
    }
}

#[async_trait]
impl Peers for FakePeers {
    async fn directory(&self) -> Result<Vec<PeerInfo>, PeerError> {
        Ok(vec![PeerInfo {
            team: "local".into(),
            name: "research".into(),
            roles: vec!["member".into()],
            skills: vec!["web".into()],
            description: "Reads the web".into(),
        }])
    }

    async fn delegate(&self, delegation: Delegation) -> Result<PeerTask, PeerError> {
        self.delegations.lock().unwrap().push(delegation);
        Ok(self.reply.lock().unwrap().clone().unwrap())
    }

    async fn status(&self, _: &str, _: &str, _: &str) -> Result<PeerTask, PeerError> {
        Ok(self.reply.lock().unwrap().clone().unwrap())
    }

    async fn cancel(&self, _: &str, _: &str, _: &str) -> Result<PeerTask, PeerError> {
        self.cancelled.lock().unwrap().push(());
        Ok(task(PeerTaskState::Canceled, "stopped"))
    }

    fn progress(&self, _: &ConversationKey, _: Option<&str>, text: &str) {
        self.progress.lock().unwrap().push(text.to_string());
    }

    async fn report_usage(&self, _: &ConversationKey, task: Option<&str>, tokens: u64) {
        self.usage
            .lock()
            .unwrap()
            .push((task.map(str::to_string), tokens));
    }

    async fn fail(&self, conversation: &ConversationKey, task: Option<&str>, reason: &str) {
        self.failed.lock().unwrap().push((
            conversation.clone(),
            task.map(str::to_string),
            reason.to_string(),
        ));
    }

    async fn answer(
        &self,
        _: &PeerTask,
        prompt: &str,
        button: &str,
    ) -> Result<PeerTask, PeerError> {
        self.answers
            .lock()
            .unwrap()
            .push((prompt.to_string(), button.to_string()));
        Ok(task(
            PeerTaskState::Completed,
            &format!("restarted after {button}"),
        ))
    }

    async fn caller(&self, conversation: &ConversationKey) -> Option<PeerCaller> {
        self.callers.lock().unwrap().get(conversation).cloned()
    }

    fn team_person(&self, account: &str) -> Option<String> {
        (account == "fake:test/lan-chat").then(|| "lan".into())
    }

    fn member_name(&self, _: &str) -> Option<String> {
        Some("assistant".into())
    }
}

struct Bot {
    channel: Arc<FakeChannel>,
    agent: Arc<ToolAgent>,
    store: Arc<MemoryStore>,
    peers: Arc<FakePeers>,
    handle: okbot_runtime::RuntimeHandle,
    stop: CancellationToken,
}

fn infra() -> ExtraMcp {
    ExtraMcp {
        server: okbot_core::McpServer {
            name: "infra".into(),
            url: "http://127.0.0.1:9/mcp".into(),
            bearer_token_env: "INFRA_TOKEN".into(),
        },
        token: Secret::new("k".into()),
        admins_only: false,
        signed: false,
        rules: McpRules {
            roles: vec!["ops".into()],
            ..McpRules::default()
        },
        stdio: None,
        oauth: None,
    }
}

fn start(channel: &str, steps: Vec<Step>, configure: impl FnOnce(&mut RuntimeConfig)) -> Bot {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-team-tests"));
    config.mcp = vec![infra()];
    let mut roles = Roles::default();
    roles.add("ops", Member::Person("lan".into()));
    config.roles = roles;
    config.people.insert(
        ("fake:test".parse().unwrap(), "lan-chat".into()),
        okbot_core::Person {
            id: "lan".into(),
            accounts: vec![("fake:test".parse().unwrap(), "lan-chat".into())],
        },
    );
    config.teams = vec![TeamPolicy {
        id: "local".into(),
        roles: HashMap::from([("main".into(), vec!["ops".into()])]),
        delegate_roles: vec!["everyone".into()],
        confirm: false,
        wait: Duration::from_secs(5),
        chain_tokens: None,
        act_as_person: false,
    }];
    configure(&mut config);
    let channel = Arc::new(FakeChannel::new(channel, ChannelCaps::basic()));
    let agent = Arc::new(ToolAgent::new(steps));
    let store = Arc::new(MemoryStore::new(Arc::new(okbot_core::SystemClock)));
    let peers = Arc::new(FakePeers::default());
    let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
    let runtime = Runtime::builder(config)
        .store(store.clone())
        .agent(agent.clone())
        .renderer(renderer)
        .channel(channel.clone())
        .peers(peers.clone())
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });
    Bot {
        channel,
        agent,
        store,
        peers,
        handle,
        stop,
    }
}

impl Bot {
    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(20), self.handle.wait_idle())
            .await
            .unwrap();
    }
}

fn context(person: Option<&str>) -> DelegationContext {
    DelegationContext {
        origin: Origin {
            bot: "assistant".into(),
            conversation: "zalo:shop|-|1|-".into(),
            person: person.map(str::to_string),
            name: "Lan".into(),
        },
        chain: vec!["assistant".into()],
        deadline: "2099-01-01T00:00:00Z".parse().unwrap(),
        budget: Budget {
            turns: 3,
            tokens: None,
        },
        trace: "tr".into(),
    }
}

/// Delivers a task from `caller` (team roles `roles`) for `person`, as the peer adapter does.
async fn task_from(bot: &Bot, room: &str, roles: &[&str], person: Option<&str>, text: &str) {
    let event = inbound("peer:local", room, "assistant", text);
    bot.peers.callers.lock().unwrap().insert(
        event.conversation.clone(),
        PeerCaller {
            team: "local".into(),
            member: "assistant".into(),
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
            context: context(person),
        },
    );
    bot.channel.inject(event).await;
    bot.idle().await;
}

#[tokio::test]
async fn a_task_from_another_bot_gets_the_intersected_roles() {
    let bot = start("peer:local", vec![Step::List("infra")], |c| {
        c.teams[0].act_as_person = true;
    });
    // The caller's role maps to `ops`, and lan has `ops` here: offered.
    task_from(&bot, "a/1", &["main"], Some("lan"), "restart nginx").await;
    // A guest: `ops` from the mapping, but not from the person: not offered.
    task_from(&bot, "a/2", &["main"], None, "restart nginx").await;
    // lan, but the caller's team role maps to nothing: not offered.
    task_from(&bot, "a/3", &["member"], Some("lan"), "restart nginx").await;
    let results = bot.agent.results();
    assert_ne!(results[0], json!("not offered"), "{results:?}");
    assert_eq!(results[1], json!("not offered"));
    assert_eq!(results[2], json!("not offered"));
    // Usage counts for the origin person.
    let usage = bot
        .store
        .usage_since("person:lan", "2000-01-01T00:00:00Z".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(usage.turns, 2);
    bot.stop.cancel();
}

#[tokio::test]
async fn admins_never_come_through_another_bot_and_commands_never_run() {
    let bot = start("peer:local", vec![Step::List("infra")], |c| {
        c.mcp[0].admins_only = true;
        c.mcp[0].rules.roles.clear();
        c.access
            .admins
            .insert(("peer:local".parse().unwrap(), "assistant".into()));
    });
    task_from(&bot, "a/1", &["main"], Some("lan"), "/reset").await;
    assert_eq!(bot.agent.results(), [json!("not offered")]);
    // `/reset` was text for the agent, not a command.
    assert_eq!(bot.agent.turns().len(), 1);
    assert!(bot.agent.turns()[0].prompt.contains("/reset"));
    assert!(
        bot.agent.turns()[0]
            .system
            .contains("another bot of your team")
    );
    bot.stop.cancel();
}

fn delegate_steps() -> Vec<Step> {
    vec![
        Step::List("team"),
        Step::Call(
            "team",
            "delegate",
            json!({"to": "research", "task": "Find the release notes"}),
        ),
    ]
}

#[tokio::test]
async fn the_agent_delegates_through_the_team_server_and_the_call_is_audited() {
    let bot = start("fake:test", delegate_steps(), |_| {});
    *bot.peers.reply.lock().unwrap() = Some(task(PeerTaskState::Completed, "v2.1 is out"));
    bot.channel
        .inject(inbound("fake:test", "room", "lan-chat", "what is new?"))
        .await;
    bot.idle().await;
    let results = bot.agent.results();
    assert_eq!(
        results[0],
        json!(["team_directory", "delegate", "task_cancel", "task_status"])
    );
    let answer = results[1].as_str().unwrap();
    assert!(answer.contains("Answer from `research`"), "{answer}");
    assert!(answer.contains("v2.1 is out"));

    let delegation = bot.peers.delegations.lock().unwrap()[0].clone();
    assert_eq!(delegation.team.as_deref(), Some("local"));
    assert_eq!(delegation.context.origin.person.as_deref(), Some("lan"));
    assert_eq!(delegation.context.origin.conversation, "fake:test|-|room|-");
    assert!(
        delegation.context.chain.is_empty(),
        "the adapter adds this bot"
    );

    let calls = bot
        .store
        .tool_calls("2000-01-01T00:00:00Z".parse().unwrap(), 10)
        .await
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0].server.as_str(), calls[0].tool.as_str()),
        ("team", "delegate")
    );
    assert_eq!(calls[0].decision, ToolDecision::Allowed);
    bot.stop.cancel();
}

#[tokio::test]
async fn only_delegate_roles_get_the_team_server() {
    let bot = start("fake:test", delegate_steps(), |c| {
        c.teams[0].delegate_roles = vec!["admins".into()];
    });
    bot.channel
        .inject(inbound("fake:test", "room", "bob", "what is new?"))
        .await;
    bot.idle().await;
    assert_eq!(
        bot.agent.results(),
        [json!("not offered"), json!("not offered")]
    );
    assert!(bot.peers.delegations.lock().unwrap().is_empty());
    bot.stop.cancel();
}

#[tokio::test]
async fn the_other_bots_question_goes_to_the_requester() {
    let bot = start("fake:test", delegate_steps(), |_| {});
    let mut asking = task(PeerTaskState::InputRequired, "Restart nginx?");
    asking.prompt = Some(PeerPrompt {
        id: "p-9".into(),
        buttons: vec![
            ("allow".into(), "Allow".into()),
            ("deny".into(), "Deny".into()),
        ],
    });
    *bot.peers.reply.lock().unwrap() = Some(asking);
    bot.channel
        .inject(inbound("fake:test", "room", "lan-chat", "restart it"))
        .await;
    // The question appears for the requester; their press answers it.
    let mut prompt = None;
    for _ in 0..200 {
        prompt = bot.channel.prompts().into_iter().next();
        if prompt.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let prompt = prompt.expect("a prompt is shown");
    assert!(prompt.text.contains("Restart nginx?"));
    assert_eq!(prompt.allowed, ["lan-chat".into()]);
    let sent = okbot_core::ReplyTarget {
        conversation: ConversationKey::room("fake:test".parse().unwrap(), "room"),
        in_reply_to: None,
    };
    bot.channel
        .answer(PromptAnswer {
            prompt: prompt.id.clone(),
            button: "allow".into(),
            values: std::collections::BTreeMap::new(),
            user: UserRef {
                id: "lan-chat".into(),
                display_name: "lan".into(),
                is_bot: false,
            },
            conversation: sent.conversation.clone(),
            sent: okbot_core::SentRef {
                target: sent,
                message_id: prompt.id.as_str().into(),
            },
        })
        .await;
    bot.idle().await;
    assert_eq!(
        bot.peers.answers.lock().unwrap().clone(),
        [("p-9".to_string(), "allow".to_string())]
    );
    let answer = bot.agent.results()[1].as_str().unwrap().to_string();
    assert!(answer.contains("restarted after allow"), "{answer}");
    bot.stop.cancel();
}

/// An agent that always fails.
struct FailingAgent;

#[async_trait]
impl okbot_core::AgentBackend for FailingAgent {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn caps(&self) -> okbot_core::AgentCaps {
        okbot_core::AgentCaps::default()
    }

    async fn run(
        &self,
        _: okbot_core::AgentTurn,
    ) -> Result<okbot_core::AgentReply, okbot_core::AgentError> {
        Err(okbot_core::AgentError::Unavailable("down".into()))
    }
}

#[tokio::test]
async fn a_failed_turn_fails_the_task_instead_of_apologising() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-team-tests"));
    let channel = Arc::new(FakeChannel::new("peer:local", ChannelCaps::basic()));
    let peers = Arc::new(FakePeers::default());
    let runtime = Runtime::builder(config)
        .store(Arc::new(MemoryStore::new(Arc::new(
            okbot_core::SystemClock,
        ))))
        .agent(Arc::new(FailingAgent))
        .renderer(Arc::new(PlainRenderer))
        .channel(channel.clone())
        .peers(peers.clone())
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });
    let event = inbound("peer:local", "a/1", "assistant", "do it");
    let task = event.message_id.clone().unwrap();
    channel.inject(event).await;
    tokio::time::timeout(Duration::from_secs(20), handle.wait_idle())
        .await
        .unwrap();
    assert!(
        channel.sent_texts().is_empty(),
        "{:?}",
        channel.sent_texts()
    );
    let failed = peers.failed.lock().unwrap().clone();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].1.as_deref(), Some(task.as_str()));
    assert!(failed[0].2.contains("failed"));
    stop.cancel();
}

#[tokio::test]
async fn a_cancelled_task_stops_its_turn_silently() {
    let channel = Arc::new(FakeChannel::new("peer:local", ChannelCaps::basic()));
    let peers = Arc::new(FakePeers::default());
    let runtime = Runtime::builder(RuntimeConfig::new(
        "okbot",
        std::env::temp_dir().join("okbot-team-tests"),
    ))
    .store(Arc::new(MemoryStore::new(Arc::new(
        okbot_core::SystemClock,
    ))))
    .agent(Arc::new(okbot_testkit::FakeAgent::sleeping(
        "slow",
        Duration::from_secs(30),
    )))
    .renderer(Arc::new(PlainRenderer))
    .channel(channel.clone())
    .peers(peers.clone())
    .build()
    .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });
    channel
        .inject(inbound("peer:local", "a/1", "assistant", "long job"))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut cancel = inbound("peer:local", "a/1", "assistant", "");
    cancel.event_id = format!("{}x", okbot_core::PEER_CANCEL_PREFIX);
    channel.inject(cancel).await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    // The turn stopped: the task fails as "stopped", nothing is sent as chat.
    let failed = peers.failed.lock().unwrap().clone();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].2, "stopped");
    assert_eq!(channel.sent_texts(), Vec::<String>::new());
    stop.cancel();
}

#[tokio::test]
async fn a_task_still_running_at_a_restart_still_brings_its_result() {
    let store = Arc::new(MemoryStore::new(Arc::new(okbot_core::SystemClock)));
    let now = okbot_core::Clock::now(&okbot_core::SystemClock);
    store
        .put_peer_task(okbot_core::PeerTaskRecord {
            id: "t-1".into(),
            inbound: false,
            team: "local".into(),
            peer: "research".into(),
            context_id: "c".into(),
            message_id: "m".into(),
            state: PeerTaskState::Working,
            conversation: Some(ConversationKey::room("fake:test".parse().unwrap(), "room")),
            trace: "tr".into(),
            text: None,
            requester: Some(UserRef {
                id: "lan-chat".into(),
                display_name: "Lan".into(),
                is_bot: false,
            }),
            direct: true,
            tokens: None,
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let peers = Arc::new(FakePeers::default());
    *peers.reply.lock().unwrap() = Some(task(PeerTaskState::Completed, "v2.1 is out"));
    let agent = Arc::new(okbot_testkit::FakeAgent::echo("echo"));
    let runtime = Runtime::builder(RuntimeConfig::new(
        "okbot",
        std::env::temp_dir().join("okbot-team-tests"),
    ))
    .store(store)
    .agent(agent.clone())
    .renderer(Arc::new(PlainRenderer))
    .channel(channel.clone())
    .peers(peers)
    .build()
    .unwrap();
    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });
    for _ in 0..300 {
        if agent.calls() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let turns = agent.turns();
    assert_eq!(turns.len(), 1, "the result starts a turn");
    assert!(turns[0].prompt.contains("v2.1 is out"));
    assert!(turns[0].prompt.contains("task handed to `research`"));
    stop.cancel();
}

#[tokio::test]
async fn a_chain_stops_at_its_token_budget() {
    let steps = vec![
        Step::Call(
            "team",
            "delegate",
            json!({"to": "research", "task": "first"}),
        ),
        Step::Call(
            "team",
            "delegate",
            json!({"to": "research", "task": "second"}),
        ),
    ];
    let bot = start("fake:test", steps, |c| c.teams[0].chain_tokens = Some(1000));
    let mut done = task(PeerTaskState::Completed, "ok");
    done.tokens = Some(1000);
    *bot.peers.reply.lock().unwrap() = Some(done);
    bot.channel
        .inject(inbound("fake:test", "room", "lan-chat", "go"))
        .await;
    bot.idle().await;
    let delegations = bot.peers.delegations.lock().unwrap().clone();
    assert_eq!(
        delegations.len(),
        1,
        "the second one is refused before it starts"
    );
    assert_eq!(delegations[0].context.budget.tokens, Some(1000));
    let second = bot.agent.results()[1].as_str().unwrap().to_string();
    assert!(second.contains("token budget"), "{second}");
    bot.stop.cancel();
}

#[tokio::test]
async fn a_peer_turn_reports_the_tokens_it_used() {
    let bot = start("peer:local", vec![Step::List("infra")], |_| {});
    task_from(&bot, "a/1", &["main"], Some("lan"), "count").await;
    let usage = bot.peers.usage.lock().unwrap().clone();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].1, 110, "input + output of the turn");
    bot.stop.cancel();
}

#[tokio::test]
async fn a_peer_turn_passes_its_progress_to_the_caller() {
    let channel = Arc::new(FakeChannel::new("peer:local", ChannelCaps::basic()));
    let peers = Arc::new(FakePeers::default());
    let agent = okbot_testkit::FakeAgent::streaming(
        "streamer",
        vec![
            okbot_core::AgentProgress::ToolUse("WebSearch".into()),
            okbot_core::AgentProgress::TextDelta("Found two ".into()),
            okbot_core::AgentProgress::TextDelta("releases".into()),
        ],
        Duration::from_millis(5),
        "done",
    );
    let runtime = Runtime::builder(RuntimeConfig::new(
        "okbot",
        std::env::temp_dir().join("okbot-team-tests"),
    ))
    .store(Arc::new(MemoryStore::new(Arc::new(
        okbot_core::SystemClock,
    ))))
    .agent(Arc::new(agent))
    .renderer(Arc::new(PlainRenderer))
    .channel(channel.clone())
    .peers(peers.clone())
    .build()
    .unwrap();
    let handle = runtime.handle();
    tokio::spawn(async move { runtime.run(CancellationToken::new()).await.unwrap() });
    channel
        .inject(inbound("peer:local", "a/1", "assistant", "what is new?"))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    assert_eq!(
        peers.progress.lock().unwrap().clone(),
        ["Searching the web…", "Found two ", "Found two releases"]
    );
}

async fn usage_keys(bot: &Bot) -> (u64, u64, u64) {
    let since = "2000-01-01T00:00:00Z".parse().unwrap();
    let turns = |key: &'static str| {
        let store = bot.store.clone();
        async move { store.usage_since(key, since).await.unwrap().turns }
    };
    (
        turns("person:lan").await,
        turns("person:owner").await,
        turns("peer:local/assistant").await,
    )
}

#[tokio::test]
async fn without_act_as_person_a_task_never_acts_as_the_person_it_names() {
    let bot = start("peer:local", vec![Step::List("infra")], |_| {});
    task_from(&bot, "a/1", &["main"], Some("lan"), "read lan's mail").await;
    // Not lan's limits (nor lan's accounts or memory): the calling bot's.
    assert_eq!(usage_keys(&bot).await, (0, 0, 1));
    bot.stop.cancel();
}

#[tokio::test]
async fn a_personal_bot_never_lends_its_owner_even_with_act_as_person() {
    let bot = start("peer:local", vec![Step::List("infra")], |c| {
        c.personal = true;
        c.teams[0].act_as_person = true;
    });
    task_from(
        &bot,
        "a/1",
        &["main"],
        Some(okbot_runtime::OWNER),
        "read the owner's mail",
    )
    .await;
    task_from(&bot, "a/2", &["main"], Some("lan"), "read lan's mail").await;
    assert_eq!(usage_keys(&bot).await, (0, 0, 2));
    bot.stop.cancel();
}

#[tokio::test]
async fn act_as_person_needs_the_person_in_people() {
    let bot = start("peer:local", vec![Step::List("infra")], |c| {
        c.teams[0].act_as_person = true;
    });
    task_from(&bot, "a/1", &["main"], Some("mallory"), "who am I").await;
    task_from(&bot, "a/2", &["main"], Some("lan"), "who am I").await;
    assert_eq!(usage_keys(&bot).await, (1, 0, 1));
    bot.stop.cancel();
}
