//! okbase integration for okbot: knowledge bundle, scoped MCP tools and memory.
//!
//! [`Knowledge`] opens the bot's okbase bundle and implements [`KnowledgeProvider`]:
//!
//! - **context**: persona documents, the bundle catalog (shared folders only) and the requester's
//!   profile, for the system prompt;
//! - **grant**: a run token for the loopback MCP endpoint, where okbase serves `kb_grep`,
//!   `kb_get`, `kb_query`, … limited to what the requester may see;
//! - **excerpts**: lexical search results for agents that cannot use MCP.
//!
//! Visibility (design §5.3): everyone sees `persona/`, `system/`, `knowledge/`, `data/` and
//! `memory/global/`; a user additionally sees their own profile and notes, a room its own notes.

mod bot_mcp;
mod mcp;
mod memory;
mod paths;
mod scope;
mod template;
mod tokens;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use okbase::{Bundle, CatalogOptions, GetRequest, GrepRequest, OpenOptions, StateDir};
use okbot_core::{
    CancellationToken, KnowledgeContext, KnowledgeError, KnowledgeProvider, McpServer, MemoryEntry,
    MemoryError, MemoryNote, MemoryScope, MemoryStore, Requester, RunGrant, Secret, ToolPolicy,
    async_trait,
};

pub use template::{default_bundle_files, write_default_bundle};

/// Name of the environment variable that carries the run token to the agent.
pub const KB_TOKEN_ENV: &str = "KB_MCP_TOKEN";

/// How to open the knowledge bundle.
#[derive(Debug, Clone)]
pub struct KnowledgeOptions {
    /// Bundle directory.
    pub root: PathBuf,
    /// Where okbase keeps its index; `None` lets okbase choose (`<bundle>/.okbase`).
    pub index_dir: Option<PathBuf>,
    /// Largest catalog placed in the system prompt, in estimated tokens.
    pub catalog_tokens: usize,
    /// Changed files are picked up after at most this long.
    pub refresh_every: Duration,
}

impl KnowledgeOptions {
    /// Defaults for the bundle at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            index_dir: None,
            catalog_tokens: 3000,
            refresh_every: Duration::from_secs(5),
        }
    }
}

fn bundle_error(e: impl std::error::Error + Send + Sync + 'static) -> KnowledgeError {
    KnowledgeError::Bundle(Box::new(e))
}

fn other_error(e: impl std::error::Error + Send + Sync + 'static) -> KnowledgeError {
    KnowledgeError::Other(Box::new(e))
}

