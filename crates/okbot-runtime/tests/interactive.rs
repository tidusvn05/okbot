//! Interactive replies end to end (ADR 0015): the agent's `show` tool, the prompt after the
//! answer, and a press becoming the clicker's next message.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

mod support;

use std::sync::Arc;
use std::time::Duration;

use okbot_core::{
    CancellationToken, ChannelCaps, ConversationStore as _, PromptAnswer, Renderer, ReplyMode,
    UiCaps, UserRef,
};
use okbot_runtime::{Runtime, RuntimeConfig};
use okbot_testkit::{FakeChannel, MemoryStore, PlainRenderer, inbound};
use serde_json::json;
use support::{Step, ToolAgent};

struct Bot {
    channel: Arc<FakeChannel>,
    agent: Arc<ToolAgent>,
    store: Arc<MemoryStore>,
    handle: okbot_runtime::RuntimeHandle,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Bot {
    fn start(steps: Vec<Step>, ui: UiCaps, configure: impl FnOnce(&mut RuntimeConfig)) -> Self {
        Self::launch(steps, ui, configure, None)
    }

    fn launch(
        steps: Vec<Step>,
        ui: UiCaps,
        configure: impl FnOnce(&mut RuntimeConfig),
        charts: Option<Arc<dyn okbot_core::ChartRenderer>>,
    ) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut config = RuntimeConfig::new(
            "okbot",
            std::env::temp_dir().join("okbot-interactive-tests"),
        );
        config.reply_mode = ReplyMode::Interactive;
        configure(&mut config);
        let channel = Arc::new(FakeChannel::new(
            "fake:test",
            ChannelCaps {
                ui,
                ..ChannelCaps::basic()
            },
        ));
        let agent = Arc::new(ToolAgent::new(steps));
        let store = Arc::new(MemoryStore::new(Arc::new(okbot_core::SystemClock)));
        let renderer: Arc<dyn Renderer> = Arc::new(PlainRenderer);
        let mut builder = Runtime::builder(config)
            .store(store.clone())
            .agent(agent.clone())
            .renderer(renderer)
            .channel(channel.clone());
        if let Some(charts) = charts {
            builder = builder.charts(charts);
        }
        let runtime = builder.build().unwrap();
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

    async fn press(&self, room: &str, user: &str, prompt: &str, button: &str) {
        let conversation = okbot_testkit::conversation("fake:test", room);
        self.channel
            .answer(PromptAnswer {
                prompt: prompt.into(),
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
                    message_id: okbot_core::MessageId::new("p-1"),
                },
            })
            .await;
    }

    async fn finish(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
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

fn show() -> Step {
    Step::Call(
        "ui",
        "show",
        json!({
            "cards": [{"title": "Order #12", "tone": "info", "fields": [{"label": "Total", "value": "120k"}]}],
            "choices": ["✅ Confirm", {"label": "❌ Cancel", "style": "danger"}]
        }),
    )
}

#[tokio::test]
async fn a_press_becomes_the_next_message() {
    let bot = Bot::start(vec![show()], UiCaps::all(), |_| {});
    bot.say("room", "ana", "my order?").await;
    bot.idle().await;

    let turn = &bot.agent.turns()[0];
    assert!(turn.system.contains("<interactive>"), "{}", turn.system);
    assert!(turn.mcp.iter().any(|s| s.name == "ui"));
    assert_eq!(
        bot.agent.results()[0],
        json!("It will be shown after your answer text.")
    );
    let prompts = bot.channel.prompts();
    let [prompt] = prompts.as_slice() else {
        panic!("one prompt: {prompts:?}");
    };
    assert_eq!(prompt.cards[0].title.as_deref(), Some("Order #12"));
    assert!(prompt.cards[0].text.contains("**Total:** 120k"));
    let ids: Vec<&str> = prompt.buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["b1", "b2"]);
    // The history keeps the view as text.
    let key = okbot_testkit::conversation("fake:test", "room");
    let history = bot.store.recent(&key, 10).await.unwrap();
    assert!(
        history[1].text.contains("2. ❌ Cancel"),
        "{}",
        history[1].text
    );

    // Someone else presses first: the first press wins, the second finds nothing.
    bot.press("room", "bob", &prompt.id, "b2").await;
    bot.idle().await;
    eventually(|| bot.agent.turns().len() == 2).await;
    bot.press("room", "ana", &prompt.id, "b1").await;
    bot.idle().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bot.agent.turns().len(), 2, "a closed view answers nothing");

    let second = &bot.agent.turns()[1];
    assert!(second.prompt.contains("→ ❌ Cancel"), "{}", second.prompt);
    let updates = bot.channel.prompt_updates();
    assert!(
        updates
            .iter()
            .any(|(_, text, keep)| text.contains("→ ✅ bob: ❌ Cancel") && !keep),
        "{updates:?}"
    );
    bot.finish().await;
}

