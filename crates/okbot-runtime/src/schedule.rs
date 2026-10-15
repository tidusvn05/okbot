//! The reminder scheduler (design §5.6): finds due task firings, sends them once, and moves each
//! task to its next firing.

use std::sync::Arc;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use okbot_core::{
    CancellationToken, ChannelId, ConversationKey, FireState, MessageRole, NewMessage, ReplyTarget,
    Task, TaskChange, TaskKind, TaskStatus, UserId,
};

use crate::Shared;

/// Delivery attempts before a firing is given up.
pub(crate) const MAX_ATTEMPTS: u32 = 5;

/// Wait before retry `attempts` (30 s, 1 min, 2 min, 4 min).
fn backoff(attempts: u32) -> SignedDuration {
    SignedDuration::from_secs(30 * (1_i64 << attempts.saturating_sub(1).min(4)))
}

/// Runs until `stop`: one pass, then a sleep until the next firing or the rescan interval.
pub(crate) async fn run(shared: Arc<Shared>, stop: CancellationToken) {
    let rescan = shared.config.schedule.rescan;
    tracing::info!("reminder scheduler started");
    loop {
        let next = tick(&shared).await;
        let now = shared.clock.now();
        let wait = next
            .and_then(|at| Duration::try_from(at.duration_since(now)).ok())
            .map_or(rescan, |d| d.max(Duration::from_millis(200)).min(rescan));
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// One pass: fires every due task and returns the next firing time, if any.
pub(crate) async fn tick(s: &Shared) -> Option<Timestamp> {
    let store = s.tasks.as_ref()?;
    let tasks = match store.scheduled().await {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::error!(%error, "cannot read the scheduled tasks");
            return None;
        }
    };
    let now = s.clock.now();
    let mut next: Option<Timestamp> = None;
    for task in tasks {
        let Some(at) = task.remind_at else { continue };
        if task.status != TaskStatus::Open {
            continue;
        }
        if at > now {
            next = Some(next.map_or(at, |n| n.min(at)));
            continue;
        }
        if task.kind == TaskKind::Routine {
            // Routines run agent turns (design §5.7); they cannot be created yet.
            continue;
        }
        fire(s, &task, at, now).await;
    }
    next
}

async fn fire(s: &Shared, task: &Task, at: Timestamp, now: Timestamp) {
    let span = tracing::info_span!("reminder", task = %task.id, %at);
    let _enter = span.enter();
    let fire = match s.store.claim_fire(&task.id, at).await {
        Ok(fire) => fire,
        Err(error) => {
            tracing::error!(%error, "cannot record the firing");
            return;
        }
    };
    if fire.state.is_final() {
        // Sent (or given up) before the task file was moved on: only move it on now.
        advance(s, task, at, now).await;
        return;
    }
    if fire.attempts > 0 && now < fire.updated_at + backoff(fire.attempts) {
        return;
    }
    let late = now.duration_since(at);
    if late > SignedDuration::try_from(s.config.schedule.late_grace).unwrap_or(SignedDuration::MAX)
    {
        tracing::warn!(late = %late, "reminder missed (the bot was not running)");
        finish(s, task, at, FireState::Missed, fire.attempts).await;
        advance(s, task, at, now).await;
        return;
    }
    let attempts = fire.attempts + 1;
    match deliver(s, task, at, now).await {
        Ok(()) => {
            tracing::info!("reminder sent");
            finish(s, task, at, FireState::Sent, attempts).await;
            advance(s, task, at, now).await;
        }
        Err(reason) if attempts >= MAX_ATTEMPTS => {
            tracing::error!(%reason, attempts, "giving up on a reminder");
            finish(s, task, at, FireState::Failed, attempts).await;
            advance(s, task, at, now).await;
        }
        Err(reason) => {
            tracing::warn!(%reason, attempts, "reminder not sent; will retry");
            finish(s, task, at, FireState::Sending, attempts).await;
        }
    }
}

async fn finish(s: &Shared, task: &Task, at: Timestamp, state: FireState, attempts: u32) {
    if let Err(error) = s.store.set_fire(&task.id, at, state, attempts).await {
        tracing::error!(%error, "cannot record the firing");
    }
}

/// Moves a fired task on: the next occurrence of a repeating task, `done` for a one-shot
/// reminder, no reminder time for a to-do.
async fn advance(s: &Shared, task: &Task, at: Timestamp, now: Timestamp) {
    let Some(store) = &s.tasks else { return };
    let change = match &task.repeat {
        // Occurrences missed while the bot was down are skipped.
        Some(repeat) => TaskChange {
            remind_at: Some(repeat.next_after(at.max(now), &s.config.time_zone())),
            ..TaskChange::default()
        },
        None if task.kind == TaskKind::Reminder => TaskChange {
            status: Some(TaskStatus::Done),
            remind_at: Some(None),
            ..TaskChange::default()
        },
        None => TaskChange {
            remind_at: Some(None),
            ..TaskChange::default()
        },
    };
    if let Err(error) = store.after_fire(&task.id, change).await {
        tracing::error!(%error, "cannot move the task to its next firing");
    }
}

/// The accounts of the person or user a task belongs to (none for a room's task).
async fn accounts(s: &Shared, task: &Task) -> Vec<(ChannelId, UserId)> {
    let parts: Vec<&str> = task.id.split('/').collect();
    match parts.as_slice() {
        ["tasks", "people", person, ..] => {
            if s.config.personal && *person == crate::config::OWNER {
                s.owners().await
            } else {
                s.config
                    .people
                    .values()
                    .find(|p| p.id == *person)
                    .map(|p| p.accounts.clone())
                    .unwrap_or_default()
            }
        }
        ["tasks", "users", ..] => task
            .author
            .split_once('/')
            .and_then(|(channel, user)| Some((channel.parse().ok()?, UserId::new(user))))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// The direct chat of one of `accounts` on `channel`, if the bot has seen one.
async fn direct_on(
    s: &Shared,
    accounts: &[(ChannelId, UserId)],
    channel: &ChannelId,
) -> Option<ConversationKey> {
    for (c, user) in accounts.iter().filter(|(c, _)| c == channel) {
        match s.store.direct_chat(c, user).await {
            Ok(Some(key)) => return Some(key),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "cannot read the direct chats"),
        }
    }
    None
}

/// Where a task's reminder goes: its `deliver_to` channel, else the chat it was created in, else
/// the owner's direct chat on `[schedule] deliver_to` or on any running channel.
async fn target(s: &Shared, task: &Task) -> Result<ConversationKey, String> {
    let running = |c: &ChannelId| s.channels.contains_key(c);
    if let Some(channel) = &task.deliver_to {
        let accounts = accounts(s, task).await;
        return direct_on(s, &accounts, channel)
            .await
            .ok_or_else(|| format!("no direct chat with the owner on {channel} yet"));
    }
    if let Some(origin) = task.origin.as_ref().filter(|o| running(&o.channel)) {
        return Ok(origin.clone());
    }
    let accounts = accounts(s, task).await;
    let mut channels: Vec<ChannelId> = s.config.schedule.deliver_to.iter().cloned().collect();
    let mut others: Vec<ChannelId> = s.channels.keys().cloned().collect();
    others.sort();
    channels.extend(others);
    for channel in channels.iter().filter(|c| running(c)) {
        if let Some(key) = direct_on(s, &accounts, channel).await {
            return Ok(key);
        }
    }
    Err("no chat to send it to: the owner has not written to the bot in a direct chat".into())
}

/// The reminder as sent: the planned time, the title and the notes.
pub(crate) fn reminder_text(task: &Task, at: Timestamp, now: Timestamp, s: &Shared) -> String {
    let tz = s.config.time_zone();
    let local = at.to_zoned(tz.clone());
    let when = if local.date() == now.to_zoned(tz).date() {
        local.strftime("%H:%M").to_string()
    } else {
        local.strftime("%d/%m %H:%M").to_string()
    };
    let mut text = format!("⏰ {when} · **{}**", task.title);
    if !task.notes.is_empty() {
        text.push('\n');
        text.push_str(&task.notes);
    }
    text
}

async fn deliver(s: &Shared, task: &Task, at: Timestamp, now: Timestamp) -> Result<(), String> {
    let conversation = target(s, task).await?;
    let channel = s
        .channels
        .get(&conversation.channel)
        .ok_or_else(|| format!("channel {} is not running", conversation.channel))?;
    let text = reminder_text(task, at, now, s);
    let to = ReplyTarget {
        conversation: conversation.clone(),
        in_reply_to: None,
    };
    for part in s.renderer.render(&text, channel.caps()) {
        channel
            .send(&to, &part)
            .await
            .map_err(|e| format!("sending failed: {e}"))?;
    }
    // In the history, so a reply such as "in 10 minutes" reaches an agent that knows the task.
    let short = task.id.rsplit('/').next().unwrap_or(&task.id);
    let record = NewMessage {
        conversation,
        role: MessageRole::Bot,
        author_id: None,
        author_name: s.config.bot_name.clone(),
        text: format!("{text}\n\n(reminder `{short}`; the user may snooze it or mark it done)"),
        message_id: None,
        at: now,
    };
    if let Err(error) = s.store.append(record).await {
        tracing::warn!(%error, "cannot add the reminder to the history");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use okbot_core::{Channel, ChannelCaps, Clock, MemoryScope, Repeat, TaskStore};
    use okbot_testkit::{
        FakeAgent, FakeChannel, ManualClock, MemoryStore, MemoryTasks, PlainRenderer,
    };

    use super::*;
    use crate::config::RuntimeConfig;
    use crate::policy::Gate;

    struct Setup {
        shared: Shared,
        clock: Arc<ManualClock>,
        tasks: Arc<MemoryTasks>,
        zalo: Arc<FakeChannel>,
        discord: Arc<FakeChannel>,
    }

    fn setup(personal: bool) -> Setup {
        let clock = Arc::new(ManualClock::default());
        let tasks = Arc::new(MemoryTasks::new(clock.clone()));
        let zalo = Arc::new(FakeChannel::new("zalo:me", ChannelCaps::basic()));
        let discord = Arc::new(FakeChannel::new("discord:main", ChannelCaps::basic()));
        let mut config = RuntimeConfig::new("okbot", std::env::temp_dir());
        config.personal = personal;
        config.timezone = Some("Asia/Ho_Chi_Minh".into());
        let channels: HashMap<ChannelId, Arc<dyn Channel>> = HashMap::from([
            (zalo.id().clone(), zalo.clone() as Arc<dyn Channel>),
            (discord.id().clone(), discord.clone() as Arc<dyn Channel>),
        ]);
        let shared = Shared {
            gate: Gate::new(config.access.clone()),
            config,
            store: Arc::new(MemoryStore::new(clock.clone())),
            agent: Arc::new(FakeAgent::echo("fake")),
            knowledge: None,
            memory: None,
            tasks: Some(tasks.clone()),
            renderer: Arc::new(PlainRenderer),
            clock: clock.clone(),
            channels,
            in_flight: Arc::new(crate::InFlight::default()),
            metrics: crate::metrics::Metrics::default(),
        };
        Setup {
            shared,
            clock,
            tasks,
            zalo,
            discord,
        }
    }

    fn task(id: &str, kind: TaskKind, at: Timestamp, origin: Option<ConversationKey>) -> Task {
        Task {
            id: format!("tasks/people/owner/{id}"),
            kind,
            status: TaskStatus::Open,
            title: "Gọi mẹ".into(),
            notes: String::new(),
            scope: MemoryScope::User,
            due: None,
            remind_at: Some(at),
            repeat: None,
            deliver_to: None,
            origin,
            author: "zalo:me/u1".into(),
            created: at,
        }
    }

    fn mins(n: i64) -> SignedDuration {
        SignedDuration::from_mins(n)
    }

    #[tokio::test]
    async fn sends_once_on_time_and_closes_a_reminder() {
        let t = setup(true);
        let now = t.clock.now();
        let origin = ConversationKey::room("zalo:me".parse().unwrap(), "dm-u1");
        t.tasks.insert(task(
            "t-1",
            TaskKind::Reminder,
            now + mins(5),
            Some(origin.clone()),
        ));
        assert_eq!(tick(&t.shared).await, Some(now + mins(5)));
        assert_eq!(t.zalo.sent_texts(), Vec::<String>::new());

        t.clock.advance(Duration::from_secs(300));
        assert_eq!(tick(&t.shared).await, None);
        // 2026-01-01 00:05Z is 07:05 in Ho Chi Minh City.
        assert_eq!(t.zalo.sent_texts(), ["⏰ 07:05 · **Gọi mẹ**"]);
        assert_eq!(t.zalo.sent()[0].0.conversation, origin);
        let done = &t.tasks.all()[0];
        assert_eq!((done.status, done.remind_at), (TaskStatus::Done, None));
        let history = t.shared.store.recent(&origin, 5).await.unwrap();
        assert!(history[0].text.contains("reminder `t-1`"), "{history:?}");

        tick(&t.shared).await;
        assert_eq!(t.zalo.sent_texts().len(), 1, "sent once");
    }

    #[tokio::test]
    async fn repeats_skip_missed_occurrences_and_todos_stay_open() {
        let t = setup(true);
        let now = t.clock.now();
        let origin = ConversationKey::room("zalo:me".parse().unwrap(), "dm-u1");
        let mut daily = task(
            "t-1",
            TaskKind::Reminder,
            now - mins(60),
            Some(origin.clone()),
        );
        daily.repeat = Some("daily 07:00".parse::<Repeat>().unwrap());
        t.tasks.insert(daily);
        let todo = task("t-2", TaskKind::Todo, now - mins(1), Some(origin));
        t.tasks.insert(todo);
        tick(&t.shared).await;
        assert_eq!(
            t.zalo.sent_texts().len(),
            2,
            "an hour late is within the grace"
        );
        let all = t.tasks.all();
        let daily = all.iter().find(|t| t.id.ends_with("t-1")).unwrap();
        // Now is 07:00 local, so the next 07:00 is tomorrow's.
        assert_eq!(
            daily.remind_at,
            Some("2026-01-02T00:00:00Z".parse().unwrap())
        );
        assert_eq!(daily.status, TaskStatus::Open);
        let todo = all.iter().find(|t| t.id.ends_with("t-2")).unwrap();
        assert_eq!((todo.status, todo.remind_at), (TaskStatus::Open, None));
    }

    #[tokio::test]
    async fn too_late_is_missed_and_crash_after_send_is_not_resent() {
        let t = setup(true);
        let now = t.clock.now();
        let origin = ConversationKey::room("zalo:me".parse().unwrap(), "dm-u1");
        t.tasks.insert(task(
            "t-1",
            TaskKind::Reminder,
            now - mins(7 * 60),
            Some(origin.clone()),
        ));
        tick(&t.shared).await;
        assert_eq!(t.zalo.sent_texts(), Vec::<String>::new());
        let fires = t.shared.store.recent_fires(5).await.unwrap();
        assert_eq!(fires[0].state, FireState::Missed);

        // A firing already sent whose file was not moved on (crash): only the file moves.
        let at = now - mins(1);
        t.tasks
            .insert(task("t-2", TaskKind::Reminder, at, Some(origin)));
        t.shared
            .store
            .set_fire("tasks/people/owner/t-2", at, FireState::Sent, 1)
            .await
            .unwrap();
        tick(&t.shared).await;
        assert_eq!(t.zalo.sent_texts(), Vec::<String>::new());
        assert!(t.tasks.all().iter().all(|t| t.status == TaskStatus::Done));
    }

    #[tokio::test]
    async fn terminal_tasks_go_to_the_owners_direct_chat() {
        let t = setup(true);
        let now = t.clock.now();
        let discord: ChannelId = "discord:main".parse().unwrap();
        let console = ConversationKey::room("console:cli".parse().unwrap(), "terminal");
        t.tasks.insert(task(
            "t-1",
            TaskKind::Reminder,
            now - mins(1),
            Some(console),
        ));

        // No owner and no direct chat yet: retried with backoff, then given up.
        tick(&t.shared).await;
        let fires = t.shared.store.recent_fires(5).await.unwrap();
        assert_eq!((fires[0].state, fires[0].attempts), (FireState::Sending, 1));
        tick(&t.shared).await;
        assert_eq!(
            t.shared.store.recent_fires(5).await.unwrap()[0].attempts,
            1,
            "waits for the backoff"
        );

        // The owner paired on Discord and wrote in a direct chat.
        let code = "123456";
        t.shared
            .store
            .create_pairing_code(code, Duration::from_secs(600))
            .await
            .unwrap();
        let user = UserId::new("d1");
        assert!(
            t.shared
                .store
                .redeem_pairing_code(code, &discord, &user)
                .await
                .unwrap()
        );
        let dm = ConversationKey::room(discord.clone(), "dm-d1");
        t.shared
            .store
            .set_direct_chat(&discord, &user, &dm)
            .await
            .unwrap();
        t.clock.advance(Duration::from_secs(31));
        tick(&t.shared).await;
        assert_eq!(t.discord.sent_texts().len(), 1);
        assert_eq!(t.discord.sent()[0].0.conversation, dm);

        // deliver_to picks that channel even when the task was made in another chat.
        let mut to_discord = task(
            "t-3",
            TaskKind::Reminder,
            t.clock.now() - mins(1),
            Some(ConversationKey::room("zalo:me".parse().unwrap(), "dm-u1")),
        );
        to_discord.deliver_to = Some(discord);
        t.tasks.insert(to_discord);
        tick(&t.shared).await;
        assert_eq!(t.discord.sent_texts().len(), 2);
        assert_eq!(t.zalo.sent_texts(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let t = setup(false);
        let gone = ConversationKey::room("slack:old".parse().unwrap(), "c1");
        t.tasks.insert(task(
            "t-1",
            TaskKind::Reminder,
            t.clock.now() - mins(1),
            Some(gone),
        ));
        for _ in 0..MAX_ATTEMPTS {
            tick(&t.shared).await;
            t.clock.advance(Duration::from_secs(600));
        }
        let fires = t.shared.store.recent_fires(5).await.unwrap();
        assert_eq!(
            (fires[0].state, fires[0].attempts),
            (FireState::Failed, MAX_ATTEMPTS)
        );
        assert_eq!(t.tasks.all()[0].status, TaskStatus::Done);
        // The store is only used through the port.
        let _: &dyn TaskStore = t.tasks.as_ref();
    }
}
