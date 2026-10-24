//! Schema migrations, tracked with `PRAGMA user_version`.

use rusqlite::Connection;

/// Migrations in order; migration `i` brings the schema from version `i` to `i + 1`.
/// Never edit a released migration: append a new one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema (design §6.1).
    r"
    CREATE TABLE conversations (
        id          INTEGER PRIMARY KEY,
        key         TEXT NOT NULL UNIQUE,
        reset_after INTEGER NOT NULL DEFAULT 0,
        session_id  TEXT,
        backend     TEXT,
        updated_at  INTEGER NOT NULL
    );
    CREATE TABLE messages (
        id              INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role            TEXT NOT NULL,
        author_id       TEXT,
        author_name     TEXT NOT NULL,
        text            TEXT NOT NULL,
        message_id      TEXT,
        at              INTEGER NOT NULL
    );
    CREATE INDEX messages_by_conversation ON messages(conversation_id, id);
    CREATE INDEX messages_by_time ON messages(at);
    CREATE TABLE inbound_dedupe (
        channel    TEXT NOT NULL,
        event_id   TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (channel, event_id)
    );
    CREATE TABLE agent_runs (
        run_id       TEXT PRIMARY KEY,
        conversation TEXT NOT NULL,
        backend      TEXT NOT NULL,
        model        TEXT,
        tools        TEXT NOT NULL,
        input_tokens       INTEGER,
        output_tokens      INTEGER,
        reasoning_tokens   INTEGER,
        cache_read_tokens  INTEGER,
        cache_write_tokens INTEGER,
        duration_ms  INTEGER NOT NULL,
        error        TEXT,
        started_at   INTEGER NOT NULL
    );
    CREATE INDEX agent_runs_by_time ON agent_runs(started_at);
    CREATE TABLE users (
        channel TEXT NOT NULL,
        user_id TEXT NOT NULL,
        role    TEXT NOT NULL,
        PRIMARY KEY (channel, user_id)
    );
    ",
    // 2: pairing codes for personal bots (owners live in `users` with role `owner`).
    r"
    CREATE TABLE pairing_codes (
        code       TEXT PRIMARY KEY,
        expires_at INTEGER NOT NULL,
        failures   INTEGER NOT NULL DEFAULT 0
    );
    ",
    // 3: task firings (design §5.6) and each user's direct chat, where reminders go.
    r"
    CREATE TABLE task_fires (
        task_id    TEXT NOT NULL,
        fire_at    INTEGER NOT NULL,
        state      TEXT NOT NULL,
        attempts   INTEGER NOT NULL DEFAULT 0,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (task_id, fire_at)
    );
    CREATE INDEX task_fires_by_update ON task_fires(updated_at);
    CREATE TABLE direct_chats (
        channel      TEXT NOT NULL,
        user_id      TEXT NOT NULL,
        conversation TEXT NOT NULL,
        updated_at   INTEGER NOT NULL,
        PRIMARY KEY (channel, user_id)
    );
    ",
    // 4: who each run was for (`[limits]`), tool calls through the MCP gateway and temporary
    // role grants (ADR 0011).
    r"
    ALTER TABLE agent_runs ADD COLUMN person TEXT;
    CREATE INDEX agent_runs_by_person ON agent_runs(person, started_at);
    CREATE TABLE tool_calls (
        id           INTEGER PRIMARY KEY,
        run_id       TEXT,
        command      TEXT,
        person       TEXT NOT NULL,
        conversation TEXT NOT NULL,
        server       TEXT NOT NULL,
        tool         TEXT NOT NULL,
        decision     TEXT NOT NULL,
        error        TEXT,
        duration_ms  INTEGER NOT NULL,
        at           INTEGER NOT NULL
    );
    CREATE INDEX tool_calls_by_time ON tool_calls(at);
    CREATE TABLE role_grants (
        role       TEXT NOT NULL,
        account    TEXT NOT NULL,
        granted_by TEXT NOT NULL,
        granted_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (role, account)
    );
    ",
    // 5: OAuth credentials sealed by the runtime (ADR 0012), and automation state (ADR 0013).
    r"
    CREATE TABLE sealed_secrets (
        kind       TEXT NOT NULL,
        server     TEXT NOT NULL,
        subject    TEXT NOT NULL,
        sealed     BLOB NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (kind, server, subject)
    );
    CREATE TABLE automation_items (
        automation TEXT NOT NULL,
        item       TEXT NOT NULL,
        seen_at    INTEGER NOT NULL,
        PRIMARY KEY (automation, item)
    );
    CREATE INDEX automation_items_by_time ON automation_items(seen_at);
    CREATE TABLE automation_runs (
        id         INTEGER PRIMARY KEY,
        automation TEXT NOT NULL,
        at         INTEGER NOT NULL,
        items      INTEGER NOT NULL,
        action     TEXT NOT NULL,
        error      TEXT
    );
    CREATE INDEX automation_runs_by_name ON automation_runs(automation, at);
    CREATE INDEX automation_runs_by_time ON automation_runs(at);
    ",
    // 6: tasks delegated between bots of a team (ADR 0014).
    r"
    CREATE TABLE peer_tasks (
        id           TEXT NOT NULL,
        inbound      INTEGER NOT NULL,
        team         TEXT NOT NULL,
        peer         TEXT NOT NULL,
        context_id   TEXT NOT NULL,
        message_id   TEXT NOT NULL,
        state        TEXT NOT NULL,
        conversation TEXT,
        trace        TEXT NOT NULL,
        text         TEXT,
        created_at   INTEGER NOT NULL,
        updated_at   INTEGER NOT NULL,
        PRIMARY KEY (inbound, id),
        UNIQUE (inbound, team, peer, message_id)
    );
    CREATE INDEX peer_tasks_by_time ON peer_tasks(updated_at);
    ",
];

/// The schema version this build writes.
#[allow(clippy::cast_possible_truncation)] // a handful of migrations
pub(crate) const VERSION: u32 = MIGRATIONS.len() as u32;

/// Outcome of [`migrate`] when the database is newer than this build.
#[derive(Debug)]
pub(crate) struct TooNew(pub(crate) u32);

/// Brings the database to [`VERSION`]. Refuses databases written by a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<Result<u32, TooNew>, rusqlite::Error> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current > VERSION {
        return Ok(Err(TooNew(current)));
    }
    for version in current..VERSION {
        let tx = conn.transaction()?;
        tx.execute_batch(MIGRATIONS[version as usize])?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
        tracing::info!(version = version + 1, "applied database migration");
    }
    Ok(Ok(VERSION))
}
