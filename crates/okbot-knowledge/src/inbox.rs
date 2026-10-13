//! Documents saved from chat or the terminal into `knowledge/inbox/` (design §5.8): a markdown
//! document, and the original file next to it when there is one (okbase reads PDF, Word, … itself).

use std::fmt::Write as _;
use std::path::Path;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Map, Value};

use rmcp::model::Tool;
use serde_json::json;

use crate::bot_mcp::{fetch, str_arg, tool};
use crate::memory::{language, quote, slug, truncate, write_atomic, write_index};
use crate::tokens::RunContext;
use crate::{Knowledge, areas};

/// Largest text saved, in bytes.
pub(crate) const MAX_TEXT_BYTES: usize = 2 << 20;
/// Largest file saved, in bytes.
pub(crate) const MAX_FILE_BYTES: usize = 20 << 20;

/// The `note_add` and `knowledge_add` tool definitions.
pub(crate) fn tools() -> Vec<Tool> {
    vec![
        tool(
            "note_add",
            "Append a quick note to today's journal (one line with the time).",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The note, as the user said it."},
                    "scope": {"type": "string", "enum": ["user", "room"],
                              "description": "user (default): the person's own; room: this chat's."}
                },
                "required": ["text"]
            }),
        ),
        tool(
            "knowledge_add",
            "Save a document to the knowledge base (owner only): text, a web page you read, or an attached file.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "text": {"type": "string", "description": "Markdown content, or a summary when a file is attached."},
                    "attachment": {"type": "integer", "minimum": 1,
                                   "description": "Number of the attached image or file to keep (from the message)."},
                    "source_url": {"type": "string", "description": "The page it came from, if any."}
                },
                "required": ["title"]
            }),
        ),
    ]
}

/// A document to save.
#[derive(Debug, Clone, Default)]
pub struct NewDocument {
    /// Title.
    pub title: String,
    /// Markdown text: the content, a summary or a transcription of the file.
    pub text: String,
    /// The original file and its extension (`pdf`), stored next to the document.
    pub file: Option<(Vec<u8>, String)>,
    /// Where it came from (a URL), when known.
    pub source: Option<String>,
    /// Who added it (`platform:instance/user`, or `operator`).
    pub added_by: String,
}

