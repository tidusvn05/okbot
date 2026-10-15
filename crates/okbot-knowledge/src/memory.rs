//! Notes written by the bot (design §5.4): one OKF document per note, complete to level L2, in
//! the requester's folder, with an `index.md` in every folder so the bundle keeps linting clean.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use okbot_core::{MemoryEntry, MemoryError, MemoryNote, MemoryScope, Requester};

use crate::paths;

/// Longest note, in characters.
pub(crate) const MAX_NOTE_CHARS: usize = 2000;

/// Notes in one bundle. Writes are serialized by the caller.
#[derive(Debug, Clone)]
pub(crate) struct Notes {
    root: PathBuf,
}

fn backend(e: impl std::error::Error + Send + Sync + 'static) -> MemoryError {
    MemoryError::Backend(Box::new(e))
}

/// `platform:instance/user`.
pub(crate) fn author_of(who: &Requester) -> String {
    format!("{}/{}", who.conversation.channel, who.user.id)
}

/// The folder (concept id prefix) holding `scope` notes for `who`.
pub(crate) fn folder(scope: MemoryScope, who: &Requester) -> String {
    let channel = &who.conversation.channel;
    match scope {
        MemoryScope::User => paths::owner_dir(who),
        MemoryScope::Room => paths::room_dir(channel, &who.conversation.room),
        MemoryScope::Global => "memory/global".to_string(),
    }
}

pub(crate) fn quote(text: &str) -> String {
    // A JSON string is a valid YAML double-quoted scalar.
    serde_json::Value::String(text.to_string()).to_string()
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(i) if i > max / 2 => format!("{}…", &cut[..i]),
        _ => format!("{cut}…"),
    }
}

/// `vi` when the text has Vietnamese letters, otherwise `en`.
pub(crate) fn language(text: &str) -> &'static str {
    let vietnamese = text.chars().any(|c| {
        matches!(
            c,
            'ă' | 'â' | 'đ' | 'ê' | 'ô' | 'ơ' | 'ư' | 'Ă' | 'Â' | 'Đ' | 'Ê' | 'Ô' | 'Ơ' | 'Ư'
        ) || ('\u{1EA0}'..='\u{1EF9}').contains(&c)
    });
    if vietnamese { "vi" } else { "en" }
}

/// A file-name slug from the title: ASCII letters and digits, Vietnamese letters folded.
pub(crate) fn slug(title: &str) -> String {
    let folded: String = title
        .chars()
        .map(|c| match c {
            'đ' | 'Đ' => 'd',
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            c if c.is_alphabetic() => fold_vietnamese(c),
            _ => '-',
        })
        .collect();
    let words: Vec<&str> = folded
        .split('-')
        .filter(|w| !w.is_empty())
        .take(6)
        .collect();
    if words.is_empty() {
        "note".into()
    } else {
        words.join("-")
    }
}

fn fold_vietnamese(c: char) -> char {
    const GROUPS: [(&str, char); 6] = [
        ("àáạảãâầấậẩẫăằắặẳẵÀÁẠẢÃÂẦẤẬẨẪĂẰẮẶẲẴ", 'a'),
        ("èéẹẻẽêềếệểễÈÉẸẺẼÊỀẾỆỂỄ", 'e'),
        ("ìíịỉĩÌÍỊỈĨ", 'i'),
        ("òóọỏõôồốộổỗơờớợởỡÒÓỌỎÕÔỒỐỘỔỖƠỜỚỢỞỠ", 'o'),
        ("ùúụủũưừứựửữÙÚỤỦŨƯỪỨỰỬỮ", 'u'),
        ("ỳýỵỷỹỲÝỴỶỸ", 'y'),
    ];
    GROUPS
        .iter()
        .find(|(set, _)| set.contains(c))
        .map_or('-', |(_, base)| *base)
}

fn scope_name(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::User => "user",
        MemoryScope::Room => "room",
        MemoryScope::Global => "global",
    }
}

fn scope_label(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::User => "this person",
        MemoryScope::Room => "this chat",
        MemoryScope::Global => "everyone",
    }
}

