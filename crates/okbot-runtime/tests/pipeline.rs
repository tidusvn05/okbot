//! End-to-end pipeline tests with a fake channel, fake agents and an in-memory store.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::sync::Arc;
use std::time::Duration;

use okbot_core::{
    AgentProgress, CancellationToken, ChannelCaps, ConversationStore, MessageRole, Renderer,
    ToolPolicy,
};
use okbot_runtime::{Runtime, RuntimeConfig, RuntimeHandle};
use okbot_testkit::{FakeAgent, FakeChannel, ManualClock, MemoryStore, PlainRenderer, inbound};

struct Harness {
    channel: Arc<FakeChannel>,
    agent: Arc<FakeAgent>,
    store: Arc<MemoryStore>,
    handle: RuntimeHandle,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn start(agent: FakeAgent) -> Self {
        Self::start_with(agent, |_| {})
    }

    fn start_with(agent: FakeAgent, tweak: impl FnOnce(&mut RuntimeConfig)) -> Self {
        Self::start_full(
            agent,
            ChannelCaps {
                typing: true,
                ..ChannelCaps::basic()
            },
            tweak,
        )
    }

    fn start_full(
        agent: FakeAgent,
        caps: ChannelCaps,
        tweak: impl FnOnce(&mut RuntimeConfig),
    ) -> Self {
        let work = std::env::temp_dir().join("okbot-runtime-tests");
        let mut config = RuntimeConfig::new("okbot", work);
        config.typing_interval = Duration::from_millis(50);
        tweak(&mut config);
        let channel = Arc::new(FakeChannel::new("fake:test", caps));
        let agent = Arc::new(agent.enforcing(&[ToolPolicy::Web, ToolPolicy::McpOnly]));
        let store = Arc::new(MemoryStore::new(Arc::new(ManualClock::default())));
        let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
        let runtime = Runtime::builder(config)
            .store(store.clone())
            .agent(agent.clone())
            .renderer(renderer)
            .channel(channel.clone())
            .build()
            .expect("runtime builds");
        let handle = runtime.handle();
        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let stop = stop.clone();
            async move {
                runtime.run(stop).await.expect("runtime runs");
            }
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

    async fn say(&self, text: &str) {
        self.channel
            .inject(inbound("fake:test", "room", "ana", text))
            .await;
    }

    async fn settle(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.handle.wait_idle())
            .await
            .expect("pipeline goes idle");
    }

    async fn stop(self) {
        self.stop.cancel();
        self.task.await.expect("runtime task");
    }
}

#[tokio::test]
async fn answers_and_stores_history() {
    let h = Harness::start(FakeAgent::replying("fake", "Xin **chào**!"));
    h.say("hello").await;
    h.settle().await;
    assert_eq!(h.channel.sent_texts(), ["Xin **chào**!"]);
    let key = okbot_testkit::conversation("fake:test", "room");
    let history = h.store.recent(&key, 10).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].role, MessageRole::Bot);
    let runs = h.store.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].error, None);
    let turn = &h.agent.turns()[0];
    assert!(turn.prompt.contains("hello"));
    assert!(turn.system.contains("You are okbot"));
    assert!(h.channel.typing_count() >= 1);
    h.stop().await;
}

#[tokio::test]
async fn drops_duplicates_and_unaddressed_group_messages() {
    let h = Harness::start(FakeAgent::echo("fake"));
    let event = inbound("fake:test", "room", "ana", "once");
    h.channel.inject(event.clone()).await;
    h.channel.inject(event).await;
    let mut group = inbound("fake:test", "group", "ana", "not for you");
    group.is_direct = false;
    h.channel.inject(group).await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 1);
    assert_eq!(h.channel.sent_texts().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn commands_do_not_reach_the_agent() {
    let h = Harness::start(FakeAgent::echo("fake"));
    h.say("first").await;
    h.settle().await;
    h.say("/help").await;
    h.say("/whoami").await;
    h.say("/reset").await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 1);
    let sent = h.channel.sent_texts();
    assert!(sent[1].contains("/reset"), "{sent:?}");
    assert!(sent[2].contains("`fake:test/ana`"), "{sent:?}");
    assert!(sent[2].contains("okbot people link"), "{sent:?}");
    assert!(sent[3].contains("new conversation"), "{sent:?}");
    let key = okbot_testkit::conversation("fake:test", "room");
    assert!(
        h.store.recent(&key, 10).await.unwrap().is_empty(),
        "history reset"
    );
    h.stop().await;
}

