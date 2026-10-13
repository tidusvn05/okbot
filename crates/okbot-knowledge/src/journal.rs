//! Quick notes and the journal (design §5.9): one OKF document per owner and day under `notes/`,
//! each note a timestamped line.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use okbot_core::{MemoryError, MemoryScope, Requester};

use crate::memory::{MAX_NOTE_CHARS, author_of, language, quote, reindex, write_atomic};
use crate::{areas, paths};

fn backend(e: impl std::error::Error + Send + Sync + 'static) -> MemoryError {
    MemoryError::Backend(Box::new(e))
}

/// The journal of one bundle. Writes are serialized by the caller.
#[derive(Debug, Clone)]
pub(crate) struct Journal {
    root: PathBuf,
    tz: TimeZone,
}

impl Journal {
    pub(crate) fn new(root: &Path, tz: TimeZone) -> Self {
        Self {
            root: root.to_path_buf(),
            tz,
        }
    }

    /// Appends `text` to today's notes of `who` (or of the room). Returns the document id.
    pub(crate) fn add(
        &self,
        who: &Requester,
        scope: MemoryScope,
        text: &str,
        now: Timestamp,
    ) -> Result<String, MemoryError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(MemoryError::Invalid("the note is empty".into()));
        }
        if text.chars().count() > MAX_NOTE_CHARS {
            return Err(MemoryError::Invalid(format!(
                "the note is longer than {MAX_NOTE_CHARS} characters"
            )));
        }
        let owner = match scope {
            MemoryScope::User => paths::owner_dir(who),
            MemoryScope::Room => paths::room_dir(&who.conversation.channel, &who.conversation.room),
            MemoryScope::Global => {
                return Err(MemoryError::Invalid(
                    "notes are personal (`user`) or for this chat (`room`)".into(),
                ));
            }
        };
        let local = now.to_zoned(self.tz.clone());
        let day = local.strftime("%Y-%m-%d").to_string();
        let folder = format!(
            "{}/{}",
            paths::in_area("notes", &owner),
            local.strftime("%Y")
        );
        let path = self.root.join(&folder).join(format!("{day}.md"));
        std::fs::create_dir_all(path.parent().unwrap_or(&self.root)).map_err(backend)?;
        let mut doc = match std::fs::read_to_string(&path) {
            Ok(existing) => existing,
            Err(_) => {
                let mut doc = String::new();
                let _ = writeln!(doc, "---");
                let _ = writeln!(doc, "type: Journal");
                let _ = writeln!(doc, "title: {}", quote(&format!("Notes {day}")));
                let _ = writeln!(
                    doc,
                    "description: {}",
                    quote(&format!("Quick notes written on {day}."))
                );
                let _ = writeln!(doc, "tags: [journal]");
                // The day's first note decides; okbase needs one language per document.
                let _ = writeln!(doc, "lang: {}", language(text));
                let _ = writeln!(doc, "status: stable");
                let _ = writeln!(doc, "updated: {day}");
                let _ = writeln!(doc, "author: {}", quote(&author_of(who)));
                let _ = writeln!(
                    doc,
                    "scope: {}",
                    if scope == MemoryScope::Room {
                        "room"
                    } else {
                        "user"
                    }
                );
                let _ = writeln!(doc, "generated: {{by: okbot, at: \"{day}\"}}");
                let _ = writeln!(doc, "---\n\n# Notes {day}\n");
                doc
            }
        };
        if !doc.ends_with('\n') {
            doc.push('\n');
        }
        // Later lines of a note are indented under its bullet.
        let body = text.lines().collect::<Vec<_>>().join("\n  ");
        let _ = writeln!(doc, "- {} {body}", local.strftime("%H:%M"));
        write_atomic(&path, &doc).map_err(backend)?;
        reindex(&self.root, &folder).map_err(backend)?;
        areas::ensure(&self.root, &areas::NOTES).map_err(backend)?;
        Ok(format!("{folder}/{day}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{ConversationKey, UserRef};

    #[test]
    fn appends_to_the_day_file() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let journal = Journal::new(dir.path(), TimeZone::get("Asia/Ho_Chi_Minh").unwrap());
        let who = Requester {
            conversation: ConversationKey::room("zalo:me".parse().unwrap(), "dm"),
            user: UserRef {
                id: "u1".into(),
                display_name: "Ana".into(),
                is_bot: false,
            },
            internal: false,
            person: None,
        };
        let now: Timestamp = "2026-10-14T02:05:00Z".parse().unwrap();
        let id = journal
            .add(&who, MemoryScope::User, "Gặp anh Minh về dự án X", now)
            .unwrap();
        assert_eq!(id, "notes/users/zalo-me/u1/2026/2026-10-14");
        journal
            .add(&who, MemoryScope::User, "Gửi báo giá\ntrước thứ 6", now)
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join(format!("{id}.md"))).unwrap();
        assert!(
            text.ends_with("- 09:05 Gặp anh Minh về dự án X\n- 09:05 Gửi báo giá\n  trước thứ 6\n"),
            "{text}"
        );
        assert_eq!(text.matches("type: Journal").count(), 1);
        assert!(journal.add(&who, MemoryScope::Global, "x", now).is_err());
    }
}
