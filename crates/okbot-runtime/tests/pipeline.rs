//! End-to-end pipeline tests with a fake channel, fake agents and an in-memory store.

use std::sync::Arc;
use std::time::Duration;

use okbot_core::{
    CancellationToken, ChannelCaps, ConversationStore, MessageRole, Renderer, ToolPolicy,
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
        let work = std::env::temp_dir().join("okbot-runtime-tests");
        let mut config = RuntimeConfig::new("okbot", work);
        config.typing_interval = Duration::from_millis(50);
        tweak(&mut config);
        let caps = ChannelCaps {
            typing: true,
            ..ChannelCaps::basic()
        };
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
    assert!(sent[2].contains("`ana`"), "{sent:?}");
    assert!(sent[3].contains("new conversation"), "{sent:?}");
    let key = okbot_testkit::conversation("fake:test", "room");
    assert!(
        h.store.recent(&key, 10).await.unwrap().is_empty(),
        "history reset"
    );
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
