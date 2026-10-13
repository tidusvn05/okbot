//! An in-memory [`ConversationStore`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use okbot_core::{
    AgentRunRecord, ChannelId, Clock, ConversationKey, ConversationStore, Fire, FireState,
    NewMessage, PAIRING_MAX_FAILURES, StoreError, StoredMessage, UserId, async_trait,
};

#[derive(Default)]
struct State {
    seen: HashMap<(ChannelId, String), Timestamp>,
    messages: Vec<(u64, NewMessage)>,
    reset_after: HashMap<ConversationKey, u64>,
    sessions: HashMap<ConversationKey, (String, String)>,
    runs: Vec<AgentRunRecord>,
    next: u64,
    owners: Vec<(ChannelId, UserId)>,
    /// code → (expires, failures)
    codes: HashMap<String, (Timestamp, u32)>,
    fires: HashMap<(String, Timestamp), Fire>,
    direct: HashMap<(ChannelId, UserId), ConversationKey>,
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

    async fn owners(&self) -> Result<Vec<(ChannelId, UserId)>, StoreError> {
        let mut owners = self.lock().owners.clone();
        owners.sort();
        Ok(owners)
    }

    async fn remove_owner(&self, channel: &ChannelId, user: &UserId) -> Result<bool, StoreError> {
        let mut state = self.lock();
        let before = state.owners.len();
        state.owners.retain(|(c, u)| c != channel || u != user);
        Ok(state.owners.len() != before)
    }

    async fn create_pairing_code(&self, code: &str, ttl: Duration) -> Result<(), StoreError> {
        let expires = self.clock.now() + signed(ttl);
        self.lock().codes.insert(code.to_string(), (expires, 0));
        Ok(())
    }

    async fn redeem_pairing_code(
        &self,
        code: &str,
        channel: &ChannelId,
        user: &UserId,
    ) -> Result<bool, StoreError> {
        let now = self.clock.now();
        let mut state = self.lock();
        state.codes.retain(|_, (expires, _)| *expires > now);
        if state.codes.remove(code).is_some() {
            let account = (channel.clone(), user.clone());
            if !state.owners.contains(&account) {
                state.owners.push(account);
            }
            return Ok(true);
        }
        for (_, failures) in state.codes.values_mut() {
            *failures += 1;
        }
        state
            .codes
            .retain(|_, (_, failures)| *failures < PAIRING_MAX_FAILURES);
        Ok(false)
    }

    async fn record_run(&self, run: AgentRunRecord) -> Result<(), StoreError> {
        self.lock().runs.push(run);
        Ok(())
    }

    async fn claim_fire(&self, task_id: &str, fire_at: Timestamp) -> Result<Fire, StoreError> {
        let now = self.clock.now();
        Ok(self
            .lock()
            .fires
            .entry((task_id.to_string(), fire_at))
            .or_insert_with(|| Fire {
                task_id: task_id.to_string(),
                fire_at,
                state: FireState::Sending,
                attempts: 0,
                updated_at: now,
            })
            .clone())
    }

    async fn set_fire(
        &self,
        task_id: &str,
        fire_at: Timestamp,
        state: FireState,
        attempts: u32,
    ) -> Result<(), StoreError> {
        let fire = Fire {
            task_id: task_id.to_string(),
            fire_at,
            state,
            attempts,
            updated_at: self.clock.now(),
        };
        self.lock()
            .fires
            .insert((task_id.to_string(), fire_at), fire);
        Ok(())
    }

    async fn recent_fires(&self, limit: usize) -> Result<Vec<Fire>, StoreError> {
        let mut fires: Vec<Fire> = self.lock().fires.values().cloned().collect();
        fires.sort_by_key(|f| std::cmp::Reverse((f.updated_at, f.fire_at)));
        fires.truncate(limit);
        Ok(fires)
    }

    async fn set_direct_chat(
        &self,
        channel: &ChannelId,
        user: &UserId,
        conversation: &ConversationKey,
    ) -> Result<(), StoreError> {
        self.lock()
            .direct
            .insert((channel.clone(), user.clone()), conversation.clone());
        Ok(())
    }

    async fn direct_chat(
        &self,
        channel: &ChannelId,
        user: &UserId,
    ) -> Result<Option<ConversationKey>, StoreError> {
        Ok(self
            .lock()
            .direct
            .get(&(channel.clone(), user.clone()))
            .cloned())
    }

    async fn purge_before(&self, before: Timestamp) -> Result<u64, StoreError> {
        let now = self.clock.now();
        let mut state = self.lock();
        let count = |s: &State| s.messages.len() + s.runs.len() + s.seen.len() + s.fires.len();
        let start = count(&state);
        state.messages.retain(|(_, m)| m.at >= before);
        state.runs.retain(|r| r.started_at >= before);
        state.seen.retain(|_, expires| *expires > now);
        state
            .fires
            .retain(|_, f| f.updated_at >= before || f.state == FireState::Sending);
        let end = count(&state);
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
