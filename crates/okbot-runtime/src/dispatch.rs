//! The dispatcher: per-event checks, commands, and routing to conversation workers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use okbot_core::{CancellationToken, ConversationKey, InboundEvent, ReplyTarget};
use tokio::sync::mpsc;

use crate::command::{self, Command};
use crate::interact::Choice;
use crate::policy::Verdict;
use crate::{Shared, Tracked, turn};

/// A conversation worker's mailbox and its running turn.
struct Worker {
    tx: mpsc::UnboundedSender<Tracked>,
    current: Arc<Mutex<Option<CancellationToken>>>,
}

type Workers = Arc<Mutex<HashMap<ConversationKey, Worker>>>;

pub(crate) struct Dispatcher {
    shared: Arc<Shared>,
    workers: Workers,
    /// Cancelled when running turns must stop (after the shutdown grace period).
    work_stop: CancellationToken,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Dispatcher {
    pub(crate) fn new(shared: Arc<Shared>, work_stop: CancellationToken) -> Self {
        Self {
            shared,
            workers: Arc::new(Mutex::new(HashMap::new())),
            work_stop,
        }
    }

    /// Handles events in arrival order until `shutdown`.
    pub(crate) async fn run(self, mut rx: mpsc::Receiver<Tracked>, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                next = rx.recv() => match next {
                    Some(tracked) => self.dispatch(tracked).await,
                    None => break,
                },
            }
        }
        rx.close();
        // Events delivered before shutdown are still handled.
        while let Ok(tracked) = rx.try_recv() {
            self.dispatch(tracked).await;
        }
    }

    async fn dispatch(&self, (event, guard): Tracked) {
        let s = &self.shared;
        s.metrics.event();
        let channel = &event.conversation.channel;
        match s
            .store
            .mark_seen(channel, &event.event_id, s.config.dedupe_ttl)
            .await
        {
            Ok(false) => {
                s.metrics.duplicate();
                tracing::debug!(event = %event.event_id, "duplicate event dropped");
                return;
            }
            Ok(true) => {}
            // Failing open: answering twice is better than not answering.
            Err(error) => tracing::warn!(%error, "dedupe check failed"),
        }
        // Another bot's task was admitted by the peer adapter (team links, depth, limits); its
        // roles decide what it may do (ADR 0014).
        let peer = crate::team::is_peer(&event);
        let verdict = if peer {
            crate::policy::Verdict::Answer
        } else {
            s.gate.check(&event, s.clock.now())
        };
        match verdict {
            Verdict::Ignore => {
                s.metrics.ignored();
                return;
            }
            Verdict::RateLimited => {
                turn::reply(s, &event, &s.config.replies.rate_limited).await;
                return;
            }
            Verdict::TooLong => {
                turn::reply(s, &event, &s.config.replies.too_long).await;
                return;
            }
            Verdict::Answer => {}
        }
        if peer {
            if event.event_id.starts_with(okbot_core::PEER_CANCEL_PREFIX) {
                // The caller cancelled the task: stop its turn, say nothing.
                let running = lock(&self.workers)
                    .get(&event.conversation)
                    .and_then(|w| lock(&w.current).clone());
                if let Some(token) = running {
                    token.cancel();
                }
                return;
            }
            // No chat commands through another bot.
            self.route((event, guard));
            return;
        }
        if s.config.personal && !self.owner_or_pairing(&event).await {
            return;
        }
        if event.is_direct
            && let Err(error) = s
                .store
                .set_direct_chat(channel, &event.author.id, &event.conversation)
                .await
        {
            // Only reminders need it; the message is still answered.
            tracing::warn!(%error, "cannot remember the direct chat");
        }
        if event.restricted {
            turn::reply(s, &event, &s.config.replies.restricted).await;
            return;
        }
        match Command::parse(&event.text) {
            Some(Command::Stop) => {
                self.stop(&event).await;
                return;
            }
            // A turn waiting for this answer holds the conversation's queue: answer at once.
            Some(Command::Approve(code)) => {
                return self.answer(&event, &code, Choice::Id("allow".into())).await;
            }
            Some(Command::Deny(code)) => {
                return self.answer(&event, &code, Choice::Id("deny".into())).await;
            }
            Some(Command::Answer(rest)) => {
                let mut words = rest.split_whitespace();
                let code = words.next().unwrap_or_default().to_string();
                return match words.next().and_then(|n| n.parse().ok()) {
                    Some(n) => self.answer(&event, &code, Choice::Number(n)).await,
                    None => {
                        turn::reply(s, &event, "Use `/answer <code> <number>`.").await;
                    }
                };
            }
            _ => {}
        }
        self.route((event, guard));
    }

    /// On a personal bot: handles `/pair` and turns away accounts that are not the owner's.
    /// Returns whether the event continues (it comes from the owner and is not `/pair`).
    async fn owner_or_pairing(&self, event: &InboundEvent) -> bool {
        let s = &self.shared;
        let is_owner = s.person_of(event).await.is_some();
        let Some(Command::Pair(code)) = Command::parse(&event.text) else {
            if !is_owner {
                s.metrics.ignored();
                // Explain in a direct chat only: in a group every mention would get the notice.
                if event.is_direct {
                    turn::reply(s, event, &s.config.replies.private).await;
                }
            }
            return is_owner;
        };
        let reply = if is_owner {
            "This account is already paired.".to_string()
        } else if !event.is_direct {
            "Send `/pair <code>` in a direct message, not in a group.".to_string()
        } else {
            match s
                .store
                .redeem_pairing_code(code.trim(), &event.conversation.channel, &event.author.id)
                .await
            {
                Ok(true) => {
                    tracing::info!(
                        channel = %event.conversation.channel,
                        user = %event.author.id,
                        "owner account paired"
                    );
                    format!(
                        "✓ Paired. This {} account is now yours; I share one memory across your \
                         accounts.",
                        event.conversation.channel.platform()
                    )
                }
                Ok(false) => "That code is not valid. Codes work once and expire after 10 \
                              minutes: run `okbot pair` again."
                    .to_string(),
                Err(error) => {
                    tracing::error!(%error, "pairing failed");
                    s.config.replies.error.clone()
                }
            }
        };
        turn::reply(s, event, &reply).await;
        false
    }

    async fn answer(&self, event: &InboundEvent, code: &str, choice: Choice) {
        crate::interact::answer_text(self.shared.clone(), event, code.trim(), choice).await;
    }

    /// `/stop` acts at once; every other command waits its turn in the conversation.
    async fn stop(&self, event: &InboundEvent) {
        let s = &self.shared;
        let running = lock(&self.workers)
            .get(&event.conversation)
            .and_then(|w| lock(&w.current).clone());
        match running {
            // The turn reports the stop itself (replacing its "thinking…" message if any).
            Some(token) => token.cancel(),
            None => {
                turn::reply(s, event, "Nothing is running.").await;
            }
        }
    }

    /// Sends the event to its conversation's worker, starting one if needed.
    fn route(&self, tracked: Tracked) {
        let key = tracked.0.conversation.clone();
        let mut workers = lock(&self.workers);
        let tracked = match workers.get(&key) {
            Some(worker) => match worker.tx.send(tracked) {
                Ok(()) => return,
                Err(mpsc::error::SendError(tracked)) => tracked,
            },
            None => tracked,
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let current = Arc::new(Mutex::new(None));
        let _ = tx.send(tracked);
        workers.insert(
            key.clone(),
            Worker {
                tx,
                current: current.clone(),
            },
        );
        tokio::spawn(worker(
            self.shared.clone(),
            self.workers.clone(),
            key,
            rx,
            current,
            self.work_stop.clone(),
        ));
    }
}

