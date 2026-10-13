//! Conformance suites: call them from an implementation's tests.
//!
//! ```ignore
//! #[tokio::test]
//! async fn conformance() {
//!     let clock = Arc::new(ManualClock::default());
//!     let store = MyStore::new(clock.clone());
//!     okbot_testkit::conformance::conversation_store(&store, &clock).await;
//! }
//! ```

use std::time::Duration;

use okbot_core::{AgentRunRecord, Clock, ConversationStore, MessageRole, RunId, ToolPolicy};

use crate::{ManualClock, bot_message, conversation, user_message};

/// Checks a fresh, empty [`ConversationStore`] that reads time from `clock`.
///
/// # Panics
/// On the first behaviour that differs from the port's contract.
pub async fn conversation_store(store: &dyn ConversationStore, clock: &ManualClock) {
    let a = conversation("zalo:shop", "room-a");
    let b = conversation("zalo:shop", "room-b");
    dedupe(store, clock).await;
    pairing(store, clock).await;

    // History: oldest first, limited to the newest, separated by conversation.
    for i in 0..5 {
        store
            .append(user_message(&a, &format!("q{i}"), clock.now()))
            .await
            .expect("append");
        store
            .append(bot_message(&a, &format!("r{i}"), clock.now()))
            .await
            .expect("append");
    }
    store
        .append(user_message(&b, "other room", clock.now()))
        .await
        .expect("append");
    let recent = store.recent(&a, 3).await.expect("recent");
    let texts: Vec<&str> = recent.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["r3", "q4", "r4"]);
    assert_eq!(recent[2].role, MessageRole::Bot);
    assert_eq!(store.recent(&b, 10).await.expect("recent").len(), 1);
    assert_eq!(
        store
            .recent(&conversation("zalo:shop", "none"), 10)
            .await
            .expect("recent"),
        []
    );

    // Sessions: stored per conversation, cleared by reset.
    assert_eq!(store.session(&a).await.expect("session"), None);
    store
        .set_session(&a, "claude", "s-1")
        .await
        .expect("set_session");
    store
        .set_session(&a, "claude", "s-2")
        .await
        .expect("set_session");
    assert_eq!(
        store.session(&a).await.expect("session"),
        Some(("claude".into(), "s-2".into()))
    );
    assert_eq!(store.session(&b).await.expect("session"), None);

    // Reset hides earlier messages of that conversation only.
    store.reset(&a).await.expect("reset");
    assert_eq!(
        store.session(&a).await.expect("session"),
        None,
        "reset forgets the session"
    );
    assert_eq!(store.recent(&a, 10).await.expect("recent"), []);
    store
        .append(user_message(&a, "after reset", clock.now()))
        .await
        .expect("append");
    assert_eq!(store.recent(&a, 10).await.expect("recent").len(), 1);
    assert_eq!(store.recent(&b, 10).await.expect("recent").len(), 1);

    // Audit records are accepted.
    store
        .record_run(AgentRunRecord {
            run_id: RunId::new("run-1"),
            conversation: a.clone(),
            backend: "fake".into(),
            model: None,
            tools: ToolPolicy::Web,
            usage: None,
            duration: Duration::from_millis(1500),
            error: None,
            started_at: clock.now(),
        })
        .await
        .expect("record_run");

    // Retention removes old messages.
    clock.advance(Duration::from_secs(3600));
    store
        .append(user_message(&b, "new", clock.now()))
        .await
        .expect("append");
    let cutoff = clock
        .now()
        .checked_sub(jiff::SignedDuration::from_secs(60))
        .expect("in range");
    let removed = store.purge_before(cutoff).await.expect("purge");
    assert!(removed >= 1, "purge removed nothing");
    let left: Vec<String> = store
        .recent(&b, 10)
        .await
        .expect("recent")
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(left, ["new"]);
}

/// Dedupe: first sighting is new, repeats are not, expiry makes it new again; per channel.
async fn dedupe(store: &dyn ConversationStore, clock: &ManualClock) {
    let channel: okbot_core::ChannelId = "zalo:shop".parse().expect("id");
    let ttl = Duration::from_secs(60);
    assert!(
        store
            .mark_seen(&channel, "e1", ttl)
            .await
            .expect("mark_seen")
    );
    assert!(
        !store
            .mark_seen(&channel, "e1", ttl)
            .await
            .expect("mark_seen")
    );
    let other: okbot_core::ChannelId = "zalo:other".parse().expect("id");
    assert!(
        store.mark_seen(&other, "e1", ttl).await.expect("mark_seen"),
        "dedupe is per channel"
    );
    clock.advance(Duration::from_secs(61));
    assert!(
        store
            .mark_seen(&channel, "e1", ttl)
            .await
            .expect("mark_seen"),
        "expired mark"
    );
}

