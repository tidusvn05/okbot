//! Operator history queries. Opening this reader never creates or migrates a database.

use crate::{OpenError, from_millis, schema};
use jiff::Timestamp;
use okbot_core::{ChannelId, ConversationKey, MessageRole, StoreError, TokenUsage, ToolPolicy};
use rusqlite::{Connection, OpenFlags, Row, params};
use serde::Serialize;
use std::path::Path;

/// Filters and pagination shared by history queries.
#[derive(Debug, Clone)]
pub struct HistoryQuery {
    /// Exact configured channel instance.
    pub channel: Option<ChannelId>,
    /// Database conversation id.
    pub conversation: Option<i64>,
    /// Inclusive lower timestamp.
    pub since: Option<Timestamp>,
    /// Exclusive upper timestamp.
    pub until: Option<Timestamp>,
    /// Page size, from 1 to 1000.
    pub limit: usize,
    /// Number of matching rows to skip.
    pub offset: usize,
    /// Literal Unicode-lowercase substring to find in messages.
    pub search: Option<String>,
    /// Only failed or cancelled agent runs.
    pub errors_only: bool,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            channel: None,
            conversation: None,
            since: None,
            until: None,
            limit: 50,
            offset: 0,
            search: None,
            errors_only: false,
        }
    }
}

/// A conversation available to an operator.
#[derive(Debug, Serialize)]
pub struct ConversationSummary {
    /// Id accepted by `history show`.
    pub id: i64,
    /// Structured channel, room and thread.
    pub conversation: ConversationKey,
    /// Number of retained messages, including before reset.
    pub messages: u64,
    /// Most recent conversation update.
    pub updated_at: Timestamp,
    /// Last message id excluded from the current prompt by reset.
    pub reset_after: i64,
}

/// One retained message, including its audit identifiers.
#[derive(Debug, Serialize)]
pub struct HistoryMessage {
    /// Database message id; also breaks timestamp ties.
    pub id: i64,
    /// Database conversation id.
    pub conversation_id: i64,
    /// Structured conversation key.
    pub conversation: ConversationKey,
    /// User or bot.
    pub role: MessageRole,
    /// Platform author id, when recorded.
    pub author_id: Option<String>,
    /// Display name at the time of the message.
    pub author_name: String,
    /// Retained text.
    pub text: String,
    /// Platform message id, when recorded.
    pub message_id: Option<String>,
    /// Message timestamp.
    pub at: Timestamp,
    /// Whether this message precedes the conversation's latest reset.
    pub before_reset: bool,
}

/// The existing audit record for one agent turn, not individual fallback attempts.
#[derive(Debug, Serialize)]
pub struct HistoryRun {
    /// Audit run id.
    pub run_id: String,
    /// Database conversation id, when it still exists.
    pub conversation_id: Option<i64>,
    /// Structured conversation key.
    pub conversation: ConversationKey,
    /// Recorded backend (may be the chain on failure).
    pub backend: String,
    /// Configured model override; null means the CLI default was not recorded.
    pub model: Option<String>,
    /// Granted tool policy.
    pub tools: ToolPolicy,
    /// Recorded token counters, when available.
    pub usage: Option<TokenUsage>,
    /// Turn duration in milliseconds.
    pub duration_ms: u64,
    /// Failure or cancellation, when recorded.
    pub error: Option<String>,
    /// Turn start timestamp.
    pub started_at: Timestamp,
}

/// A read-only SQLite connection for local operator queries.
///
/// Methods are synchronous: call them on a blocking worker in async applications.
pub struct HistoryReader {
    conn: Connection,
}

fn key(row: &Row<'_>, index: usize) -> rusqlite::Result<ConversationKey> {
    let value: String = row.get(index)?;
    value.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(e),
        )
    })
}

fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Backend(Box::new(error))
}

// Stored times have millisecond precision; round both comparison bounds up so a fractional
// lower bound excludes that millisecond and a fractional exclusive upper bound includes it.
fn bound(at: Timestamp) -> i64 {
    let nanos = at.as_nanosecond();
    i64::try_from(nanos.div_euclid(1_000_000) + i128::from(nanos.rem_euclid(1_000_000) != 0))
        .expect("jiff's timestamp range fits in i64 milliseconds")
}