/// Writes `text` to `path` atomically (temporary file + rename).
pub(crate) fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl Notes {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn dir(&self, id_prefix: &str) -> PathBuf {
        self.root.join(id_prefix)
    }

    /// Writes a new note and refreshes the folder indexes. Returns the entry.
    pub(crate) fn save(
        &self,
        who: &Requester,
        note: &MemoryNote,
        now: Timestamp,
        run_policy: &str,
    ) -> Result<MemoryEntry, MemoryError> {
        let text = note.text.trim();
        if text.is_empty() {
            return Err(MemoryError::Invalid("the note is empty".into()));
        }
        if text.chars().count() > MAX_NOTE_CHARS {
            return Err(MemoryError::Invalid(format!(
                "the note is longer than {MAX_NOTE_CHARS} characters"
            )));
        }
        let title = note
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(|| truncate(text, 60), |t| truncate(t, 80));
        if note.scope == MemoryScope::User && !who.personal {
            return Err(MemoryError::Forbidden(paths::NOT_HERE.into()));
        }
        let folder = folder(note.scope, who);
        let dir = self.dir(&folder);
        std::fs::create_dir_all(&dir).map_err(backend)?;
        let day = now.strftime("%Y-%m-%d").to_string();
        let base = slug(&title);
        let mut name = base.clone();
        let mut n = 2;
        while dir.join(format!("{name}.md")).exists() {
            name = format!("{base}-{n}");
            n += 1;
        }
        let description = truncate(
            &format!("Remembered for {}: {text}", scope_label(note.scope)),
            160,
        );
        let author = author_of(who);
        let mut doc = String::new();
        let _ = writeln!(doc, "---");
        let _ = writeln!(doc, "type: Memory");
        let _ = writeln!(doc, "title: {}", quote(&title));
        let _ = writeln!(doc, "description: {}", quote(&description));
        let _ = writeln!(doc, "tags: [memory]");
        let _ = writeln!(doc, "lang: {}", language(text));
        let _ = writeln!(doc, "status: stable");
        let _ = writeln!(doc, "updated: {day}");
        let _ = writeln!(doc, "author: {}", quote(&author));
        let _ = writeln!(doc, "source: chat");
        let _ = writeln!(doc, "scope: {}", scope_name(note.scope));
        let _ = writeln!(doc, "run_policy: {run_policy}");
        let _ = writeln!(doc, "generated: {{by: okbot, at: \"{day}\"}}");
        let _ = writeln!(doc, "---\n\n{text}");
        write_atomic(&dir.join(format!("{name}.md")), &doc).map_err(backend)?;
        reindex(&self.root, &folder).map_err(backend)?;
        Ok(MemoryEntry {
            id: format!("{folder}/{name}"),
            scope: note.scope,
            title,
            text: text.to_string(),
            author: Some(author),
            updated: day,
        })
    }

    /// Notes in `folder` (not recursive), newest first.
    fn read_folder(&self, folder: &str, scope: MemoryScope) -> Vec<MemoryEntry> {
        let Ok(entries) = std::fs::read_dir(self.dir(folder)) else {
            return Vec::new();
        };
        let mut out: Vec<MemoryEntry> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
            .filter(|p| p.file_name().is_some_and(|n| n != "index.md"))
            .filter_map(|path| {
                let text = std::fs::read_to_string(&path).ok()?;
                let (fm, body) = okbase::core::Frontmatter::split(&text);
                let stem = path.file_stem()?.to_string_lossy().into_owned();
                Some(MemoryEntry {
                    id: format!("{folder}/{stem}"),
                    scope,
                    title: fm.get_str("title").unwrap_or(&stem).to_string(),
                    text: body.trim().to_string(),
                    author: fm.get_str("author").map(str::to_string),
                    updated: fm
                        .get("updated")
                        .map(|v| v.as_str().map_or_else(|| format!("{v:?}"), str::to_string))
                        .unwrap_or_default(),
                })
            })
            .collect();
        out.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
        out
    }

    /// Every note visible to `who`.
    pub(crate) fn list(&self, who: &Requester) -> Vec<MemoryEntry> {
        let mut out = Vec::new();
        for dir in paths::personal_dirs(who) {
            out.extend(self.read_folder(&dir, MemoryScope::User));
        }
        for scope in [MemoryScope::Room, MemoryScope::Global] {
            out.extend(self.read_folder(&folder(scope, who), scope));
        }
        out
    }

    /// Removes a visible note. Team notes only by their author unless `admin`.
    pub(crate) fn forget(
        &self,
        who: &Requester,
        id: &str,
        admin: bool,
    ) -> Result<MemoryEntry, MemoryError> {
        let entry = self.visible(who, id)?;
        if entry.scope == MemoryScope::Global
            && !admin
            && entry.author.as_deref() != Some(author_of(who).as_str())
        {
            return Err(MemoryError::Forbidden(
                "only the author or an operator can remove a team note".into(),
            ));
        }
        self.remove(&entry.id)?;
        Ok(entry)
    }

    /// Rewrites a visible note's text (and title) in place; same permissions as [`Self::forget`].
    pub(crate) fn update(
        &self,
        who: &Requester,
        id: &str,
        text: &str,
        title: Option<&str>,
        now: Timestamp,
    ) -> Result<MemoryEntry, MemoryError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(MemoryError::Invalid("the note is empty".into()));
        }
        if text.chars().count() > MAX_NOTE_CHARS {
            return Err(MemoryError::Invalid(format!(
                "the note is longer than {MAX_NOTE_CHARS} characters"
            )));
        }
        let entry = self.visible(who, id)?;
        if entry.scope == MemoryScope::Global
            && entry.author.as_deref() != Some(author_of(who).as_str())
        {
            return Err(MemoryError::Forbidden(
                "only the author can change a team note".into(),
            ));
        }
        let path = self.root.join(format!("{}.md", entry.id));
        let old = std::fs::read_to_string(&path).map_err(backend)?;
        let title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(|| entry.title.clone(), |t| truncate(t, 80));
        let description = truncate(
            &format!("Remembered for {}: {text}", scope_label(entry.scope)),
            160,
        );
        let day = now.strftime("%Y-%m-%d").to_string();
        // Rewrite the managed keys, keep every other frontmatter line as it is.
        let mut doc = String::new();
        let mut in_frontmatter = false;
        for (i, line) in old.lines().enumerate() {
            if line == "---" {
                if i == 0 {
                    in_frontmatter = true;
                } else if in_frontmatter {
                    doc.push_str("---\n\n");
                    doc.push_str(text);
                    doc.push('\n');
                    break;
                }
                doc.push_str("---\n");
                continue;
            }
            let replaced = if !in_frontmatter {
                None
            } else if line.starts_with("title:") {
                Some(format!("title: {}", quote(&title)))
            } else if line.starts_with("description:") {
                Some(format!("description: {}", quote(&description)))
            } else if line.starts_with("updated:") {
                Some(format!("updated: {day}"))
            } else if line.starts_with("lang:") {
                Some(format!("lang: {}", language(text)))
            } else {
                None
            };
            doc.push_str(replaced.as_deref().unwrap_or(line));
            doc.push('\n');
        }
        write_atomic(&path, &doc).map_err(backend)?;
        let folder = entry.id.rsplit_once('/').map_or("memory", |(f, _)| f);
        reindex(&self.root, folder).map_err(backend)?;
        Ok(MemoryEntry {
            title,
            text: text.to_string(),
            updated: day,
            ..entry
        })
    }

    /// A note `who` sees, by id.
    fn visible(&self, who: &Requester, id: &str) -> Result<MemoryEntry, MemoryError> {
        let id = id.trim().trim_start_matches('/').trim_end_matches(".md");
        if id
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return Err(MemoryError::NotFound(id.to_string()));
        }
        self.list(who)
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| MemoryError::NotFound(id.to_string()))
    }

    /// Deletes a note by id (no visibility check: operators only) and refreshes its index.
    pub(crate) fn remove(&self, id: &str) -> Result<(), MemoryError> {
        if !id.starts_with("memory/") || id.split('/').any(|s| s.is_empty() || s == "..") {
            return Err(MemoryError::NotFound(id.to_string()));
        }
        let path = self.root.join(format!("{id}.md"));
        std::fs::remove_file(&path).map_err(|_| MemoryError::NotFound(id.to_string()))?;
        let folder = id.rsplit_once('/').map_or("memory", |(f, _)| f);
        reindex(&self.root, folder).map_err(backend)
    }

    /// Every note in the bundle (operators).
    pub(crate) fn all(&self) -> Vec<MemoryEntry> {
        fn walk(notes: &Notes, folder: &str, scope: MemoryScope, out: &mut Vec<MemoryEntry>) {
            out.extend(notes.read_folder(folder, scope));
            if let Ok(entries) = std::fs::read_dir(notes.dir(folder)) {
                for e in entries.filter_map(Result::ok) {
                    if e.path().is_dir() {
                        let sub = format!("{folder}/{}", e.file_name().to_string_lossy());
                        walk(notes, &sub, scope, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(self, "memory/global", MemoryScope::Global, &mut out);
        walk(self, "memory/rooms", MemoryScope::Room, &mut out);
        walk(self, "memory/users", MemoryScope::User, &mut out);
        walk(self, "memory/people", MemoryScope::User, &mut out);
        out
    }
}

/// Rewrites `index.md` of `folder` and of every parent up to its area (`memory`, `tasks`, …),
/// so the bundle keeps linting clean.
pub(crate) fn reindex(root: &Path, folder: &str) -> std::io::Result<()> {
    let mut current = folder.to_string();
    loop {
        write_index(root, &current)?;
        match current.rsplit_once('/') {
            Some((parent, _)) => current = parent.to_string(),
            None => return Ok(()),
        }
    }
}

pub(crate) fn write_index(root: &Path, folder: &str) -> std::io::Result<()> {
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir)?;
    let mut docs = Vec::new();
    let mut subdirs = Vec::new();
    for entry in std::fs::read_dir(&dir)?.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            subdirs.push(name);
        } else if path.extension().is_some_and(|x| x == "md") && name != "index.md" {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let (fm, _) = okbase::core::Frontmatter::split(&text);
            let title = fm.get_str("title").unwrap_or(&name).to_string();
            let description = fm.get_str("description").unwrap_or("").to_string();
            docs.push((name, title, description));
        }
    }
    docs.sort();
    subdirs.sort();
    let mut out = format!("# Index: {folder}\n\n");
    if docs.is_empty() && subdirs.is_empty() {
        out.push_str(match folder.split('/').next() {
            Some("tasks") => "No tasks yet.\n",
            Some("knowledge") => "No documents yet.\n",
            _ => "No notes yet.\n",
        });
    }
    if !docs.is_empty() {
        out.push_str("## Documents\n");
        for (file, title, description) in &docs {
            let _ = writeln!(out, "* [{title}]({file}) - {description}");
        }
    }
    if !subdirs.is_empty() {
        out.push_str(if docs.is_empty() {
            "## Subdirectories\n"
        } else {
            "\n## Subdirectories\n"
        });
        for sub in &subdirs {
            let _ = writeln!(out, "* [{sub}](/{folder}/{sub}/index.md)");
        }
    }
    write_atomic(&dir.join("index.md"), &out)
}

/// Commits memory and task changes when the bundle is a git repository (operators can review and revert).
pub(crate) fn git_commit(root: &Path, message: &str) {
    if !root.join(".git").exists() {
        return;
    }
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=okbot", "-c", "user.email=okbot@localhost"])
            .args(args)
            .output()
    };
    // Only the folders the bot writes; `tasks` may not exist yet in older bundles.
    let areas: Vec<&str> = ["memory", "tasks", "notes", "knowledge/inbox"]
        .into_iter()
        .filter(|a| root.join(a).exists())
        .collect();
    let mut add = vec!["add", "-A", "--"];
    add.extend(&areas);
    let mut commit = vec!["commit", "-q", "-m", message, "--"];
    commit.extend(&areas);
    let added = git(&add);
    let committed = git(&commit);
    if let Err(error) = added.and(committed) {
        tracing::warn!(%error, "cannot commit memory changes");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_languages() {
        assert_eq!(slug("Thích trà sữa trân châu"), "thich-tra-sua-tran-chau");
        assert_eq!(slug("Đơn hàng #42!"), "don-hang-42");
        assert_eq!(slug("???"), "note");
        assert_eq!(language("Tôi thích trà"), "vi");
        assert_eq!(language("likes tea"), "en");
    }

    #[test]
    fn update_rewrites_text_and_keeps_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let notes = Notes::new(dir.path());
        let who = Requester {
            conversation: okbot_core::ConversationKey::room("zalo:shop".parse().unwrap(), "dm"),
            user: okbot_core::UserRef {
                id: "ana".into(),
                display_name: "Ana".into(),
                is_bot: false,
            },
            internal: false,
            person: None,
            personal: true,
        };
        let now: Timestamp = "2026-10-13T07:00:00Z".parse().unwrap();
        let note = MemoryNote {
            scope: MemoryScope::User,
            title: Some("Coffee".into()),
            text: "Likes black coffee".into(),
        };
        let saved = notes.save(&who, &note, now, "web").unwrap();
        let updated = notes
            .update(&who, &saved.id, "Likes black coffee, no sugar", None, now)
            .unwrap();
        assert_eq!(updated.id, saved.id);
        let text = std::fs::read_to_string(dir.path().join(format!("{}.md", saved.id))).unwrap();
        assert!(text.contains("run_policy: web"), "{text}");
        assert!(
            text.ends_with("---\n\nLikes black coffee, no sugar\n"),
            "{text}"
        );
        assert_eq!(notes.list(&who)[0].text, "Likes black coffee, no sugar");
    }

    #[test]
    fn truncates_on_words() {
        assert_eq!(truncate("a  b\nc", 10), "a b c");
        assert_eq!(truncate("one two three four", 9), "one two…");
    }
}