/// Handles one conversation's events in order. Messages that arrive during a turn are answered
/// together in the next turn, as long as they come from the same person.
async fn worker(
    shared: Arc<Shared>,
    workers: Workers,
    key: ConversationKey,
    mut rx: mpsc::UnboundedReceiver<Tracked>,
    current: Arc<Mutex<Option<CancellationToken>>>,
    work_stop: CancellationToken,
) {
    let mut pending: Option<Tracked> = None;
    loop {
        let first = if let Some(held) = pending.take() {
            Some(held)
        } else {
            tokio::select! {
                next = rx.recv() => next,
                () = tokio::time::sleep(shared.config.worker_idle) => {
                    // Leave only if nothing arrived meanwhile; the dispatcher sends under this lock.
                    let mut map = lock(&workers);
                    if rx.is_empty() {
                        map.remove(&key);
                        return;
                    }
                    continue;
                }
            }
        };
        let Some(first) = first else { return };
        // Commands run alone, in order with the messages around them. Another bot's text is
        // never a command.
        let peer = crate::team::is_peer(&first.0);
        if !peer && let Some(command) = Command::parse(&first.0.text) {
            let text = command::execute(&shared, command, &first.0).await;
            turn::reply(&shared, &first.0, &text).await;
            continue;
        }
        if !peer && let Some((command, rest)) = crate::mcp_command::find(&shared, &first.0.text) {
            crate::mcp_command::run(&shared, command, rest, &first.0).await;
            continue;
        }
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            // A turn runs with one person's identity, scope and rights, so it only batches that
            // person's messages: another author's text must never run as this one.
            // An automation's turn is restricted and isolated: it never batches with a person's
            // messages.
            if (!peer
                && (Command::parse(&more.0.text).is_some()
                    || crate::mcp_command::find(&shared, &more.0.text).is_some()))
                || more.0.author.id != batch[0].0.author.id
                || crate::automation::is_automation(&more.0)
                || crate::automation::is_automation(&batch[0].0)
            {
                pending = Some(more);
                break;
            }
            batch.push(more);
        }
        let cancel = work_stop.child_token();
        *lock(&current) = Some(cancel.clone());
        turn::answer(&shared, &batch, &cancel).await;
        *lock(&current) = None;
        drop(batch);
    }
}

/// Where a reply to `event` goes.
pub(crate) fn target(event: &InboundEvent) -> ReplyTarget {
    ReplyTarget::to_event(event)
}
