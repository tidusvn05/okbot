//! The memory port: notes the bot keeps about people, rooms and the team (design §5.4).

use serde::{Deserialize, Serialize};

use crate::knowledge::Requester;
use crate::{BoxError, async_trait};

/// Who a note is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryScope {
    /// The requester only.
    User,
    /// The current room (chat) only.
    Room,
    /// Everyone who talks to the bot.
    Global,
}

/// A note to save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryNote {
    /// Who it is for.
    pub scope: MemoryScope,
    /// Short title; derived from the text when `None`.
    pub title: Option<String>,
    /// The note.
    pub text: String,
}

/// A stored note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Id in the knowledge bundle (`memory/users/zalo-shop/u1/coffee`).
    pub id: String,
    /// Who it is for.
    pub scope: MemoryScope,
    /// Title.
    pub title: String,
    /// The note.
    pub text: String,
    /// `platform:instance/user` of the author.
    pub author: Option<String>,
    /// Last update (`YYYY-MM-DD`).
    pub updated: String,
}

/// A memory operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// No such note in the requester's scope.
    #[error("no note `{0}` is visible here")]
    NotFound(String),
    /// The note exists but the requester may not change it.
    #[error("not allowed: {0}")]
    Forbidden(String),
    /// The request is not acceptable (empty, too long).
    #[error("{0}")]
    Invalid(String),
    /// Storage failed.
    #[error("memory storage: {0}")]
    Backend(#[source] BoxError),
}

/// Saves, lists and forgets notes, always within the requester's visibility.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Saves a note.
    async fn save(&self, who: &Requester, note: MemoryNote) -> Result<MemoryEntry, MemoryError>;

    /// Notes visible to `who`: their own, their room's and the team's.
    async fn list(&self, who: &Requester) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// Removes a note by id. Personal and room notes can be removed by anyone who sees them;
    /// team notes by their author (operators use `okbot memory forget`).
    async fn forget(&self, who: &Requester, id: &str) -> Result<MemoryEntry, MemoryError>;
}