#[tokio::test]
async fn whoami_shows_the_linked_person() {
    let h = Harness::start_with(FakeAgent::echo("fake"), |c| {
        c.link_person(
            "ana-k",
            vec![
                ("fake:test".parse().unwrap(), "ana".into()),
                ("discord:main".parse().unwrap(), "42".into()),
            ],
        );
    });
    h.say("/whoami").await;
    h.settle().await;
    let sent = h.channel.sent_texts();
    assert!(
        sent[0].contains("belongs to `ana-k`") && sent[0].contains("2 linked"),
        "{sent:?}"
    );
    h.stop().await;
}

#[tokio::test]
async fn whoami_gives_admins_the_notify_address_of_the_chat() {
    let h = Harness::start_with(FakeAgent::echo("fake"), |c| {
        c.access
            .admins
            .insert(("fake:test".parse().unwrap(), "ana".into()));
    });
    h.say("/whoami").await;
    h.settle().await;
    let sent = h.channel.sent_texts();
    assert!(sent[0].contains("`room:fake:test|-|room|-`"), "{sent:?}");
    h.stop().await;
}

#[tokio::test]
async fn coalesces_messages_sent_during_a_turn() {
    let h = Harness::start(FakeAgent::sleeping("fake", Duration::from_millis(300)));
    h.say("one").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.say("two").await;
    h.say("three").await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 2);
    let second = &h.agent.turns()[1].prompt;
    assert!(second.contains("New messages to answer"), "{second}");
    let new_part = second.split("New messages to answer").nth(1).unwrap();
    assert!(new_part.contains("two") && new_part.contains("three") && !new_part.contains("one"));
    h.stop().await;
}

#[tokio::test]
async fn never_batches_two_authors_into_one_turn() {
    let h = Harness::start(FakeAgent::sleeping("fake", Duration::from_millis(300)));
    h.say("one").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    for (author, text) in [("bob", "from bob"), ("ana", "from ana")] {
        h.channel
            .inject(inbound("fake:test", "room", author, text))
            .await;
    }
    h.settle().await;
    // Each author's message runs in its own turn, with that author as the requester.
    assert_eq!(h.agent.calls(), 3);
    let turns = h.agent.turns();
    let new_part = |i: usize| {
        turns[i]
            .prompt
            // The fake agent echoes its prompt, so earlier answers repeat the marker: take the last.
            .rsplit("New message to answer")
            .next()
            .unwrap_or_default()
            .to_string()
    };
    assert!(new_part(1).contains("from bob") && !new_part(1).contains("from ana"));
    assert!(new_part(2).contains("from ana") && !new_part(2).contains("from bob"));
    h.stop().await;
}

#[tokio::test]
async fn stop_cancels_the_running_turn() {
    let h = Harness::start(FakeAgent::sleeping("fake", Duration::from_secs(30)));
    h.say("long question").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.say("/stop").await;
    h.settle().await;
    assert_eq!(h.channel.sent_texts(), ["Stopped."]);
    assert_eq!(h.store.runs()[0].error.as_deref(), Some("cancelled"));
    h.stop().await;
}

#[tokio::test]
async fn agent_failure_sends_the_error_reply() {
    let h = Harness::start(FakeAgent::failing("fake", true));
    h.say("hello").await;
    h.settle().await;
    assert!(h.channel.sent_texts()[0].starts_with("Sorry, something went wrong"));
    assert!(h.store.runs()[0].error.is_some());
    h.stop().await;
}

#[tokio::test]
async fn restricted_and_too_long_messages_get_fixed_replies() {
    let h = Harness::start_with(FakeAgent::echo("fake"), |c| c.access.max_input_chars = 10);
    let mut restricted = inbound("fake:test", "room", "ana", "");
    restricted.restricted = true;
    h.channel.inject(restricted).await;
    h.say("this message is far too long").await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 0);
    let sent = h.channel.sent_texts();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].contains("can't read"));
    assert!(sent[1].contains("too long"));
    h.stop().await;
}

#[tokio::test]
async fn room_rules_choose_the_tool_policy() {
    let h = Harness::start_with(FakeAgent::echo("fake"), |c| {
        c.rooms.insert(
            ("fake:test".parse().unwrap(), "room".into()),
            okbot_runtime::config::RoomRule {
                tool_policy: Some(ToolPolicy::McpOnly),
                internal: false,
                personal: false,
            },
        );
    });
    h.say("hi").await;
    h.settle().await;
    let turn = &h.agent.turns()[0];
    assert_eq!(turn.tools, ToolPolicy::McpOnly);
    assert!(turn.system.contains("cannot browse the web"));
    h.stop().await;
}

