//! SQLite storage for okbot conversations, deduplication and audit records.
//!
//! [`SqliteStore`] implements [`ConversationStore`] on one SQLite file in WAL mode. Every call runs
//! on Tokio's blocking pool behind a single connection, which is ample for chat traffic and keeps
//! writes serialized.

mod schema;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;
use okbot_core::{
    AgentRunRecord, ChannelId, Clock, ConversationKey, ConversationStore, MessageRole, NewMessage,
    StoreError, StoredMessage, SystemClock, async_trait,
};
use rusqlite::{Connection, OptionalExtension, params};

/// Errors specific to opening the store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// SQLite failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The database belongs to a newer okbot.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A [`ConversationStore`] backed by SQLite.
#[derive(Clone)]
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore").finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Opens (or creates) the database at `path` and migrates it.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        Self::with_connection(Connection::open(path)?, Arc::new(SystemClock))
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self, OpenError> {
        Self::with_connection(Connection::open_in_memory()?, clock)
    }

    /// Opens the database at `path` with an explicit clock.
    pub fn open_with_clock(path: &Path, clock: Arc<dyn Clock>) -> Result<Self, OpenError> {
        Self::with_connection(Connection::open(path)?, clock)
    }

    fn with_connection(mut conn: Connection, clock: Arc<dyn Clock>) -> Result<Self, OpenError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        if let Err(schema::TooNew(found)) = schema::migrate(&mut conn)? {
            return Err(StoreError::TooNew {
                found,
                supported: schema::VERSION,
            }
            .into());
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            clock,
        })
    }

    /// The schema version this build uses.
    pub fn schema_version() -> u32 {
        schema::VERSION
    }

    async fn with<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, Timestamp) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || {
            // A poisoned lock means a panic happened mid-call; SQLite's own transactions keep the
            // data consistent, so continuing with the inner connection is safe.
            let mut guard = conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            work(&mut guard, now)
        })
        .await
        .map_err(|e| StoreError::Backend(Box::new(e)))?
        .map_err(|e| StoreError::Backend(Box::new(e)))
    }
}

fn millis(t: Timestamp) -> i64 {
    t.as_millisecond()
}

fn from_millis(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn duration_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn conversation_id(conn: &Connection, key: &str, now: Timestamp) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO conversations (key, updated_at) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET updated_at = excluded.updated_at",
        params![key, millis(now)],
    )?;
    conn.query_row(
        "SELECT id FROM conversations WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Bot => "bot",
    }
}