/// The bot's knowledge and memory, backed by okbase.
pub struct Knowledge {
    bundle: Bundle,
    options: KnowledgeOptions,
    tokens: Arc<tokens::RunTokens>,
    mcp_addr: Mutex<Option<SocketAddr>>,
    catalog: Mutex<Option<String>>,
    notes: memory::Notes,
    /// Serializes memory writes (one bot process owns its bundle).
    writes: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Knowledge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Knowledge")
            .field("root", &self.options.root)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Knowledge {
    /// Opens and indexes the bundle. Blocking: call it at start-up or from `spawn_blocking`.
    pub fn open(options: KnowledgeOptions) -> Result<Self, KnowledgeError> {
        let state_dir = options
            .index_dir
            .clone()
            .map_or(StateDir::Auto, StateDir::Path);
        let bundle = Bundle::open(
            &options.root,
            OpenOptions {
                state_dir,
                ..OpenOptions::default()
            },
        )
        .map_err(bundle_error)?;
        bundle.sync().map_err(bundle_error)?;
        Ok(Self {
            bundle,
            notes: memory::Notes::new(&options.root),
            options,
            tokens: Arc::new(tokens::RunTokens::default()),
            mcp_addr: Mutex::new(None),
            catalog: Mutex::new(None),
            writes: tokio::sync::Mutex::new(()),
        })
    }

    /// The okbase bundle (for lint and maintenance commands).
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// The bundle directory.
    pub fn root(&self) -> &Path {
        &self.options.root
    }

    /// Starts the loopback MCP endpoint used by [`KnowledgeProvider::grant`]: okbase's knowledge
    /// tools at `/kb/mcp` and memory tools at `/bot/mcp`. Without it, grants carry no tools.
    pub async fn start_mcp(
        self: &Arc<Self>,
        stop: CancellationToken,
    ) -> std::io::Result<SocketAddr> {
        let bot = bot_mcp::router(bot_mcp::BotServer {
            knowledge: self.clone(),
            tokens: self.tokens.clone(),
        });
        let router = mcp::router(self.bundle.clone(), &self.tokens, bot);
        let addr = mcp::serve(router, stop).await?;
        *lock(&self.mcp_addr) = Some(addr);
        Ok(addr)
    }

    /// Re-indexes changed files at most every `refresh_every`; drops the cached catalog on change.
    async fn refresh(&self) {
        let bundle = self.bundle.clone();
        let every = self.options.refresh_every;
        match tokio::task::spawn_blocking(move || bundle.refresh(every)).await {
            Ok(Ok(true)) => *lock(&self.catalog) = None,
            Ok(Ok(false)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "knowledge refresh failed"),
            Err(error) => tracing::warn!(%error, "knowledge refresh task failed"),
        }
    }

    async fn catalog(&self) -> Result<String, KnowledgeError> {
        if let Some(cached) = lock(&self.catalog).clone() {
            return Ok(cached);
        }
        let bundle = self.bundle.clone();
        let options = CatalogOptions {
            max_tokens: self.options.catalog_tokens,
        };
        let content = tokio::task::spawn_blocking(move || {
            bundle
                .catalog(&options, &scope::shared())
                .map(|c| c.content)
        })
        .await
        .map_err(other_error)?
        .map_err(bundle_error)?;
        *lock(&self.catalog) = Some(content.clone());
        Ok(content)
    }

    /// Bodies of the documents in `persona/`, in file-name order.
    fn persona(&self) -> String {
        let dir = self.options.root.join("persona");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return String::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "md")
                    && p.file_name().is_some_and(|n| n != "index.md")
            })
            .collect();
        files.sort();
        files
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .map(|text| strip_frontmatter(&text).trim().to_string())
            .filter(|body| !body.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// After a memory change: re-index, drop the cached catalog, commit when the bundle is a git
    /// repository.
    async fn after_write(&self, message: String) {
        let bundle = self.bundle.clone();
        if let Ok(Err(error)) = tokio::task::spawn_blocking(move || bundle.sync()).await {
            tracing::warn!(%error, "re-indexing after a memory change failed");
        }
        *lock(&self.catalog) = None;
        let root = self.options.root.clone();
        let _ = tokio::task::spawn_blocking(move || memory::git_commit(&root, &message)).await;
    }

    /// Saves a note written during a run with tool policy `policy` (recorded on the note).
    pub(crate) async fn save_with_policy(
        &self,
        who: &Requester,
        note: MemoryNote,
        policy: &'static str,
    ) -> Result<MemoryEntry, MemoryError> {
        let _guard = self.writes.lock().await;
        let notes = self.notes.clone();
        let requester = who.clone();
        let entry = tokio::task::spawn_blocking(move || {
            notes.save(&requester, &note, jiff::Timestamp::now(), policy)
        })
        .await
        .map_err(|e| MemoryError::Backend(Box::new(e)))??;
        self.after_write(format!("memory: save {}", entry.id)).await;
        Ok(entry)
    }

    /// Every note in the bundle (for operators).
    pub async fn all_notes(&self) -> Vec<MemoryEntry> {
        let notes = self.notes.clone();
        tokio::task::spawn_blocking(move || notes.all())
            .await
            .unwrap_or_default()
    }

    /// Removes any note by id (for operators).
    pub async fn remove_note(&self, id: &str) -> Result<(), MemoryError> {
        let _guard = self.writes.lock().await;
        let notes = self.notes.clone();
        let target = id.to_string();
        tokio::task::spawn_blocking(move || notes.remove(&target))
            .await
            .map_err(|e| MemoryError::Backend(Box::new(e)))??;
        self.after_write(format!("memory: remove {id} (operator)"))
            .await;
        Ok(())
    }

    async fn user_profile(&self, who: &Requester) -> Option<String> {
        let bundle = self.bundle.clone();
        let id = paths::user_profile(&who.conversation.channel, &who.user.id);
        let scope = scope::for_requester(who);
        let request = GetRequest {
            id,
            max_tokens: Some(800),
            ..GetRequest::default()
        };
        let profile = tokio::task::spawn_blocking(move || bundle.get(&request, &scope))
            .await
            .ok()
            .and_then(Result::ok)
            .map(|r| strip_frontmatter(&r.content).trim().to_string())
            .filter(|s| !s.is_empty());
        let notes: Vec<String> = self
            .list(who)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.scope == MemoryScope::User)
            .take(20)
            .map(|e| format!("- {} ({})", e.text.replace('\n', " "), e.id))
            .collect();
        match (profile, notes.is_empty()) {
            (None, true) => None,
            (Some(p), true) => Some(p),
            (p, false) => Some(format!(
                "{}Notes about this person:\n{}",
                p.map(|p| format!("{p}\n\n")).unwrap_or_default(),
                notes.join("\n")
            )),
        }
    }
}

