//! Artifacts (ADR 0021): pages, documents, tables and diagrams the agent makes in chat, kept as
//! files in the bundle with their versions, in the same owner folders as tasks and memory.

use async_trait::async_trait;
use jiff::Timestamp;

use crate::{BoxError, ConversationKey, MemoryScope, Requester};

/// What an artifact holds, which decides how it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArtifactKind {
    /// One HTML page with inline CSS and JS, shown in the sandboxed viewer.
    Html,
    /// Markdown, rendered without raw HTML.
    Markdown,
    /// An SVG image, shown in the sandboxed viewer.
    Svg,
    /// A Mermaid diagram's source, rendered in the sandboxed viewer.
    Mermaid,
    /// Comma-separated values, shown as a table.
    Csv,
    /// JSON, shown as a tree.
    Json,
    /// Plain text.
    Text,
}

impl ArtifactKind {
    /// Every kind.
    pub const ALL: [Self; 7] = [
        Self::Html,
        Self::Markdown,
        Self::Svg,
        Self::Mermaid,
        Self::Csv,
        Self::Json,
        Self::Text,
    ];

    /// The name used in tools and files (`html`, `markdown`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "markdown",
            Self::Svg => "svg",
            Self::Mermaid => "mermaid",
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Text => "text",
        }
    }

    /// The extension of its version files.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "md",
            Self::Svg => "svg",
            Self::Mermaid => "mmd",
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Text => "txt",
        }
    }

    /// The media type it is downloaded as.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Markdown => "text/markdown; charset=utf-8",
            Self::Svg => "image/svg+xml",
            Self::Mermaid | Self::Text => "text/plain; charset=utf-8",
            Self::Csv => "text/csv; charset=utf-8",
            Self::Json => "application/json",
        }
    }
}

impl std::str::FromStr for ArtifactKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        let s = match s.as_str() {
            "md" => "markdown",
            "txt" | "plain" => "text",
            "mmd" => "mermaid",
            other => other,
        };
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| {
                format!("kind `{s}`: one of html, markdown, svg, mermaid, csv, json or text")
            })
    }
}

impl std::fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One saved version of an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVersion {
    /// 1, 2, 3…
    pub n: u32,
    /// When it was saved.
    pub at: Timestamp,
    /// Who saved it (`channel/user`).
    pub by: String,
    /// Its size.
    pub bytes: u64,
    /// SHA-256 of its content, hex.
    pub sha256: String,
    /// What changed, as the saver put it.
    pub note: String,
    /// Its file was removed to keep within `keep_versions`.
    pub pruned: bool,
}

/// A stored artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Path-like id (`artifacts/people/p-1/a-7k2m9q4x2c`); the last segment is its short id.
    pub id: String,
    /// Title, in the user's language.
    pub title: String,
    /// What it holds.
    pub kind: ArtifactKind,
    /// What it is, in a few lines: what knowledge search finds.
    pub summary: String,
    /// The person's own (`User`) or this chat's (`Room`).
    pub scope: MemoryScope,
    /// Who made it (`channel/user`).
    pub author: String,
    /// Where it was made.
    pub origin: Option<ConversationKey>,
    /// When it was made.
    pub created: Timestamp,
    /// Every version, oldest first; never empty.
    pub versions: Vec<ArtifactVersion>,
}

impl Artifact {
    /// The short id (`a-7k2m9q4x2c`).
    pub fn short_id(&self) -> &str {
        self.id.rsplit('/').next().unwrap_or(&self.id)
    }

    /// The newest version.
    pub fn latest(&self) -> &ArtifactVersion {
        self.versions
            .last()
            .expect("an artifact always has a version")
    }

    /// When it last changed.
    pub fn updated(&self) -> Timestamp {
        self.latest().at
    }

    /// Bytes kept on disk, over the versions that were not pruned.
    pub fn stored_bytes(&self) -> u64 {
        self.versions
            .iter()
            .filter(|v| !v.pruned)
            .map(|v| v.bytes)
            .sum()
    }
}

/// A new artifact, or with `id` a new version of an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArtifact {
    /// An existing artifact to add a version to.
    pub id: Option<String>,
    /// Title; for a new version, empty keeps the current one.
    pub title: String,
    /// What it holds; a new version keeps the artifact's kind.
    pub kind: ArtifactKind,
    /// What it is; for a new version, empty keeps the current one.
    pub summary: String,
    /// The content.
    pub content: String,
    /// The person's own or this chat's (new artifacts only).
    pub scope: MemoryScope,
    /// What changed (versions after the first).
    pub note: String,
}

/// Bounds on what artifacts may take (`[artifacts]`, ADR 0021).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactLimits {
    /// Largest content of one version.
    pub max_bytes: u64,
    /// All artifacts of one owner folder.
    pub quota_owner: u64,
    /// All artifacts of the bot.
    pub quota_bot: u64,
    /// Versions whose files are kept; older ones are pruned.
    pub keep_versions: u32,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1 << 20,
            quota_owner: 50 << 20,
            quota_bot: 1 << 30,
            keep_versions: 10,
        }
    }
}

/// Why an artifact call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArtifactError {
    /// No such artifact in the requester's scope.
    #[error("no artifact `{0}` is visible here")]
    NotFound(String),
    /// The request is not acceptable.
    #[error("{0}")]
    Invalid(String),
    /// A size limit or quota would be passed.
    #[error("{0}")]
    TooLarge(String),
    /// Storage failed.
    #[error("artifact storage: {0}")]
    Backend(#[source] BoxError),
}

/// Saves, lists and changes artifacts, always within the requester's visibility.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// Saves a new artifact, or a new version of a visible one.
    async fn save(&self, who: &Requester, new: NewArtifact) -> Result<Artifact, ArtifactError>;

    /// Artifacts visible to `who` (theirs and their room's), newest change first.
    async fn list(&self, who: &Requester) -> Result<Vec<Artifact>, ArtifactError>;

    /// A visible artifact by full or short id, and the content of `version` (default: the
    /// newest), with its number.
    async fn open(
        &self,
        who: &Requester,
        id: &str,
        version: Option<u32>,
    ) -> Result<(Artifact, u32, String), ArtifactError>;

    /// Renames a visible artifact.
    async fn rename(
        &self,
        who: &Requester,
        id: &str,
        title: &str,
    ) -> Result<Artifact, ArtifactError>;

    /// Deletes a visible artifact with every version.
    async fn delete(&self, who: &Requester, id: &str) -> Result<Artifact, ArtifactError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        for kind in ArtifactKind::ALL {
            assert_eq!(kind.as_str().parse::<ArtifactKind>(), Ok(kind));
        }
        assert_eq!("MD".parse::<ArtifactKind>(), Ok(ArtifactKind::Markdown));
        assert!("exe".parse::<ArtifactKind>().is_err());
    }
}