#[async_trait]
impl ConversationStore for SqliteStore {
    async fn mark_seen(
        &self,
        channel: &ChannelId,
        event_id: &str,
        ttl: Duration,
    ) -> Result<bool, StoreError> {
        let channel = channel.to_string();
        let event_id = event_id.to_string();
        self.with(move |conn, now| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM inbound_dedupe WHERE channel = ?1 AND event_id = ?2 AND expires_at <= ?3",
                params![channel, event_id, millis(now)],
            )?;
            let expires = millis(now).saturating_add(duration_ms(ttl));
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO inbound_dedupe (channel, event_id, expires_at) VALUES (?1, ?2, ?3)",
                params![channel, event_id, expires],
            )?;
            tx.commit()?;
            Ok(inserted == 1)
        })
        .await
    }

    async fn append(&self, message: NewMessage) -> Result<(), StoreError> {
        self.with(move |conn, now| {
            let tx = conn.transaction()?;
            let id = conversation_id(&tx, &message.conversation.to_string(), now)?;
            tx.execute(
                "INSERT INTO messages (conversation_id, role, author_id, author_name, text, message_id, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    role_name(message.role),
                    message.author_id.as_ref().map(|u| u.as_str().to_string()),
                    message.author_name,
                    message.text,
                    message.message_id.as_ref().map(|m| m.as_str().to_string()),
                    millis(message.at),
                ],
            )?;
            tx.commit()
        })
        .await
    }

    async fn recent(
        &self,
        conversation: &ConversationKey,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StoreError> {
        let key = conversation.to_string();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with(move |conn, _| {
            let mut stmt = conn.prepare(
                "SELECT m.role, m.author_name, m.text, m.at
                 FROM messages m JOIN conversations c ON c.id = m.conversation_id
                 WHERE c.key = ?1 AND m.id > c.reset_after
                 ORDER BY m.id DESC LIMIT ?2",
            )?;
            let mut rows: Vec<StoredMessage> = stmt
                .query_map(params![key, limit], |row| {
                    let role: String = row.get(0)?;
                    Ok(StoredMessage {
                        role: if role == "bot" {
                            MessageRole::Bot
                        } else {
                            MessageRole::User
                        },
                        author_name: row.get(1)?,
                        text: row.get(2)?,
                        at: from_millis(row.get(3)?),
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            rows.reverse();
            Ok(rows)
        })
        .await
    }

    async fn reset(&self, conversation: &ConversationKey) -> Result<(), StoreError> {
        let key = conversation.to_string();
        self.with(move |conn, now| {
            let tx = conn.transaction()?;
            let id = conversation_id(&tx, &key, now)?;
            let last: Option<i64> = tx
                .query_row(
                    "SELECT MAX(id) FROM messages WHERE conversation_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            tx.execute(
                "UPDATE conversations SET reset_after = ?2, session_id = NULL WHERE id = ?1",
                params![id, last.unwrap_or(0)],
            )?;
            tx.commit()
        })
        .await
    }

    async fn record_run(&self, run: AgentRunRecord) -> Result<(), StoreError> {
        self.with(move |conn, _| {
            let usage = run.usage;
            let tools = match run.tools {
                okbot_core::ToolPolicy::McpOnly => "mcp-only",
                _ => "web",
            };
            let tokens = |f: fn(&okbot_core::TokenUsage) -> u64| {
                usage
                    .as_ref()
                    .map(|u| i64::try_from(f(u)).unwrap_or(i64::MAX))
            };
            conn.execute(
                "INSERT OR REPLACE INTO agent_runs (run_id, conversation, backend, model, tools,
                    input_tokens, output_tokens, reasoning_tokens, cache_read_tokens, cache_write_tokens,
                    duration_ms, error, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    run.run_id.as_str(),
                    run.conversation.to_string(),
                    run.backend,
                    run.model,
                    tools,
                    tokens(|u| u.input),
                    tokens(|u| u.output),
                    tokens(|u| u.reasoning),
                    tokens(|u| u.cache_read),
                    tokens(|u| u.cache_write),
                    duration_ms(run.duration),
                    run.error,
                    millis(run.started_at),
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn purge_before(&self, before: Timestamp) -> Result<u64, StoreError> {
        self.with(move |conn, now| {
            let tx = conn.transaction()?;
            let cutoff = millis(before);
            let mut removed = 0;
            removed += tx.execute("DELETE FROM messages WHERE at < ?1", params![cutoff])?;
            removed += tx.execute("DELETE FROM agent_runs WHERE started_at < ?1", params![cutoff])?;
            removed += tx.execute(
                "DELETE FROM inbound_dedupe WHERE expires_at <= ?1",
                params![millis(now)],
            )?;
            removed += tx.execute(
                "DELETE FROM conversations WHERE updated_at < ?1
                 AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.conversation_id = conversations.id)",
                params![cutoff],
            )?;
            tx.commit()?;
            Ok(removed as u64)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_testkit::ManualClock;

    #[tokio::test]
    async fn conformance() {
        let clock = Arc::new(ManualClock::default());
        let store = SqliteStore::open_in_memory(clock.clone()).unwrap();
        okbot_testkit::conformance::conversation_store(&store, &clock).await;
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okbot.db");
        let clock = Arc::new(ManualClock::default());
        let key = okbot_testkit::conversation("zalo:shop", "u1");
        {
            let store = SqliteStore::open_with_clock(&path, clock.clone()).unwrap();
            store
                .append(okbot_testkit::user_message(&key, "hello", clock.now()))
                .await
                .unwrap();
        }
        let store = SqliteStore::open_with_clock(&path, clock).unwrap();
        assert_eq!(store.recent(&key, 10).await.unwrap()[0].text, "hello");
    }

    #[test]
    fn refuses_newer_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okbot.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", schema::VERSION + 1)
                .unwrap();
        }
        let err = SqliteStore::open(&path).unwrap_err();
        assert!(
            matches!(err, OpenError::Store(StoreError::TooNew { .. })),
            "{err}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers() {
        let clock = Arc::new(ManualClock::default());
        let store = SqliteStore::open_in_memory(clock.clone()).unwrap();
        let key = okbot_testkit::conversation("zalo:shop", "u1");
        let mut tasks = Vec::new();
        for i in 0..50 {
            let store = store.clone();
            let key = key.clone();
            let at = clock.now();
            tasks.push(tokio::spawn(async move {
                store
                    .append(okbot_testkit::user_message(&key, &format!("m{i}"), at))
                    .await
            }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        assert_eq!(store.recent(&key, 100).await.unwrap().len(), 50);
    }
}