#[tokio::test]
async fn shutdown_lets_running_turns_finish() {
    let h = Harness::start(FakeAgent::sleeping("fake", Duration::from_millis(300)));
    h.say("question").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let channel = h.channel.clone();
    h.stop().await;
    assert_eq!(
        channel.sent_texts().len(),
        1,
        "answer sent before shutdown completed"
    );
}

#[tokio::test]
async fn commands_wait_their_turn_behind_messages() {
    let h = Harness::start(FakeAgent::sleeping("fake", Duration::from_millis(300)));
    h.say("one").await;
    h.say("/reset").await;
    h.say("two").await;
    h.settle().await;
    let turns = h.agent.turns();
    assert_eq!(turns.len(), 2);
    assert!(
        turns[0].prompt.contains("one"),
        "the first turn still sees its message"
    );
    assert!(
        !turns[1].prompt.contains("Earlier messages"),
        "{}",
        turns[1].prompt
    );
    assert!(
        !turns[1].prompt.contains("one"),
        "reset happened between the turns"
    );
    let sent = h.channel.sent_texts();
    assert!(
        sent[0].starts_with("echo:"),
        "answer to `one` comes before the reset reply: {sent:?}"
    );
    assert!(sent[1].contains("new conversation"), "{sent:?}");
    h.stop().await;
}

#[tokio::test]
async fn edit_capable_channels_get_a_thinking_message() {
    let caps = ChannelCaps {
        edit: true,
        typing: false,
        ..ChannelCaps::basic()
    };
    let h = Harness::start_full(FakeAgent::replying("fake", "the answer"), caps, |_| {});
    h.say("question").await;
    h.settle().await;
    assert_eq!(h.channel.sent_texts(), ["_Thinking…_"]);
    assert_eq!(
        h.channel.edits(),
        [("sent-1".to_string(), "the answer".to_string())]
    );
    assert_eq!(h.channel.typing_count(), 0);
    h.stop().await;
}

#[tokio::test]
async fn resumes_the_native_session() {
    let h = Harness::start(FakeAgent::echo("fake").with_session("s-1"));
    h.say("first question").await;
    h.settle().await;
    h.say("second question").await;
    h.settle().await;
    let turns = h.agent.turns();
    assert!(turns[0].resume.is_none());
    let resume = turns[1].resume.as_ref().expect("second turn resumes");
    assert_eq!(
        (resume.backend.as_str(), resume.session.as_str()),
        ("fake", "s-1")
    );
    assert!(resume.prompt.contains("second question"));
    assert!(
        !resume.prompt.contains("first question"),
        "{}",
        resume.prompt
    );
    assert!(
        turns[1].prompt.contains("first question"),
        "the full prompt stays as a fallback"
    );
    h.say("/reset").await;
    h.say("third").await;
    h.settle().await;
    assert!(
        h.agent.turns()[2].resume.is_none(),
        "reset forgets the session"
    );
    h.stop().await;
}

#[tokio::test]
async fn progress_updates_the_thinking_message() {
    let caps = ChannelCaps {
        edit: true,
        typing: false,
        ..ChannelCaps::basic()
    };
    let agent = FakeAgent::streaming(
        "fake",
        vec![
            AgentProgress::ToolUse("mcp__kb__kb_grep".into()),
            AgentProgress::TextDelta("Hel".into()),
            AgentProgress::TextDelta("lo".into()),
        ],
        Duration::from_millis(120),
        "Hello world",
    );
    let h = Harness::start_full(agent, caps, |c| {
        c.progress_interval = Duration::from_millis(40);
    });
    h.say("hi").await;
    h.settle().await;
    let edits: Vec<String> = h.channel.edits().into_iter().map(|(_, t)| t).collect();
    assert!(
        edits.iter().any(|t| t.contains("knowledge base")),
        "{edits:?}"
    );
    assert!(
        edits
            .iter()
            .any(|t| t.starts_with("Hel") && t.ends_with('…')),
        "{edits:?}"
    );
    assert_eq!(edits.last().map(String::as_str), Some("Hello world"));
    h.stop().await;
}