/// The file extension okbase can read for a file `name` or media type.
pub fn readable_extension(name: Option<&str>, mime: &str) -> Option<String> {
    let from_name = name
        .and_then(|n| Path::new(n).extension())
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let from_mime = match mime {
        "application/pdf" => Some("pdf"),
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "text/html" => Some("html"),
        "text/plain" => Some("txt"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        _ => None,
    }
    .map(str::to_string);
    from_name
        .into_iter()
        .chain(from_mime)
        .find(|ext| okbase::is_source_path(&format!("x.{ext}")))
}

/// Writes `doc` into `knowledge/inbox/` and returns its id. Blocking.
pub(crate) fn save(
    root: &Path,
    tz: &TimeZone,
    doc: &NewDocument,
    now: Timestamp,
) -> Result<String, String> {
    let title = doc.title.trim();
    if title.is_empty() {
        return Err("the document needs a title".into());
    }
    if doc.text.trim().is_empty() && doc.file.is_none() {
        return Err("give the text to save, or attach a file".into());
    }
    if doc.text.len() > MAX_TEXT_BYTES {
        return Err("the text is longer than 2 MiB".into());
    }
    if doc
        .file
        .as_ref()
        .is_some_and(|(b, _)| b.len() > MAX_FILE_BYTES)
    {
        return Err("the file is larger than 20 MiB".into());
    }
    let folder = areas::INBOX.path;
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let day = now.to_zoned(tz.clone()).strftime("%Y-%m-%d").to_string();
    let base = format!("{day}-{}", slug(title));
    let taken = |name: &str| {
        dir.join(format!("{name}.md")).exists()
            || doc
                .file
                .as_ref()
                .is_some_and(|(_, ext)| dir.join(format!("{name}.{ext}")).exists())
    };
    let mut name = base.clone();
    let mut n = 2;
    while taken(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    let file_name = doc.file.as_ref().map(|(_, ext)| format!("{name}.{ext}"));
    if let (Some((bytes, _)), Some(file_name)) = (&doc.file, &file_name) {
        std::fs::write(dir.join(file_name), bytes).map_err(|e| e.to_string())?;
    }
    let text = doc.text.trim();
    let summary = if text.is_empty() {
        format!("Saved file {}", file_name.as_deref().unwrap_or(""))
    } else {
        text.to_string()
    };
    let mut md = String::new();
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "type: Document");
    let _ = writeln!(md, "title: {}", quote(title));
    let _ = writeln!(md, "description: {}", quote(&truncate(&summary, 160)));
    let _ = writeln!(md, "tags: [inbox]");
    let _ = writeln!(md, "lang: {}", language(&format!("{title} {text}")));
    let _ = writeln!(md, "status: stable");
    let _ = writeln!(md, "updated: {day}");
    let _ = writeln!(
        md,
        "source: {}",
        quote(doc.source.as_deref().unwrap_or("chat"))
    );
    let _ = writeln!(md, "added_by: {}", quote(&doc.added_by));
    let _ = writeln!(md, "generated: {{by: okbot, at: \"{day}\"}}");
    let _ = writeln!(md, "---\n\n# {title}\n");
    if !text.is_empty() {
        let _ = writeln!(md, "{text}\n");
    }
    if let Some(file_name) = &file_name {
        let _ = writeln!(md, "Original file: [{file_name}]({file_name})");
    }
    write_atomic(&dir.join(format!("{name}.md")), &md).map_err(|e| e.to_string())?;
    // Only the inbox index: `knowledge/index.md` belongs to the operator.
    write_index(root, folder).map_err(|e| e.to_string())?;
    areas::ensure(root, &areas::INBOX).map_err(|e| e.to_string())?;
    Ok(format!("{folder}/{name}"))
}

/// The `knowledge_add` tool: owner and administrators only.
pub(crate) async fn knowledge_add(
    k: &Knowledge,
    context: &RunContext,
    args: &Map<String, Value>,
) -> Result<String, String> {
    if !context.admin {
        return Err(
            "only the owner of this assistant (or an operator) can add to the \
                    knowledge base, which everyone using the bot can read"
                .into(),
        );
    }
    let mut doc = NewDocument {
        title: str_arg(args, "title").unwrap_or_default().to_string(),
        text: str_arg(args, "text").unwrap_or_default().to_string(),
        file: None,
        source: str_arg(args, "source_url").map(str::to_string),
        added_by: crate::memory::author_of(&context.who),
    };
    if let Some(index) = args.get("attachment").and_then(Value::as_u64) {
        let attachment = usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| context.attachments.get(i))
            .ok_or_else(|| format!("there is no attachment {index} in this message"))?;
        let (url, name) = match attachment {
            okbot_core::Attachment::Image { url, .. } => (url, None),
            okbot_core::Attachment::File { url, name } => (url, name.as_deref()),
            _ => return Err(format!("attachment {index} cannot be saved")),
        };
        let fetched = fetch(url, false, MAX_FILE_BYTES).await?;
        let ext = readable_extension(name, &fetched.mime).ok_or_else(|| {
            "this kind of file cannot be searched; send a PDF, Word, PowerPoint, HTML, text or \
             image file, or paste the text"
                .to_string()
        })?;
        doc.file = Some((fetched.bytes, ext));
    }
    let id = k.add_document(doc).await.map_err(|e| e.to_string())?;
    Ok(format!(
        "Saved as {id}; it can be searched now. The operator may move it into a folder later."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_text_and_files_without_touching_the_knowledge_index() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let tz = TimeZone::get("Asia/Ho_Chi_Minh").unwrap();
        let now: Timestamp = "2026-10-14T02:00:00Z".parse().unwrap();
        let before = std::fs::read_to_string(dir.path().join("knowledge/index.md")).unwrap();
        let doc = NewDocument {
            title: "Chính sách đổi trả".into(),
            text: "Đổi trả trong 7 ngày.".into(),
            source: Some("https://example.com/doi-tra".into()),
            added_by: "zalo:me/u1".into(),
            ..NewDocument::default()
        };
        let id = save(dir.path(), &tz, &doc, now).unwrap();
        assert_eq!(id, "knowledge/inbox/2026-10-14-chinh-sach-doi-tra");
        let again = save(dir.path(), &tz, &doc, now).unwrap();
        assert_eq!(again, "knowledge/inbox/2026-10-14-chinh-sach-doi-tra-2");
        let file = NewDocument {
            title: "Menu".into(),
            file: Some((b"%PDF-1.4".to_vec(), "pdf".into())),
            added_by: "operator".into(),
            ..NewDocument::default()
        };
        let id = save(dir.path(), &tz, &file, now).unwrap();
        assert!(dir.path().join(format!("{id}.pdf")).exists());
        let md = std::fs::read_to_string(dir.path().join(format!("{id}.md"))).unwrap();
        assert!(
            md.contains("Original file: [2026-10-14-menu.pdf](2026-10-14-menu.pdf)"),
            "{md}"
        );
        let after = std::fs::read_to_string(dir.path().join("knowledge/index.md")).unwrap();
        assert_eq!(before, after, "the operator's index is kept");
        let inbox = std::fs::read_to_string(dir.path().join("knowledge/inbox/index.md")).unwrap();
        assert!(inbox.contains("Chính sách đổi trả"), "{inbox}");
        assert!(save(dir.path(), &tz, &NewDocument::default(), now).is_err());
    }

    #[tokio::test]
    async fn the_tool_is_for_administrators() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let k = Knowledge::open(crate::KnowledgeOptions::new(dir.path())).unwrap();
        let mut context = RunContext {
            who: okbot_core::Requester {
                conversation: okbot_core::ConversationKey::room("zalo:me".parse().unwrap(), "dm"),
                user: okbot_core::UserRef {
                    id: "u1".into(),
                    display_name: "Ana".into(),
                    is_bot: false,
                },
                internal: false,
                person: None,
            },
            policy: "web",
            attachments: Vec::new(),
            admin: false,
        };
        let args = json!({"title": "Wifi", "text": "The guest wifi password is on the fridge."});
        let Value::Object(args) = args else {
            unreachable!("test arguments are objects")
        };
        let refused = knowledge_add(&k, &context, &args).await.unwrap_err();
        assert!(refused.contains("only the owner"), "{refused}");
        context.admin = true;
        let saved = knowledge_add(&k, &context, &args).await.unwrap();
        assert!(saved.contains("knowledge/inbox/"), "{saved}");
        let missing = json!({"title": "x", "attachment": 1});
        let Value::Object(missing) = missing else {
            unreachable!("test arguments are objects")
        };
        let error = knowledge_add(&k, &context, &missing).await.unwrap_err();
        assert!(error.contains("no attachment 1"), "{error}");
    }

    #[test]
    fn readable_extensions() {
        assert_eq!(
            readable_extension(Some("Báo giá.PDF"), ""),
            Some("pdf".into())
        );
        assert_eq!(readable_extension(None, "image/jpeg"), Some("jpg".into()));
        assert_eq!(
            readable_extension(Some("x.exe"), "application/octet-stream"),
            None
        );
    }
}