/// The text after a leading `---` frontmatter block.
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---") else {
        return text;
    };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            after.strip_prefix('\n').unwrap_or(after)
        }
        None => text,
    }
}

/// A regex alternation of the query's words (3+ characters), most specific first.
fn search_pattern(query: &str) -> Option<String> {
    let mut words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    words.dedup();
    words.truncate(12);
    (!words.is_empty()).then(|| words.join("|"))
}

#[async_trait]
impl KnowledgeProvider for Knowledge {
    async fn context(&self, who: &Requester) -> Result<KnowledgeContext, KnowledgeError> {
        self.refresh().await;
        Ok(KnowledgeContext {
            persona: self.persona(),
            catalog: self.catalog().await?,
            user_profile: self.user_profile(who).await,
        })
    }

    async fn grant(
        &self,
        who: &Requester,
        run: &okbot_core::RunInfo<'_>,
    ) -> Result<RunGrant, KnowledgeError> {
        let (tools, ttl) = (run.tools, run.ttl);
        let Some(addr) = *lock(&self.mcp_addr) else {
            return Ok(RunGrant::none());
        };
        let context = tokens::RunContext {
            who: who.clone(),
            policy: if tools == ToolPolicy::McpOnly {
                "mcp-only"
            } else {
                "web"
            },
            attachments: run.attachments.to_vec(),
        };
        let (token, guard) = self.tokens.issue(scope::for_requester(who), context, ttl);
        Ok(RunGrant::new(
            vec![
                McpServer {
                    name: "kb".into(),
                    url: format!("http://{addr}{}", mcp::KB_PATH),
                    bearer_token_env: KB_TOKEN_ENV.into(),
                },
                McpServer {
                    name: "bot".into(),
                    url: format!("http://{addr}{}", mcp::BOT_PATH),
                    bearer_token_env: KB_TOKEN_ENV.into(),
                },
            ],
            vec![(KB_TOKEN_ENV.into(), Secret::new(token))],
            guard,
        ))
    }

    async fn excerpts(&self, who: &Requester, query: &str) -> Result<String, KnowledgeError> {
        let Some(pattern) = search_pattern(query) else {
            return Ok(String::new());
        };
        self.refresh().await;
        let bundle = self.bundle.clone();
        let scope = scope::for_requester(who);
        tokio::task::spawn_blocking(move || {
            let hits = bundle
                .grep(
                    &GrepRequest {
                        pattern,
                        files_only: true,
                        limit: 5,
                        ..GrepRequest::default()
                    },
                    &scope,
                )
                .map_err(bundle_error)?;
            let mut out = Vec::new();
            for doc in hits.docs.iter().take(3) {
                let request = GetRequest {
                    id: doc.id.clone(),
                    max_tokens: Some(700),
                    ..GetRequest::default()
                };
                if let Ok(got) = bundle.get(&request, &scope) {
                    out.push(format!(
                        "[{}] {}\n{}",
                        got.id,
                        got.title,
                        strip_frontmatter(&got.content).trim()
                    ));
                }
            }
            Ok(out.join("\n\n"))
        })
        .await
        .map_err(other_error)?
    }
}

#[async_trait]
impl MemoryStore for Knowledge {
    async fn save(&self, who: &Requester, note: MemoryNote) -> Result<MemoryEntry, MemoryError> {
        self.save_with_policy(who, note, "command").await
    }

    async fn list(&self, who: &Requester) -> Result<Vec<MemoryEntry>, MemoryError> {
        let notes = self.notes.clone();
        let requester = who.clone();
        tokio::task::spawn_blocking(move || notes.list(&requester))
            .await
            .map_err(|e| MemoryError::Backend(Box::new(e)))
    }

    async fn forget(&self, who: &Requester, id: &str) -> Result<MemoryEntry, MemoryError> {
        let _guard = self.writes.lock().await;
        let notes = self.notes.clone();
        let requester = who.clone();
        let target = id.to_string();
        let entry = tokio::task::spawn_blocking(move || notes.forget(&requester, &target, false))
            .await
            .map_err(|e| MemoryError::Backend(Box::new(e)))??;
        self.after_write(format!("memory: forget {}", entry.id))
            .await;
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_is_stripped() {
        assert_eq!(strip_frontmatter("---\na: 1\n---\nBody"), "Body");
        assert_eq!(strip_frontmatter("No frontmatter"), "No frontmatter");
    }

    #[test]
    fn search_patterns() {
        assert_eq!(
            search_pattern("Chính sách đổi trả là gì?").as_deref(),
            Some("chính|sách|đổi|trả")
        );
        assert_eq!(search_pattern("a b ?"), None);
    }
}
