//! The dispatcher: per-event checks, commands, and routing to conversation workers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use okbot_core::{CancellationToken, ConversationKey, InboundEvent, ReplyTarget};
use tokio::sync::mpsc;

use crate::command::{self, Command};
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
        match s.gate.check(&event, s.clock.now()) {
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
        if event.restricted {
            turn::reply(s, &event, &s.config.replies.restricted).await;
            return;
        }
        if Command::parse(&event.text) == Some(Command::Stop) {
            self.stop(&event).await;
            return;
        }
        self.route((event, guard));
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
/// together in the next turn.
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
        // Commands run alone, in order with the messages around them.
        if let Some(command) = Command::parse(&first.0.text) {
            let text = command::execute(&shared, command, &first.0).await;
            turn::reply(&shared, &first.0, &text).await;
            continue;
        }
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            if Command::parse(&more.0.text).is_some() {
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
