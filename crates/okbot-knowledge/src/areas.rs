//! Folders the bot writes besides memory (design §5.5), and bringing bundles created before they
//! existed up to date: their tag in the vocabulary and their link in the parent index.

use std::path::Path;

use crate::memory::write_atomic;

/// A folder the bot writes into.
pub(crate) struct Area {
    /// Folder, relative to the bundle (`tasks`, `knowledge/inbox`).
    pub(crate) path: &'static str,
    /// Line in the parent folder's index.
    pub(crate) link: &'static str,
    /// The vocabulary term its documents are tagged with, as YAML under `terms:`.
    pub(crate) term: &'static str,
    /// The term's name, as it appears in the vocabulary (`\n  task:`).
    pub(crate) term_key: &'static str,
}

/// `tasks/`.
pub(crate) const TASKS: Area = Area {
    path: "tasks",
    link: "* [tasks](/tasks/index.md) - To-dos, reminders and routines.",
    term: "  task:\n    synonyms: [việc cần làm, nhắc nhở, to-do, reminder]\n    facet: topic\n    \
           description: To-dos, reminders and routines.\n",
    term_key: "\n  task:",
};

/// `notes/`.
pub(crate) const NOTES: Area = Area {
    path: "notes",
    link: "* [notes](/notes/index.md) - Quick notes and journal, one file per day.",
    term: "  journal:\n    synonyms: [ghi chú, nhật ký, note, notes]\n    facet: topic\n    \
           description: Quick notes and the journal.\n",
    term_key: "\n  journal:",
};

/// `knowledge/inbox/`.
pub(crate) const INBOX: Area = Area {
    path: "knowledge/inbox",
    link: "* [inbox](inbox/index.md) - Documents saved from chat, to sort into folders.",
    term: "  inbox:\n    synonyms: [tài liệu đã lưu, saved, lưu lại]\n    facet: topic\n    \
           description: Documents saved from chat.\n",
    term_key: "\n  inbox:",
};

/// Adds the area's tag to the vocabulary and its link to the parent index when missing.
pub(crate) fn ensure(root: &Path, area: &Area) -> std::io::Result<()> {
    let vocabulary = root.join("_meta/vocabulary.md");
    if let Ok(text) = std::fs::read_to_string(&vocabulary)
        && !text.contains(area.term_key)
        && let Some(at) = text.find("terms:\n")
    {
        let at = at + "terms:\n".len();
        write_atomic(
            &vocabulary,
            &format!("{}{}{}", &text[..at], area.term, &text[at..]),
        )?;
    }
    let parent = Path::new(area.path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| root.join("index.md"), |p| root.join(p).join("index.md"));
    let Ok(text) = std::fs::read_to_string(&parent) else {
        return Ok(());
    };
    let target = area
        .link
        .split_once("](")
        .and_then(|(_, rest)| rest.split_once(')'));
    if target.is_some_and(|(href, _)| text.contains(href)) {
        return Ok(());
    }
    let mut text = text.trim_end().to_string();
    if parent == root.join("index.md") && !text.contains("## Subdirectories") {
        text.push_str("\n\n## Subdirectories");
    }
    text.push('\n');
    text.push_str(area.link);
    text.push('\n');
    write_atomic(&parent, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_bundles_get_tags_and_links_once() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let vocabulary = dir.path().join("_meta/vocabulary.md");
        let text = std::fs::read_to_string(&vocabulary).unwrap();
        let stripped = [TASKS.term, NOTES.term, INBOX.term]
            .iter()
            .fold(text, |t, term| t.replace(term, ""));
        std::fs::write(&vocabulary, stripped).unwrap();
        for index in ["index.md", "knowledge/index.md"] {
            let path = dir.path().join(index);
            let text = std::fs::read_to_string(&path).unwrap();
            let kept: Vec<&str> = text
                .lines()
                .filter(|l| {
                    !l.contains("/tasks/") && !l.contains("/notes/") && !l.contains("inbox/")
                })
                .collect();
            std::fs::write(&path, kept.join("\n")).unwrap();
        }
        for _ in 0..2 {
            for area in [&TASKS, &NOTES, &INBOX] {
                ensure(dir.path(), area).unwrap();
            }
        }
        let text = std::fs::read_to_string(&vocabulary).unwrap();
        for key in [TASKS.term_key, NOTES.term_key, INBOX.term_key] {
            assert_eq!(text.matches(key).count(), 1, "{key}: {text}");
        }
        let root = std::fs::read_to_string(dir.path().join("index.md")).unwrap();
        assert_eq!(root.matches("/tasks/index.md").count(), 1, "{root}");
        assert_eq!(root.matches("/notes/index.md").count(), 1, "{root}");
        let knowledge = std::fs::read_to_string(dir.path().join("knowledge/index.md")).unwrap();
        assert_eq!(
            knowledge.matches("inbox/index.md").count(),
            1,
            "{knowledge}"
        );
    }
}
