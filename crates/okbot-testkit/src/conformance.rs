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

use okbot_core::{
    AgentRunRecord, AutomationRun, ChannelId, Clock, ConversationKey, ConversationStore,
    MessageRole, PeerTaskRecord, PeerTaskState, RoleGrant, RunId, TokenUsage, ToolCallRecord,
    ToolDecision, ToolPolicy, Usage, UserId,
};

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
    fires_and_direct_chats(store, clock).await;
    usage_and_tool_calls(store, clock).await;
    role_grants(store, clock).await;
    sealed_secrets(store).await;
    automations(store, clock).await;
    peer_tasks(store, clock).await;
    views(store, clock).await;

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
            person: None,
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

/// Usage per person over a period, and the tool call audit.
async fn usage_and_tool_calls(store: &dyn ConversationStore, clock: &ManualClock) {
    let a = conversation("zalo:shop", "usage");
    let run = |id: &str, person: Option<&str>, tokens: u64| AgentRunRecord {
        run_id: RunId::new(id),
        conversation: a.clone(),
        backend: "fake".into(),
        model: None,
        tools: ToolPolicy::Web,
        usage: Some(TokenUsage {
            input: tokens,
            output: 1,
            ..TokenUsage::default()
        }),
        duration: Duration::from_millis(10),
        error: None,
        started_at: clock.now(),
        person: person.map(str::to_string),
    };
    let since = clock.now();
    store
        .record_run(run("u-0", Some("person:lan"), 99))
        .await
        .expect("record_run");
    clock.advance(Duration::from_secs(10));
    let later = clock.now();
    store
        .record_run(run("u-1", Some("person:lan"), 9))
        .await
        .expect("record_run");
    store
        .record_run(run("u-2", Some("zalo:shop/other"), 5))
        .await
        .expect("record_run");
    store
        .record_run(run("u-3", None, 5))
        .await
        .expect("record_run");
    assert_eq!(
        store.usage_since("person:lan", since).await.expect("usage"),
        Usage {
            turns: 2,
            tokens: 110
        }
    );
    assert_eq!(
        store.usage_since("person:lan", later).await.expect("usage"),
        Usage {
            turns: 1,
            tokens: 10
        }
    );
    assert_eq!(
        store.usage_since("nobody", since).await.expect("usage"),
        Usage::default()
    );

    let call = |tool: &str, decision| ToolCallRecord {
        run_id: Some(RunId::new("u-1")),
        command: None,
        person: "person:lan".into(),
        conversation: a.clone(),
        server: "ops".into(),
        tool: tool.into(),
        decision,
        error: None,
        duration: Duration::from_millis(7),
        at: clock.now(),
    };
    store
        .record_tool_call(call("status", ToolDecision::Allowed))
        .await
        .expect("record_tool_call");
    clock.advance(Duration::from_secs(1));
    let mut denied = call("deploy", ToolDecision::Denied);
    denied.run_id = None;
    denied.command = Some("deploy".into());
    denied.error = Some("not allowed".into());
    store
        .record_tool_call(denied.clone())
        .await
        .expect("record_tool_call");
    let calls = store.tool_calls(since, 10).await.expect("tool_calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], denied, "newest first, read back as written");
    assert_eq!(calls[1].tool, "status");
    assert_eq!(
        store.tool_calls(since, 1).await.expect("tool_calls").len(),
        1
    );
    assert_eq!(
        store
            .tool_calls(clock.now() + jiff::SignedDuration::from_secs(1), 10)
            .await
            .expect("tool_calls"),
        []
    );
}

/// Temporary roles: listed until they expire, replaced by a new grant, revocable.
async fn sealed_secrets(store: &dyn ConversationStore) {
    let get = |subject: &'static str| store.sealed("oauth-token", "notion", subject);
    assert_eq!(get("person:lan").await.expect("sealed"), None);
    store
        .set_sealed("oauth-token", "notion", "person:lan", vec![1, 2])
        .await
        .expect("set");
    store
        .set_sealed("oauth-token", "notion", "person:lan", vec![3])
        .await
        .expect("replace");
    store
        .set_sealed("oauth-client", "notion", "https://as", vec![9])
        .await
        .expect("set");
    assert_eq!(get("person:lan").await.expect("sealed"), Some(vec![3]));
    let entries = store.sealed_entries("oauth-token").await.expect("entries");
    assert_eq!(entries.len(), 1, "one entry per kind, server and subject");
    assert_eq!(
        (entries[0].server.as_str(), entries[0].subject.as_str()),
        ("notion", "person:lan")
    );
    assert!(
        store
            .delete_sealed("oauth-token", "notion", "person:lan")
            .await
            .expect("delete")
    );
    assert!(
        !store
            .delete_sealed("oauth-token", "notion", "person:lan")
            .await
            .expect("delete")
    );
    assert_eq!(
        store
            .sealed("oauth-client", "notion", "https://as")
            .await
            .expect("sealed"),
        Some(vec![9]),
        "other kinds stay"
    );
}

async fn automations(store: &dyn ConversationStore, clock: &ManualClock) {
    let items = |ids: &[&str]| ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    let new = store
        .claim_items("mail", items(&["a", "b"]))
        .await
        .expect("claim");
    assert_eq!(new, ["a", "b"]);
    let new = store
        .claim_items("mail", items(&["b", "c", "a"]))
        .await
        .expect("claim");
    assert_eq!(new, ["c"], "only unseen items, in order");
    let other = store
        .claim_items("other", items(&["a"]))
        .await
        .expect("claim");
    assert_eq!(other, ["a"], "each automation has its own items");
    store
        .release_items("mail", items(&["c", "zz"]))
        .await
        .expect("release");
    let again = store
        .claim_items("mail", items(&["a", "c"]))
        .await
        .expect("claim");
    assert_eq!(again, ["c"], "released items are new again");
    let run = |name: &str, items: u32| AutomationRun {
        automation: name.into(),
        at: clock.now(),
        items,
        action: "text".into(),
        error: None,
    };
    store
        .record_automation_run(run("mail", 2))
        .await
        .expect("record");
    clock.advance(Duration::from_secs(60));
    store
        .record_automation_run(AutomationRun {
            error: Some("boom".into()),
            ..run("mail", 0)
        })
        .await
        .expect("record");
    store
        .record_automation_run(run("other", 1))
        .await
        .expect("record");
    let since = clock.now() - jiff::SignedDuration::from_secs(120);
    let mail = store
        .automation_runs(Some("mail"), since, 10)
        .await
        .expect("runs");
    assert_eq!(mail.len(), 2);
    assert_eq!(mail[0].error.as_deref(), Some("boom"), "newest first");
    let all = store.automation_runs(None, since, 2).await.expect("runs");
    assert_eq!(all.len(), 2, "limited");
    let recent = store
        .automation_runs(None, clock.now(), 10)
        .await
        .expect("runs");
    assert_eq!(recent.len(), 2, "only runs since the given time");
}

async fn peer_tasks(store: &dyn ConversationStore, clock: &ManualClock) {
    let task = |id: &str, inbound: bool, message: &str| PeerTaskRecord {
        id: id.into(),
        inbound,
        team: "acme".into(),
        peer: "lead".into(),
        context_id: "ctx".into(),
        message_id: message.into(),
        state: PeerTaskState::Working,
        conversation: Some(conversation("peer:acme", "lead/ctx")),
        trace: "t1".into(),
        text: None,
        requester: (!inbound).then(|| okbot_core::UserRef {
            id: "lan".into(),
            display_name: "Lan".into(),
            is_bot: false,
        }),
        direct: !inbound,
        tokens: None,
        created_at: clock.now(),
        updated_at: clock.now(),
    };
    store
        .put_peer_task(task("t-1", true, "m-1"))
        .await
        .expect("put");
    store
        .put_peer_task(task("t-1", false, "m-1"))
        .await
        .expect("each direction has its own ids");
    let found = store
        .peer_task_by_message(true, "acme", "lead", "m-1")
        .await
        .expect("by message")
        .expect("found");
    assert_eq!(found.id, "t-1");
    assert_eq!(
        found.conversation,
        Some(conversation("peer:acme", "lead/ctx"))
    );
    assert!(
        store.put_peer_task(task("t-2", true, "m-1")).await.is_err(),
        "one task per message"
    );
    clock.advance(Duration::from_secs(5));
    store
        .put_peer_task(PeerTaskRecord {
            state: PeerTaskState::Completed,
            text: Some("done".into()),
            tokens: Some(1200),
            updated_at: clock.now(),
            ..task("t-1", false, "m-1")
        })
        .await
        .expect("update");
    let out = store
        .peer_task(false, "t-1")
        .await
        .expect("get")
        .expect("found");
    assert_eq!(
        (out.state, out.text.as_deref()),
        (PeerTaskState::Completed, Some("done"))
    );
    assert_eq!(out.requester.as_ref().map(|r| r.id.as_str()), Some("lan"));
    assert!(out.direct);
    assert_eq!(out.tokens, Some(1200));
    assert_eq!(store.peer_tasks(10).await.expect("list")[0].id, "t-1");
    assert!(
        !store.peer_tasks(10).await.expect("list")[0].inbound,
        "newest first"
    );
    assert_eq!(
        store
            .interrupt_peer_tasks(clock.now())
            .await
            .expect("interrupt"),
        1
    );
    let failed = store
        .peer_task(true, "t-1")
        .await
        .expect("get")
        .expect("found");
    assert_eq!(failed.state, PeerTaskState::Failed);
    assert_eq!(
        store
            .peer_task(false, "t-1")
            .await
            .expect("get")
            .expect("found")
            .state,
        PeerTaskState::Completed,
        "outbound tasks are not interrupted"
    );
    assert!(store.peer_task(true, "nope").await.expect("get").is_none());
}

async fn role_grants(store: &dyn ConversationStore, clock: &ManualClock) {
    let grant = |role: &str, account: &str, secs: i64| RoleGrant {
        role: role.into(),
        account: account.into(),
        granted_by: "operator".into(),
        granted_at: clock.now(),
        expires_at: clock.now() + jiff::SignedDuration::from_secs(secs),
    };
    store
        .grant_role(grant("ops", "zalo:shop/u1", 60))
        .await
        .expect("grant");
    store
        .grant_role(grant("ops", "zalo:shop/u2", 600))
        .await
        .expect("grant");
    store
        .grant_role(grant("ops", "zalo:shop/u1", 120))
        .await
        .expect("grant again");
    let now = clock.now();
    let grants = store.role_grants(now).await.expect("grants");
    assert_eq!(grants.len(), 2, "one grant per role and account");
    assert_eq!(grants[0].account, "zalo:shop/u1");
    assert_eq!(
        grants[0].expires_at,
        now + jiff::SignedDuration::from_secs(120)
    );
    assert!(
        store
            .revoke_role("ops", "zalo:shop/u2")
            .await
            .expect("revoke")
    );
    assert!(
        !store
            .revoke_role("ops", "zalo:shop/u2")
            .await
            .expect("revoke")
    );
    clock.advance(Duration::from_secs(121));
    assert!(
        store
            .role_grants(clock.now())
            .await
            .expect("grants")
            .is_empty(),
        "expired grants are gone"
    );
    assert!(
        !store
            .revoke_role("ops", "zalo:shop/u1")
            .await
            .expect("revoke"),
        "an expired grant is not revoked"
    );
}

/// Dedupe: first sighting is new, repeats are not, expiry makes it new again; per channel.
/// Interactive replies (ADR 0015): kept until they expire, taken once.
async fn views(store: &dyn ConversationStore, clock: &ManualClock) {
    let now = clock.now();
    let later = now + jiff::SignedDuration::from_secs(60);
    store.put_view("v1", "{}".into(), later).await.expect("put");
    store.put_view("v2", "[]".into(), later).await.expect("put");
    assert!(
        store.put_view("v2", "{}".into(), later).await.is_err(),
        "a view never replaces another"
    );
    assert_eq!(
        store.view("v1", now).await.expect("view").as_deref(),
        Some("{}")
    );
    assert_eq!(
        store.take_view("v1", now).await.expect("take").as_deref(),
        Some("{}")
    );
    assert_eq!(
        store.take_view("v1", now).await.expect("take"),
        None,
        "taken once"
    );
    assert_eq!(
        store.view("v2", later).await.expect("view"),
        None,
        "expired"
    );
    assert_eq!(store.take_view("v2", later).await.expect("take"), None);
}

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

async fn fires_and_direct_chats(store: &dyn ConversationStore, clock: &ManualClock) {
    use okbot_core::FireState;
    let at = clock.now();
    let first = store.claim_fire("tasks/x/t-1", at).await.expect("claim");
    assert_eq!((first.state, first.attempts), (FireState::Sending, 0));
    store
        .set_fire("tasks/x/t-1", at, FireState::Sending, 2)
        .await
        .expect("set_fire");
    let again = store.claim_fire("tasks/x/t-1", at).await.expect("claim");
    assert_eq!(again.attempts, 2, "a claim keeps the existing record");
    store
        .set_fire("tasks/x/t-1", at, FireState::Sent, 3)
        .await
        .expect("set_fire");
    let sent = store.claim_fire("tasks/x/t-1", at).await.expect("claim");
    assert_eq!(sent.state, FireState::Sent, "a sent firing stays sent");
    let other = store
        .claim_fire("tasks/x/t-1", at + jiff::SignedDuration::from_hours(24))
        .await
        .expect("claim");
    assert_eq!(
        other.state,
        FireState::Sending,
        "each firing time is its own record"
    );
    let recent = store.recent_fires(10).await.expect("recent_fires");
    assert_eq!(recent.len(), 2);

    let channel: ChannelId = "discord:main".parse().expect("valid id");
    let user = UserId::new("u1");
    assert_eq!(
        store
            .direct_chat(&channel, &user)
            .await
            .expect("direct_chat"),
        None
    );
    let dm = ConversationKey::room(channel.clone(), "dm-1");
    store
        .set_direct_chat(&channel, &user, &dm)
        .await
        .expect("set_direct_chat");
    let newer = ConversationKey::room(channel.clone(), "dm-2");
    store
        .set_direct_chat(&channel, &user, &newer)
        .await
        .expect("set_direct_chat");
    assert_eq!(
        store
            .direct_chat(&channel, &user)
            .await
            .expect("direct_chat"),
        Some(newer)
    );
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