#[tokio::test]
async fn text_rooms_and_unknown_views() {
    let bot = Bot::start(vec![Step::List("ui")], UiCaps::all(), |c| {
        c.rooms.insert(
            ("fake:test".parse().unwrap(), "quiet".into()),
            okbot_runtime::config::RoomRule {
                reply_mode: Some(ReplyMode::Text),
                ..Default::default()
            },
        );
    });
    bot.say("quiet", "ana", "hi").await;
    bot.idle().await;
    assert_eq!(bot.agent.results()[0], json!("not offered"));
    assert!(!bot.agent.turns()[0].system.contains("<interactive>"));

    // A press on a view nobody kept does nothing.
    bot.press("quiet", "ana", "v00000000", "b1").await;
    bot.idle().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bot.agent.turns().len(), 1);
    bot.finish().await;
}

#[tokio::test]
async fn platforms_without_buttons_get_text() {
    let bot = Bot::start(vec![show()], UiCaps::default(), |_| {});
    bot.say("room", "ana", "my order?").await;
    bot.idle().await;
    assert_eq!(bot.channel.prompts(), []);
    let sent = bot.channel.sent_texts();
    let view = sent.last().expect("the view as text");
    assert!(
        view.contains("**ℹ️ Order #12**") && view.contains("1. ✅ Confirm"),
        "{view}"
    );
    bot.finish().await;
}

#[tokio::test]
async fn the_agent_hears_what_is_wrong() {
    let bot = Bot::start(
        vec![
            Step::Call(
                "ui",
                "show",
                json!({"form": {"title": "Login", "fields": [{"label": "Password"}]}}),
            ),
            Step::Call("ui", "show", json!({"choices": ["only one"]})),
        ],
        UiCaps::all(),
        |_| {},
    );
    bot.say("room", "ana", "log me in").await;
    bot.idle().await;
    let results = bot.agent.results();
    assert!(
        results[0].as_str().unwrap().contains("never ask"),
        "{results:?}"
    );
    assert!(
        results[1].as_str().unwrap().contains("choices"),
        "{results:?}"
    );
    assert_eq!(bot.channel.prompts(), []);
    bot.finish().await;
}

/// Draws every chart as the same tiny picture, or fails like a chart with no font for its labels.
struct StubCharts {
    fail: bool,
}

impl okbot_core::ChartRenderer for StubCharts {
    fn render(
        &self,
        chart: &okbot_core::Chart,
    ) -> Result<okbot_core::Image, okbot_core::ChartError> {
        if self.fail {
            return Err(okbot_core::ChartError::Glyphs("매".into()));
        }
        Ok(okbot_core::Image {
            name: "chart.png".into(),
            alt: format!("Bar chart of {}", chart.labels.join(", ")),
            width: 1200,
            height: 300,
            png: b"\x89PNG".to_vec(),
        })
    }
}

fn chart_show() -> Step {
    Step::Call(
        "ui",
        "show",
        json!({"cards": [{"title": "Stock", "chart": {"unit": "kg",
            "points": [{"label": "Tea", "value": 3}, {"label": "Rice", "value": 1.5}]}}]}),
    )
}

#[tokio::test]
async fn charts_become_pictures_where_the_platform_shows_them() {
    let charts: Arc<dyn okbot_core::ChartRenderer> = Arc::new(StubCharts { fail: false });
    let bot = Bot::launch(
        vec![chart_show()],
        UiCaps::all(),
        |_| {},
        Some(charts.clone()),
    );
    bot.say("room", "ana", "stock?").await;
    bot.idle().await;
    let prompts = bot.channel.prompts();
    let card = &prompts[0].cards[0];
    let image = card.image.as_ref().expect("a picture");
    assert_eq!(image.alt, "Bar chart of Tea, Rice");
    assert!(!card.text.contains('▇'), "{}", card.text);
    // The history keeps the chart as text for later turns.
    let key = okbot_testkit::conversation("fake:test", "room");
    let history = bot.store.recent(&key, 10).await.unwrap();
    assert!(history[1].text.contains("Tea `▇"), "{}", history[1].text);
    bot.finish().await;

    // A platform without pictures, and a chart that cannot be drawn, show it as text.
    let no_images = UiCaps {
        images: false,
        ..UiCaps::all()
    };
    let bot = Bot::launch(vec![chart_show()], no_images, |_| {}, Some(charts));
    bot.say("room", "ana", "stock?").await;
    bot.idle().await;
    assert!(bot.channel.prompts()[0].cards[0].text.contains("Tea `▇"));
    bot.finish().await;
    let failing: Arc<dyn okbot_core::ChartRenderer> = Arc::new(StubCharts { fail: true });
    let bot = Bot::launch(vec![chart_show()], UiCaps::all(), |_| {}, Some(failing));
    bot.say("room", "ana", "stock?").await;
    bot.idle().await;
    let card = &bot.channel.prompts()[0].cards[0];
    assert!(
        card.image.is_none() && card.text.contains("Tea `▇"),
        "{card:?}"
    );
    bot.finish().await;
}