fn page(query: &HistoryQuery) -> Result<(i64, i64), StoreError> {
    if !(1..=1000).contains(&query.limit)
        || query.since.zip(query.until).is_some_and(|(a, b)| a >= b)
        || query.conversation.is_some_and(|id| id <= 0)
    {
        return Err(backend(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "history needs limit 1..1000, a positive conversation id, and since before until",
        )));
    }
    Ok((
        i64::try_from(query.limit).map_err(backend)?,
        i64::try_from(query.offset).map_err(backend)?,
    ))
}

impl HistoryReader {
    /// Opens an existing database read-only, accepting released history schemas without migration.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > schema::VERSION {
            return Err(StoreError::TooNew {
                found: version,
                supported: schema::VERSION,
            }
            .into());
        }
        // All released migrations share these history columns. Version 0 is not an okbot DB.
        if version == 0 {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        Ok(Self { conn })
    }

    /// Lists conversations by latest update, newest first.
    pub fn conversations(&self, q: &HistoryQuery) -> Result<Vec<ConversationSummary>, StoreError> {
        let (limit, offset) = page(q)?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.id, c.key, c.reset_after, c.updated_at,
                (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id)
             FROM conversations c
             WHERE (?1 IS NULL OR substr(c.key, 1, instr(c.key, '|') - 1) = ?1)
               AND (?2 IS NULL OR c.id = ?2)
               AND (?3 IS NULL OR c.updated_at >= ?3)
               AND (?4 IS NULL OR c.updated_at < ?4)
             ORDER BY c.updated_at DESC, c.id DESC LIMIT ?5 OFFSET ?6",
            )
            .map_err(backend)?;
        stmt.query_map(
            params![
                q.channel.as_ref().map(ToString::to_string),
                q.conversation,
                q.since.map(bound),
                q.until.map(bound),
                limit,
                offset
            ],
            |r| {
                Ok(ConversationSummary {
                    id: r.get(0)?,
                    conversation: key(r, 1)?,
                    reset_after: r.get(2)?,
                    updated_at: from_millis(r.get(3)?),
                    messages: unsigned(r, 4)?,
                })
            },
        )
        .map_err(backend)?
        .collect::<rusqlite::Result<_>>()
        .map_err(backend)
    }

    /// Reads retained messages newest first, including before reset; search is literal lowercase.
    pub fn messages(&self, q: &HistoryQuery) -> Result<Vec<HistoryMessage>, StoreError> {
        page(q)?;
        let mut stmt = self.conn.prepare(
            "SELECT m.id, c.id, c.key, m.role, m.author_id, m.author_name, m.text, m.message_id, m.at,
                    m.id <= c.reset_after
             FROM messages m JOIN conversations c ON c.id = m.conversation_id
             WHERE (?1 IS NULL OR substr(c.key, 1, instr(c.key, '|') - 1) = ?1)
               AND (?2 IS NULL OR c.id = ?2)
               AND (?3 IS NULL OR m.at >= ?3)
               AND (?4 IS NULL OR m.at < ?4)
             ORDER BY m.at DESC, m.id DESC").map_err(backend)?;
        let search = q.search.as_ref().map(|s| s.to_lowercase());
        let rows = stmt
            .query_map(
                params![
                    q.channel.as_ref().map(ToString::to_string),
                    q.conversation,
                    q.since.map(bound),
                    q.until.map(bound)
                ],
                |r| {
                    let role: String = r.get(3)?;
                    Ok(HistoryMessage {
                        id: r.get(0)?,
                        conversation_id: r.get(1)?,
                        conversation: key(r, 2)?,
                        role: if role == "bot" {
                            MessageRole::Bot
                        } else {
                            MessageRole::User
                        },
                        author_id: r.get(4)?,
                        author_name: r.get(5)?,
                        text: r.get(6)?,
                        message_id: r.get(7)?,
                        at: from_millis(r.get(8)?),
                        before_reset: r.get(9)?,
                    })
                },
            )
            .map_err(backend)?;
        // Streaming over the selected range avoids SQLite's ASCII-only lower() and a new index.
        let mut found = Vec::new();
        let mut skipped = 0;
        for row in rows {
            let message = row.map_err(backend)?;
            if search
                .as_ref()
                .is_some_and(|s| !message.text.to_lowercase().contains(s))
            {
                continue;
            }
            if skipped < q.offset {
                skipped += 1;
                continue;
            }
            found.push(message);
            if found.len() == q.limit {
                break;
            }
        }
        Ok(found)
    }

    /// Reads audit runs newest first. Unknown model/usage remain null.
    pub fn runs(&self, q: &HistoryQuery) -> Result<Vec<HistoryRun>, StoreError> {
        let (limit, offset) = page(q)?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT r.run_id, c.id, r.conversation, r.backend, r.model, r.tools,
                    r.input_tokens, r.output_tokens, r.reasoning_tokens, r.cache_read_tokens,
                    r.cache_write_tokens, r.duration_ms, r.error, r.started_at
             FROM agent_runs r LEFT JOIN conversations c ON c.key = r.conversation
             WHERE (?1 IS NULL OR substr(r.conversation, 1, instr(r.conversation, '|') - 1) = ?1)
               AND (?2 IS NULL OR c.id = ?2)
               AND (?3 IS NULL OR r.started_at >= ?3)
               AND (?4 IS NULL OR r.started_at < ?4)
               AND (?5 = 0 OR r.error IS NOT NULL)
             ORDER BY r.started_at DESC, r.run_id DESC LIMIT ?6 OFFSET ?7",
            )
            .map_err(backend)?;
        stmt.query_map(
            params![
                q.channel.as_ref().map(ToString::to_string),
                q.conversation,
                q.since.map(bound),
                q.until.map(bound),
                q.errors_only,
                limit,
                offset
            ],
            |r| {
                let counters: Vec<Option<u64>> = (6..=10)
                    .map(|i| {
                        if r.get::<_, Option<i64>>(i)?.is_some() {
                            unsigned(r, i).map(Some)
                        } else {
                            Ok(None)
                        }
                    })
                    .collect::<rusqlite::Result<_>>()?;
                let usage = counters.iter().any(Option::is_some).then(|| TokenUsage {
                    input: counters[0].unwrap_or(0),
                    output: counters[1].unwrap_or(0),
                    reasoning: counters[2].unwrap_or(0),
                    cache_read: counters[3].unwrap_or(0),
                    cache_write: counters[4].unwrap_or(0),
                });
                let tools: String = r.get(5)?;
                let tools = match tools.as_str() {
                    "web" => ToolPolicy::Web,
                    "mcp-only" => ToolPolicy::McpOnly,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(HistoryRun {
                    run_id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    conversation: key(r, 2)?,
                    backend: r.get(3)?,
                    model: r.get(4)?,
                    tools,
                    usage,
                    duration_ms: unsigned(r, 11)?,
                    error: r.get(12)?,
                    started_at: from_millis(r.get(13)?),
                })
            },
        )
        .map_err(backend)?
        .collect::<rusqlite::Result<_>>()
        .map_err(backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;
    use okbot_core::{AgentRunRecord, Clock, ConversationStore};
    use okbot_testkit::{ManualClock, bot_message, conversation, user_message};
    use std::sync::Arc;

    #[tokio::test]
    async fn read_only_audit_includes_reset_unicode_and_stable_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okbot.db");
        let clock = Arc::new(ManualClock::default());
        let store = SqliteStore::open_with_clock(&path, clock.clone()).unwrap();
        let key = conversation("discord:main", "r|1");
        let other = conversation("zalo:main", "u1");
        let at = clock.now();
        store
            .append(user_message(&key, "HỢP ĐỒNG 100%_", at))
            .await
            .unwrap();
        store.append(bot_message(&key, "日本語", at)).await.unwrap();
        store.reset(&key).await.unwrap();
        store
            .append(user_message(&key, "new context", at))
            .await
            .unwrap();
        store
            .append(user_message(&other, "another channel", at))
            .await
            .unwrap();
        let reader = HistoryReader::open(&path).unwrap();
        let q = HistoryQuery {
            channel: Some(key.channel.clone()),
            ..HistoryQuery::default()
        };
        let conversations = reader.conversations(&q).unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].messages, 3);
        assert_eq!(conversations[0].conversation, key);
        let page = HistoryQuery {
            conversation: Some(conversations[0].id),
            limit: 1,
            ..HistoryQuery::default()
        };
        let newest = reader.messages(&page).unwrap();
        assert_eq!(newest[0].text, "new context");
        assert!(!newest[0].before_reset);
        let previous = reader
            .messages(&HistoryQuery {
                offset: 1,
                ..page.clone()
            })
            .unwrap();
        assert_eq!(previous[0].text, "日本語");
        assert!(previous[0].before_reset);
        for text in ["hợp đồng", "100%_", "日本語"] {
            assert_eq!(
                reader
                    .messages(&HistoryQuery {
                        search: Some(text.into()),
                        ..q.clone()
                    })
                    .unwrap()
                    .len(),
                1
            );
        }
        assert!(
            reader
                .messages(&HistoryQuery {
                    search: Some("' OR 1=1 --".into()),
                    ..q.clone()
                })
                .unwrap()
                .is_empty()
        );
        // A long-lived reader sees committed writes without migrating or taking a write lock.
        clock.advance(std::time::Duration::from_secs(1));
        store
            .append(bot_message(&key, "latest", clock.now()))
            .await
            .unwrap();
        assert_eq!(reader.messages(&page).unwrap()[0].text, "latest");
        assert!(reader.conn.execute("DELETE FROM messages", []).is_err());
        assert_eq!(
            reader
                .conn
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            schema::VERSION
        );
    }

    #[tokio::test]
    async fn fractional_time_bounds_respect_stored_millisecond_precision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okbot.db");
        let clock = Arc::new(ManualClock::default());
        let store = SqliteStore::open_with_clock(&path, clock.clone()).unwrap();
        let key = conversation("discord:main", "r1");
        let at = clock.now();
        store
            .append(user_message(&key, "one millisecond", at))
            .await
            .unwrap();
        let reader = HistoryReader::open(&path).unwrap();
        let q = HistoryQuery::default();
        assert!(
            reader
                .messages(&HistoryQuery {
                    since: Some(at + jiff::SignedDuration::from_nanos(1)),
                    ..q.clone()
                })
                .unwrap()
                .is_empty()
        );
        assert!(
            reader
                .messages(&HistoryQuery {
                    until: Some(at),
                    ..q.clone()
                })
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reader
                .messages(&HistoryQuery {
                    until: Some(at + jiff::SignedDuration::from_nanos(1)),
                    ..q.clone()
                })
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn audit_runs_keep_unknown_fields_and_filter_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okbot.db");
        let clock = Arc::new(ManualClock::default());
        let store = SqliteStore::open_with_clock(&path, clock.clone()).unwrap();
        let key = conversation("discord:main", "r|1");
        let at = clock.now();
        store
            .append(user_message(&key, "request", at))
            .await
            .unwrap();
        let reader = HistoryReader::open(&path).unwrap();
        let q = HistoryQuery {
            channel: Some(key.channel.clone()),
            ..HistoryQuery::default()
        };
        for (id, error, usage) in [
            ("r1", None, None),
            (
                "r2",
                Some("cancelled".into()),
                Some(TokenUsage {
                    input: 9,
                    ..TokenUsage::default()
                }),
            ),
        ] {
            store
                .record_run(AgentRunRecord {
                    run_id: id.into(),
                    conversation: key.clone(),
                    backend: "claude".into(),
                    model: None,
                    tools: ToolPolicy::McpOnly,
                    usage,
                    duration: std::time::Duration::from_millis(123),
                    error,
                    started_at: at,
                    person: None,
                })
                .await
                .unwrap();
        }
        let runs = reader.runs(&q).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, "r2");
        assert_eq!(runs[0].usage.unwrap().input, 9);
        assert!(runs[1].usage.is_none() && runs[1].model.is_none());
        assert_eq!(
            reader
                .runs(&HistoryQuery {
                    errors_only: true,
                    ..q
                })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn reader_never_creates_or_migrates_and_rejects_corrupt_or_future_databases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        assert!(HistoryReader::open(&path).is_err());
        assert!(!path.exists());
        drop(SqliteStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let reader = HistoryReader::open(&path).unwrap();
        assert!(
            reader
                .conversations(&HistoryQuery::default())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert!(
            reader
                .messages(&HistoryQuery {
                    limit: 0,
                    ..HistoryQuery::default()
                })
                .is_err()
        );
        drop(reader);
        conn.pragma_update(None, "user_version", schema::VERSION + 1)
            .unwrap();
        assert!(matches!(
            HistoryReader::open(&path),
            Err(OpenError::Store(StoreError::TooNew { .. }))
        ));
        drop(conn);
        std::fs::write(&path, "not SQLite").unwrap();
        assert!(HistoryReader::open(&path).is_err());
    }
}