/// Checks the outbound side of a running [`Channel`](okbot_core::Channel): call it while
/// [`Channel::run`](okbot_core::Channel::run) is serving (against your fake platform), with a
/// target in a conversation that channel owns.
///
/// It checks the capabilities are usable, that `send` returns a reference to what it sent, that a
/// message of exactly `max_message_len` is accepted, and that `edit` and `typing` agree with the
/// capabilities.
///
/// # Panics
/// On the first behaviour that differs from the port's contract.
pub async fn channel(channel: &dyn okbot_core::Channel, to: &okbot_core::ReplyTarget) {
    use okbot_core::{ChannelError, RenderedMessage};

    let caps = channel.caps().clone();
    assert!(caps.max_message_len > 0, "max_message_len must be positive");
    assert_eq!(
        &to.conversation.channel,
        channel.id(),
        "the target belongs to this channel"
    );

    let sent = channel
        .send(
            to,
            &RenderedMessage {
                text: "conformance: hello".into(),
            },
        )
        .await
        .expect("send");
    assert_eq!(&sent.target, to, "send returns the target it sent to");
    assert!(
        !sent.message_id.as_str().is_empty(),
        "send returns the platform message id"
    );

    let longest = RenderedMessage {
        text: "x".repeat(caps.max_message_len),
    };
    channel
        .send(to, &longest)
        .await
        .expect("a message of exactly max_message_len is accepted");

    let edited = channel
        .edit(
            &sent,
            &RenderedMessage {
                text: "conformance: edited".into(),
            },
        )
        .await;
    match (caps.edit, edited) {
        (true, Ok(())) | (false, Err(ChannelError::Unsupported(_))) => {}
        (true, Err(e)) => panic!("caps.edit is set but edit failed: {e}"),
        (false, other) => panic!("caps.edit is unset, so edit must be Unsupported: {other:?}"),
    }

    // Typing is best effort: it never fails a turn, even where the platform has none.
    channel
        .typing(to)
        .await
        .expect("typing succeeds (a no-op when unsupported)");
}

/// Checks that [`Channel::run`](okbot_core::Channel::run) returns soon after shutdown.
///
/// # Panics
/// When `run` fails or is still running 10 seconds after the token is cancelled.
pub async fn channel_stops(
    channel: std::sync::Arc<dyn okbot_core::Channel>,
    sink: std::sync::Arc<dyn okbot_core::InboundSink>,
    shutdown: okbot_core::CancellationToken,
) {
    let run = tokio::spawn({
        let shutdown = shutdown.clone();
        async move { channel.run(sink, shutdown).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("run returns within 10 s of shutdown")
        .expect("run does not panic")
        .expect("run returns Ok on shutdown");
}

async fn pairing(store: &dyn ConversationStore, clock: &ManualClock) {
    let zalo: okbot_core::ChannelId = "zalo:shop".parse().expect("valid id");
    let discord: okbot_core::ChannelId = "discord:shop".parse().expect("valid id");
    let me = okbot_core::UserId::new("me");
    assert_eq!(store.owners().await.expect("owners"), vec![]);

    store
        .create_pairing_code("111111", Duration::from_secs(600))
        .await
        .expect("create");
    assert!(
        store
            .redeem_pairing_code("111111", &zalo, &me)
            .await
            .expect("redeem"),
        "a fresh code pairs"
    );
    assert!(
        !store
            .redeem_pairing_code("111111", &discord, &me)
            .await
            .expect("redeem"),
        "codes are single-use"
    );
    assert_eq!(
        store.owners().await.expect("owners"),
        vec![(zalo.clone(), me.clone())]
    );

    // Expired codes do not pair.
    store
        .create_pairing_code("222222", Duration::from_secs(60))
        .await
        .expect("create");
    clock.advance(Duration::from_secs(61));
    assert!(
        !store
            .redeem_pairing_code("222222", &discord, &me)
            .await
            .expect("redeem")
    );

    // Too many wrong guesses cancel pending codes.
    store
        .create_pairing_code("333333", Duration::from_secs(600))
        .await
        .expect("create");
    for _ in 0..okbot_core::PAIRING_MAX_FAILURES {
        assert!(
            !store
                .redeem_pairing_code("000000", &discord, &me)
                .await
                .expect("redeem")
        );
    }
    assert!(
        !store
            .redeem_pairing_code("333333", &discord, &me)
            .await
            .expect("redeem"),
        "cancelled after too many failures"
    );

    assert!(store.remove_owner(&zalo, &me).await.expect("remove"));
    assert!(!store.remove_owner(&zalo, &me).await.expect("remove"));
    assert_eq!(store.owners().await.expect("owners"), vec![]);
}