#[tokio::test]
async fn personal_bot_answers_only_paired_owner_accounts() {
    let h = Harness::start_with(FakeAgent::echo("fake"), |c| c.personal = true);
    let say_as = |user: &'static str, text: &'static str, direct: bool| {
        let mut event = inbound("fake:test", if direct { user } else { "group" }, user, text);
        event.is_direct = direct;
        event.mentions_bot = !direct;
        let channel = h.channel.clone();
        async move { channel.inject(event).await }
    };

    // Nobody is the owner yet: everyone is turned away, the agent never runs.
    say_as("ana", "hello", true).await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 0);
    assert!(h.channel.sent_texts()[0].contains("private assistant"));

    h.store
        .create_pairing_code("482913", Duration::from_secs(600))
        .await
        .unwrap();
    say_as("ana", "/pair 482913", false).await; // in a group: refused, code kept
    say_as("bob", "/pair 000000", true).await; // wrong code
    say_as("ana", "/pair 482913", true).await;
    h.settle().await;
    let sent = h.channel.sent_texts();
    assert!(sent[1].contains("direct message"), "{sent:?}");
    assert!(sent[2].contains("not valid"), "{sent:?}");
    assert!(sent[3].contains("Paired"), "{sent:?}");

    // The owner is answered and the agent sees the owner person; others still are not.
    say_as("ana", "what do you remember?", true).await;
    say_as("bob", "hi", true).await;
    say_as("ana", "/whoami", true).await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 1);
    let sent = h.channel.sent_texts();
    assert!(
        sent.iter().any(|t| t.contains("private assistant")),
        "{sent:?}"
    );
    assert!(
        sent.last().unwrap().contains("owner of this assistant"),
        "{sent:?}"
    );

    // In a group, others who mention the bot get no notice (it would repeat on every mention);
    // the owner is answered there too, with the same paired account.
    let before = h.channel.sent_texts().len();
    say_as("bob", "@bot hi", false).await;
    h.settle().await;
    assert_eq!(h.channel.sent_texts().len(), before);
    say_as("ana", "@bot hi", false).await;
    h.settle().await;
    assert_eq!(h.agent.calls(), 2);
    h.stop().await;
}

/// Sends `text` as ana in a direct chat and waits until it is handled.
async fn command(channel: &FakeChannel, handle: &RuntimeHandle, text: &str) {
    channel
        .inject(inbound("fake:test", "room", "ana", text))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
}

