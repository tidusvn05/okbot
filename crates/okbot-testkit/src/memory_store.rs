//! An in-memory [`ConversationStore`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use okbot_core::{
    AgentRunRecord, ChannelId, Clock, ConversationKey, ConversationStore, NewMessage, StoreError,
    StoredMessage, async_trait,
};

#[derive(Default)]
struct State {
    seen: HashMap<(ChannelId, String), Timestamp>,
    messages: Vec<(u64, NewMessage)>,
    reset_after: HashMap<ConversationKey, u64>,
    sessions: HashMap<ConversationKey, (String, String)>,
    runs: Vec<AgentRunRecord>,
    next: u64,
}

/// A [`ConversationStore`] kept in memory, for runtime tests.
pub struct MemoryStore {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore").finish_non_exhaustive()
    }
}

impl MemoryStore {
    /// An empty store reading time from `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Mutex::new(State::default()),
            clock,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Recorded agent runs, oldest first.
    pub fn runs(&self) -> Vec<AgentRunRecord> {
        self.lock().runs.clone()
    }
}

fn signed(d: Duration) -> SignedDuration {
    SignedDuration::try_from(d).unwrap_or(SignedDuration::MAX)
}

#[async_trait]
impl ConversationStore for MemoryStore {
    async fn mark_seen(
        &self,
        channel: &ChannelId,
        event_id: &str,
        ttl: Duration,
    ) -> Result<bool, StoreError> {
        let now = self.clock.now();
        let mut state = self.lock();
        let key = (channel.clone(), event_id.to_string());
        if state.seen.get(&key).is_some_and(|expires| *expires > now) {
            return Ok(false);
        }
        state
            .seen
            .insert(key, now.saturating_add(signed(ttl)).unwrap_or(now));
        Ok(true)
    }

    async fn append(&self, message: NewMessage) -> Result<(), StoreError> {
        let mut state = self.lock();
        state.next += 1;
        let id = state.next;
        state.messages.push((id, message));
        Ok(())
    }

    async fn recent(
        &self,
        conversation: &ConversationKey,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StoreError> {
        let state = self.lock();
        let after = state.reset_after.get(conversation).copied().unwrap_or(0);
        let mut out: Vec<StoredMessage> = state
            .messages
            .iter()
            .rev()
            .filter(|(id, m)| *id > after && m.conversation == *conversation)
            .take(limit)
            .map(|(_, m)| StoredMessage {
                role: m.role,
                author_name: m.author_name.clone(),
                text: m.text.clone(),
                at: m.at,
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    async fn reset(&self, conversation: &ConversationKey) -> Result<(), StoreError> {
        let mut state = self.lock();
        let last = state.next;
        state.reset_after.insert(conversation.clone(), last);
        state.sessions.remove(conversation);
        Ok(())
    }

    async fn session(
        &self,
        conversation: &ConversationKey,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self.lock().sessions.get(conversation).cloned())
    }

    async fn set_session(
        &self,
        conversation: &ConversationKey,
        backend: &str,
        session: &str,
    ) -> Result<(), StoreError> {
        self.lock().sessions.insert(
            conversation.clone(),
            (backend.to_string(), session.to_string()),
        );
        Ok(())
    }

    async fn record_run(&self, run: AgentRunRecord) -> Result<(), StoreError> {
        self.lock().runs.push(run);
        Ok(())
    }

    async fn purge_before(&self, before: Timestamp) -> Result<u64, StoreError> {
        let now = self.clock.now();
        let mut state = self.lock();
        let start = state.messages.len() + state.runs.len() + state.seen.len();
        state.messages.retain(|(_, m)| m.at >= before);
        state.runs.retain(|r| r.started_at >= before);
        state.seen.retain(|_, expires| *expires > now);
        let end = state.messages.len() + state.runs.len() + state.seen.len();
        Ok((start - end) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;

    #[tokio::test]
    async fn conformance() {
        let clock = Arc::new(ManualClock::default());
        let store = MemoryStore::new(clock.clone());
        crate::conformance::conversation_store(&store, &clock).await;
    }
}
