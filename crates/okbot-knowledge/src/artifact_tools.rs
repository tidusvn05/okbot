//! The agent's artifact tools on the `bot` server (ADR 0021): save, list and read artifacts.

use okbot_core::{ArtifactKind, ArtifactStore as _, MemoryScope, NewArtifact, Requester};
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::Knowledge;
use crate::bot_mcp::{str_arg, tool};

/// Content returned by `artifact_get`, at most.
const MAX_GET_BYTES: usize = 64 * 1024;

/// What the `bot` server's instructions say about artifacts.
pub(crate) const INSTRUCTIONS: &str = "- artifact_save: when your answer is a page, a document, a \
table or a diagram the user will keep, reuse or print, or is longer than a chat message should be. \
Write one complete file: for html, one page with inline CSS and JS and no external URLs (they are \
blocked); mermaid for diagrams. To change one, save again with its id: that adds a version. Then \
answer with two or three lines about it; the bot adds the button that opens it.\n\
- artifact_list / artifact_get: find and read artifacts made before (to change or build on one).";

/// The artifact tools' definitions.
pub(crate) fn tools() -> Vec<Tool> {
    vec![
        tool(
            "artifact_save",
            "Save a page, document, table or diagram as an artifact the person opens from chat; with `id`, save a new version of it.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Short, in the user's language (at most 80 characters). With `id`, empty keeps the title."},
                    "kind": {"type": "string", "enum": ArtifactKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                             "description": "html: one self-contained page; markdown; svg; mermaid: diagram source; csv; json; text."},
                    "content": {"type": "string", "description": "The whole file."},
                    "summary": {"type": "string", "description": "What it is, in one to three sentences (found later by search)."},
                    "scope": {"type": "string", "enum": ["user", "room"],
                              "description": "user (default in a direct chat): the person's own; room (default in a group): shared with this chat."},
                    "id": {"type": "string", "description": "An artifact's id (`a-…`) to save a new version of."},
                    "note": {"type": "string", "description": "With `id`: what changed."}
                },
                "required": ["content"]
            }),
        ),
        tool(
            "artifact_list",
            "List the person's and this chat's artifacts with their ids, newest first.",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "Optional words in the title or summary."}}
            }),
        ),
        tool(
            "artifact_get",
            "Read one artifact's content (the newest version unless `version` is given).",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "version": {"type": "integer", "minimum": 1}
                },
                "required": ["id"]
            }),
        ),
    ]
}

/// Runs an artifact tool for `who`.
pub(crate) async fn call(
    k: &Knowledge,
    who: &Requester,
    name: &str,
    args: &Map<String, Value>,
) -> Result<String, String> {
    match name {
        "artifact_save" => {
            let id = str_arg(args, "id").map(str::trim).filter(|s| !s.is_empty());
            let kind = match str_arg(args, "kind") {
                Some(k) => k.parse::<ArtifactKind>()?,
                None if id.is_some() => ArtifactKind::Text,
                None => return Err("`kind` is required for a new artifact".into()),
            };
            let scope = match str_arg(args, "scope") {
                Some("user") => MemoryScope::User,
                Some("room") => MemoryScope::Room,
                Some(_) => return Err("scope must be user or room".into()),
                None if who.personal => MemoryScope::User,
                None => MemoryScope::Room,
            };
            let new = NewArtifact {
                id: id.map(str::to_string),
                title: str_arg(args, "title").unwrap_or_default().to_string(),
                kind,
                summary: str_arg(args, "summary").unwrap_or_default().to_string(),
                content: str_arg(args, "content").unwrap_or_default().to_string(),
                scope,
                note: str_arg(args, "note").unwrap_or_default().to_string(),
            };
            let a = k.save(who, new).await.map_err(|e| e.to_string())?;
            Ok(format!(
                "Saved {} v{} ({}, {} bytes): \"{}\". The bot adds an Open button to your answer; \
                 do not paste the content again.",
                a.short_id(),
                a.latest().n,
                a.kind,
                a.latest().bytes,
                a.title
            ))
        }
        "artifact_list" => {
            let words: Vec<String> = str_arg(args, "query")
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
            let list = k.list(who).await.map_err(|e| e.to_string())?;
            let lines: Vec<String> = list
                .iter()
                .filter(|a| {
                    let text = format!("{} {}", a.title, a.summary).to_lowercase();
                    words.iter().all(|w| text.contains(w.as_str()))
                })
                .map(|a| {
                    format!(
                        "- {} [{}, v{}, {}] {}: {}",
                        a.short_id(),
                        a.kind,
                        a.latest().n,
                        if a.scope == MemoryScope::Room {
                            "room"
                        } else {
                            "user"
                        },
                        a.title,
                        a.summary
                    )
                })
                .collect();
            if lines.is_empty() {
                return Ok("No artifacts.".into());
            }
            Ok(lines.join("\n"))
        }
        "artifact_get" => {
            let id = str_arg(args, "id").unwrap_or_default();
            let version = args
                .get("version")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok());
            let (a, n, content) = k.open(who, id, version).await.map_err(|e| e.to_string())?;
            let mut text = format!("{} v{n} ({}): {}\n\n", a.short_id(), a.kind, a.title);
            if content.len() > MAX_GET_BYTES {
                let mut cut = MAX_GET_BYTES;
                while !content.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.push_str(&content[..cut]);
                text.push_str("\n\n[cut: the rest is longer than 64 KB]");
            } else {
                text.push_str(&content);
            }
            Ok(text)
        }
        other => Err(format!("unknown tool {other}")),
    }
}