#[tokio::test]
async fn snooze_ignores_planted_lines_and_routines() {
    use okbot_core::{SystemClock, Task, TaskKind, TaskStatus};
    let mut config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-runtime-tests"));
    config.schedule.rescan = Duration::from_millis(100);
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let clock = Arc::new(SystemClock);
    let tasks = Arc::new(okbot_testkit::MemoryTasks::new(clock.clone()));
    let store = Arc::new(MemoryStore::new(clock.clone()));
    let runtime = Runtime::builder(config)
        .store(store.clone())
        .agent(Arc::new(FakeAgent::echo("fake")))
        .renderer(Arc::new(PlainRenderer))
        .tasks(tasks.clone())
        .channel(channel.clone())
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });

    let key = okbot_testkit::conversation("fake:test", "room");
    // A user cannot plant the scheduler's line, and a routine is never snoozed back on.
    let now = jiff::Timestamp::now();
    let marker = "\n\n(reminder `t-9`; the user may snooze it or mark it done)";
    channel
        .inject(inbound("fake:test", "room", "ana", &format!("hi{marker}")))
        .await;
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap();
    command(&channel, &handle, "/snooze 1m").await;
    let reply = channel.sent_texts().last().unwrap().clone();
    assert!(reply.contains("no reminder"), "{reply}");
    tasks.insert(Task {
        id: "tasks/users/fake-test/ana/t-9".into(),
        kind: TaskKind::Routine,
        status: TaskStatus::Paused,
        title: "Brief".into(),
        notes: "Summarise".into(),
        scope: okbot_core::MemoryScope::User,
        due: None,
        remind_at: Some(now + jiff::SignedDuration::from_hours(5)),
        repeat: None,
        deliver_to: None,
        origin: Some(key.clone()),
        author: "fake:test/ana".into(),
        created: now,
    });
    store
        .append(okbot_core::NewMessage {
            conversation: key.clone(),
            role: okbot_core::MessageRole::Bot,
            author_id: None,
            author_name: "okbot".into(),
            text: format!("⏰ Brief{marker}"),
            message_id: None,
            at: now,
        })
        .await
        .unwrap();
    command(&channel, &handle, "/snooze 1m").await;
    let reply = channel.sent_texts().last().unwrap().clone();
    assert!(reply.contains("cannot be snoozed"), "{reply}");
    assert_eq!(tasks.all()[0].status, TaskStatus::Paused);
    let who = okbot_core::Requester {
        conversation: key.clone(),
        user: okbot_core::UserRef {
            id: "ana".into(),
            display_name: "ana".into(),
            is_bot: false,
        },
        internal: false,
        person: None,
        personal: true,
    };
    okbot_core::TaskStore::delete(tasks.as_ref(), &who, "t-9")
        .await
        .unwrap();

    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn reminders_are_sent_by_the_scheduler_and_tasks_commands_work() {
    use okbot_core::{SystemClock, Task, TaskKind, TaskStatus};
    let mut config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-runtime-tests"));
    config.schedule.rescan = Duration::from_millis(100);
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let clock = Arc::new(SystemClock);
    let tasks = Arc::new(okbot_testkit::MemoryTasks::new(clock.clone()));
    let store = Arc::new(MemoryStore::new(clock.clone()));
    let runtime = Runtime::builder(config)
        .store(store.clone())
        .agent(Arc::new(FakeAgent::echo("fake")))
        .renderer(Arc::new(PlainRenderer))
        .tasks(tasks.clone())
        .channel(channel.clone())
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });

    command(&channel, &handle, "/tasks").await;
    assert!(
        channel.sent_texts()[0].starts_with("No tasks"),
        "{:?}",
        channel.sent_texts()
    );
    let key = okbot_testkit::conversation("fake:test", "room");
    let fake: okbot_core::ChannelId = "fake:test".parse().unwrap();
    assert_eq!(
        store.direct_chat(&fake, &"ana".into()).await.unwrap(),
        Some(key.clone()),
        "direct chats are remembered for reminders"
    );

    let now = jiff::Timestamp::now();
    tasks.insert(Task {
        id: "tasks/users/fake-test/ana/t-1".into(),
        kind: TaskKind::Reminder,
        status: TaskStatus::Open,
        title: "Stretch".into(),
        notes: String::new(),
        scope: okbot_core::MemoryScope::User,
        due: None,
        remind_at: Some(now + jiff::SignedDuration::from_millis(300)),
        repeat: None,
        deliver_to: None,
        origin: Some(key.clone()),
        author: "fake:test/ana".into(),
        created: now,
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !channel.sent_texts().iter().any(|t| t.contains("Stretch")) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the reminder is sent");
    assert_eq!(tasks.all()[0].status, TaskStatus::Done);

    command(&channel, &handle, "/snooze 1h").await;
    let reply = channel.sent_texts().last().unwrap().clone();
    assert!(
        reply.contains("remind you again") && reply.contains("Stretch"),
        "{reply}"
    );
    let snoozed = tasks.all()[0].clone();
    assert_eq!(snoozed.status, TaskStatus::Open);
    let wait = snoozed
        .remind_at
        .unwrap()
        .duration_since(jiff::Timestamp::now());
    assert!(wait > jiff::SignedDuration::from_mins(59), "{wait}");
    let done = okbot_core::TaskChange {
        status: Some(TaskStatus::Done),
        ..okbot_core::TaskChange::default()
    };
    okbot_core::TaskStore::after_fire(tasks.as_ref(), "t-1", done)
        .await
        .unwrap();

    tasks.insert(Task {
        id: "tasks/users/fake-test/ana/t-2".into(),
        kind: TaskKind::Todo,
        remind_at: None,
        title: "Buy milk".into(),
        status: TaskStatus::Open,
        ..tasks.all()[0].clone()
    });
    command(&channel, &handle, "/done 1").await;
    assert_eq!(channel.sent_texts().last().unwrap(), "Done: Buy milk");
    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn configured_mcp_servers_reach_the_agent() {
    let server = |name: &str, admins_only| okbot_runtime::ExtraMcp {
        server: okbot_core::McpServer {
            name: name.into(),
            url: format!("http://127.0.0.1:9/{name}"),
            bearer_token_env: format!("{}_TOKEN", name.to_uppercase()),
        },
        token: okbot_core::Secret::new(format!("{name}-secret")),
        admins_only,
        signed: false,
        rules: okbot_runtime::McpRules::default(),
        stdio: None,
    };
    let caps = okbot_core::AgentCaps {
        mcp: true,
        ..okbot_core::AgentCaps::default()
    };
    let h = Harness::start_with(FakeAgent::echo("fake").with_caps(caps), |c| {
        c.mcp = vec![server("ops", false), server("root", true)];
    });
    h.say("hi").await;
    h.settle().await;
    let turn = &h.agent.turns()[0];
    let names: Vec<&str> = turn.mcp.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["ops"],
        "admins_only servers are not offered to others"
    );
    // Through the gateway, with a token for this turn instead of the secret.
    assert!(turn.mcp[0].url.ends_with("/mcp/ops"), "{}", turn.mcp[0].url);
    let env: Vec<(&str, &str)> = turn
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.expose().as_str()))
        .collect();
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].0, "OPS_TOKEN");
    assert_ne!(env[0].1, "ops-secret");
    h.stop().await;
}
