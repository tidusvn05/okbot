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
    let channel = a.channel.clone();
    let ttl = Duration::from_secs(60);

    // Dedupe: first sighting is new, repeats are not, expiry makes it new again.
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

    // Reset hides earlier messages of that conversation only.
    store.reset(&a).await.expect("reset");
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
