//! Tasks written by the bot (design §5.6): one OKF document per task under `tasks/`, in the same
//! owner folders as memory. The task's life is `state` (OKF reserves `status` for documents).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use okbot_core::{
    ConversationKey, MemoryScope, NewTask, Repeat, Requester, Task, TaskChange, TaskError,
    TaskKind, TaskStatus, format_time, sort_tasks,
};

use crate::memory::{author_of, language, quote, reindex, truncate, write_atomic};
use crate::paths;

/// Most active tasks in one owner folder.
pub(crate) const MAX_ACTIVE: usize = 200;
/// Longest title, in characters.
const MAX_TITLE_CHARS: usize = 200;
/// Longest notes (a routine's prompt), in characters.
const MAX_NOTES_CHARS: usize = 4000;
/// Times this close to now still count as now (the agent computes them a moment earlier).
const PAST_SLACK: jiff::SignedDuration = jiff::SignedDuration::from_secs(120);

fn backend(e: impl std::error::Error + Send + Sync + 'static) -> TaskError {
    TaskError::Backend(Box::new(e))
}

/// Tasks in one bundle. Writes are serialized by the caller.
#[derive(Debug, Clone)]
pub(crate) struct Tasks {
    root: PathBuf,
    tz: TimeZone,
}

/// A fresh id: `t-` and six characters without look-alikes.
fn new_id() -> String {
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    let bytes = rand::random::<[u8; 6]>();
    let tail: String = bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect();
    format!("t-{tail}")
}

/// The folder holding `scope` tasks for `who`.
fn folder(scope: MemoryScope, who: &Requester) -> Result<String, TaskError> {
    match scope {
        MemoryScope::User => Ok(paths::in_area("tasks", &paths::owner_dir(who))),
        MemoryScope::Room => Ok(paths::in_area(
            "tasks",
            &paths::room_dir(&who.conversation.channel, &who.conversation.room),
        )),
        MemoryScope::Global => Err(TaskError::Invalid(
            "tasks are personal (`user`) or for this chat (`room`)".into(),
        )),
    }
}

/// Every folder whose tasks `who` sees, with the scope they belong to.
fn visible_folders(who: &Requester) -> Vec<(String, MemoryScope)> {
    let mut out: Vec<(String, MemoryScope)> = paths::personal_dirs(who)
        .iter()
        .map(|d| (paths::in_area("tasks", d), MemoryScope::User))
        .collect();
    out.push((
        paths::in_area(
            "tasks",
            &paths::room_dir(&who.conversation.channel, &who.conversation.room),
        ),
        MemoryScope::Room,
    ));
    out
}

fn scope_name(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Room => "room",
        _ => "user",
    }
}

fn check_text(title: &str, notes: &str) -> Result<(), TaskError> {
    if title.trim().is_empty() {
        return Err(TaskError::Invalid("the task needs a title".into()));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(TaskError::Invalid(format!(
            "the title is longer than {MAX_TITLE_CHARS} characters"
        )));
    }
    if notes.chars().count() > MAX_NOTES_CHARS {
        return Err(TaskError::Invalid(format!(
            "the notes are longer than {MAX_NOTES_CHARS} characters"
        )));
    }
    Ok(())
}

/// A task's firing rules hold together (checked on create and on every change).
fn check_schedule(task: &Task, now: Timestamp, fresh_time: bool) -> Result<(), TaskError> {
    if fresh_time && task.status.is_active() && task.remind_at.is_some_and(|t| t < now - PAST_SLACK)
    {
        return Err(TaskError::Invalid(
            "the reminder time is in the past; use a future time".into(),
        ));
    }
    match task.kind {
        TaskKind::Reminder if task.remind_at.is_none() && task.status.is_active() => Err(
            TaskError::Invalid("a reminder needs `remind_at` or `repeat`".into()),
        ),
        TaskKind::Routine if task.notes.trim().is_empty() => Err(TaskError::Invalid(
            "a routine needs a prompt in `notes`".into(),
        )),
        TaskKind::Routine if task.repeat.is_none() && task.remind_at.is_none() => Err(
            TaskError::Invalid("a routine needs `remind_at` or `repeat`".into()),
        ),
        _ => Ok(()),
    }
}

impl Tasks {
    pub(crate) fn new(root: &Path, tz: TimeZone) -> Self {
        Self {
            root: root.to_path_buf(),
            tz,
        }
    }

    /// The bot's time zone.
    pub(crate) fn tz(&self) -> &TimeZone {
        &self.tz
    }

    fn render(&self, task: &Task) -> String {
        let tz = &self.tz;
        let day = task.created.to_zoned(tz.clone()).strftime("%Y-%m-%d");
        let mut when = Vec::new();
        if let Some(r) = &task.repeat {
            when.push(r.to_string());
        } else if let Some(t) = task.remind_at {
            when.push(format!("at {}", format_time(t, tz)));
        }
        if let Some(d) = task.due {
            when.push(format!("due {}", format_time(d, tz)));
        }
        let description = truncate(
            &format!(
                "{} ({}{}{})",
                task.title,
                task.kind.as_str(),
                if when.is_empty() { "" } else { ", " },
                when.join(", ")
            ),
            160,
        );
        let mut doc = String::new();
        let _ = writeln!(doc, "---");
        let _ = writeln!(doc, "type: Task");
        let _ = writeln!(doc, "title: {}", quote(&task.title));
        let _ = writeln!(doc, "description: {}", quote(&description));
        let _ = writeln!(doc, "tags: [task]");
        let _ = writeln!(doc, "lang: {}", language(&task.title));
        let _ = writeln!(doc, "status: stable");
        let _ = writeln!(
            doc,
            "updated: {}",
            Timestamp::now().to_zoned(tz.clone()).strftime("%Y-%m-%d")
        );
        let stem = task.id.rsplit('/').next().unwrap_or(&task.id);
        let _ = writeln!(doc, "id: {stem}");
        let _ = writeln!(doc, "kind: {}", task.kind.as_str());
        let _ = writeln!(doc, "state: {}", task.status.as_str());
        let _ = writeln!(doc, "scope: {}", scope_name(task.scope));
        let time = |t: Option<Timestamp>| t.map(|t| quote(&format_time(t, tz)));
        if let Some(due) = time(task.due) {
            let _ = writeln!(doc, "due: {due}");
        }
        if let Some(at) = time(task.remind_at) {
            let _ = writeln!(doc, "remind_at: {at}");
        }
        if let Some(repeat) = &task.repeat {
            let _ = writeln!(doc, "repeat: {}", quote(&repeat.to_string()));
        }
        let _ = writeln!(doc, "timezone: {}", quote(tz.iana_name().unwrap_or("UTC")));
        let _ = writeln!(
            doc,
            "deliver_to: {}",
            task.deliver_to
                .as_ref()
                .map_or_else(|| "origin".to_string(), |c| quote(&c.to_string()))
        );
        if let Some(o) = &task.origin {
            let _ = writeln!(doc, "origin_channel: {}", quote(&o.channel.to_string()));
            if let Some(space) = &o.space {
                let _ = writeln!(doc, "origin_space: {}", quote(space));
            }
            let _ = writeln!(doc, "origin_room: {}", quote(&o.room));
            if let Some(thread) = &o.thread {
                let _ = writeln!(doc, "origin_thread: {}", quote(thread));
            }
        }
        let _ = writeln!(doc, "author: {}", quote(&task.author));
        let _ = writeln!(doc, "created: {}", quote(&format_time(task.created, tz)));
        let _ = writeln!(doc, "generated: {{by: okbot, at: \"{day}\"}}");
        let _ = writeln!(doc, "---\n\n# {}\n", task.title);
        if !task.notes.trim().is_empty() {
            let _ = writeln!(doc, "{}", task.notes.trim());
        }
        doc
    }

    /// Reads one task file; files that are not tasks are skipped.
    fn read(folder: &str, path: &Path, scope: MemoryScope) -> Option<Task> {
        let text = std::fs::read_to_string(path).ok()?;
        let (fm, body) = okbase::core::Frontmatter::split(&text);
        if fm.get_str("type") != Some("Task") {
            return None;
        }
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        let time = |key: &str| fm.get_str(key).and_then(|v| v.parse::<Timestamp>().ok());
        let title = fm.get_str("title").unwrap_or(&stem).to_string();
        // The body starts with the `# title` heading the writer adds.
        let notes = body.trim_start();
        let notes = notes
            .strip_prefix(&format!("# {title}"))
            .unwrap_or(notes)
            .trim()
            .to_string();
        let origin = fm
            .get_str("origin_channel")
            .and_then(|c| c.parse().ok())
            .zip(fm.get_str("origin_room"))
            .map(|(channel, room)| ConversationKey {
                channel,
                space: fm.get_str("origin_space").map(str::to_string),
                room: room.to_string(),
                thread: fm.get_str("origin_thread").map(str::to_string),
            });
        Some(Task {
            id: format!("{folder}/{stem}"),
            kind: fm.get_str("kind")?.parse().ok()?,
            status: fm.get_str("state")?.parse().ok()?,
            title,
            notes,
            scope,
            due: time("due"),
            remind_at: time("remind_at"),
            repeat: fm.get_str("repeat").and_then(|r| r.parse::<Repeat>().ok()),
            deliver_to: fm
                .get_str("deliver_to")
                .filter(|d| *d != "origin")
                .and_then(|d| d.parse().ok()),
            origin,
            author: fm.get_str("author").unwrap_or_default().to_string(),
            created: time("created").unwrap_or(Timestamp::UNIX_EPOCH),
        })
    }

    /// Tasks in `folder` (not recursive).
    fn read_folder(&self, folder: &str, scope: MemoryScope) -> Vec<Task> {
        let Ok(entries) = std::fs::read_dir(self.root.join(folder)) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
            .filter(|p| p.file_name().is_some_and(|n| n != "index.md"))
            .filter_map(|p| Self::read(folder, &p, scope))
            .collect()
    }

    fn write(&self, task: &Task) -> Result<(), TaskError> {
        let (folder, _) = task
            .id
            .rsplit_once('/')
            .ok_or_else(|| TaskError::NotFound(task.id.clone()))?;
        std::fs::create_dir_all(self.root.join(folder)).map_err(backend)?;
        write_atomic(
            &self.root.join(format!("{}.md", task.id)),
            &self.render(task),
        )
        .map_err(backend)?;
        reindex(&self.root, folder).map_err(backend)?;
        link_area(&self.root).map_err(backend)
    }

    /// Creates a task for `who`.
    pub(crate) fn add(
        &self,
        who: &Requester,
        new: NewTask,
        now: Timestamp,
    ) -> Result<Task, TaskError> {
        check_text(&new.title, &new.notes)?;
        let folder = folder(new.scope, who)?;
        let active = self
            .read_folder(&folder, new.scope)
            .iter()
            .filter(|t| t.status.is_active())
            .count();
        if active >= MAX_ACTIVE {
            return Err(TaskError::Invalid(format!(
                "there are already {MAX_ACTIVE} open tasks here; finish or delete some first"
            )));
        }
        // A repeating task without a first time starts at the rule's next occurrence.
        let remind_at = new.remind_at.or_else(|| {
            new.repeat
                .as_ref()
                .and_then(|r| r.next_after(now, &self.tz))
        });
        let mut id = format!("{folder}/{}", new_id());
        while self.root.join(format!("{id}.md")).exists() {
            id = format!("{folder}/{}", new_id());
        }
        let task = Task {
            id,
            kind: new.kind,
            status: if new.paused {
                TaskStatus::Paused
            } else {
                TaskStatus::Open
            },
            title: new.title.trim().to_string(),
            notes: new.notes.trim().to_string(),
            scope: new.scope,
            due: new.due,
            remind_at,
            repeat: new.repeat,
            deliver_to: new.deliver_to,
            origin: Some(who.conversation.clone()),
            author: author_of(who),
            created: now,
        };
        check_schedule(&task, now, true)?;
        self.write(&task)?;
        Ok(task)
    }

    /// Every task `who` sees, in display order.
    pub(crate) fn list(&self, who: &Requester) -> Vec<Task> {
        let mut out: Vec<Task> = visible_folders(who)
            .into_iter()
            .flat_map(|(folder, scope)| self.read_folder(&folder, scope))
            .collect();
        sort_tasks(&mut out);
        out
    }

    /// A visible task by full id (`tasks/…/t-7k2m9q`) or short id (`t-7k2m9q`).
    fn find(&self, who: &Requester, id: &str) -> Result<Task, TaskError> {
        let id = id.trim().trim_end_matches(".md");
        let short = id.rsplit('/').next().unwrap_or(id);
        self.list(who)
            .into_iter()
            .find(|t| t.id == id || t.id.rsplit('/').next() == Some(short) && !id.contains('/'))
            .ok_or_else(|| TaskError::NotFound(id.to_string()))
    }

    /// Changes a visible task.
    pub(crate) fn update(
        &self,
        who: &Requester,
        id: &str,
        change: TaskChange,
        now: Timestamp,
    ) -> Result<Task, TaskError> {
        let task = self.find(who, id)?;
        self.apply(task, change, now)
    }

    /// Applies `change` to `task` and writes it (no visibility check: callers check).
    pub(crate) fn apply(
        &self,
        mut task: Task,
        change: TaskChange,
        now: Timestamp,
    ) -> Result<Task, TaskError> {
        let fresh_time = change.remind_at.is_some() || change.status == Some(TaskStatus::Open);
        if let Some(title) = change.title {
            task.title = title.trim().to_string();
        }
        if let Some(notes) = change.notes {
            task.notes = notes.trim().to_string();
        }
        if let Some(status) = change.status {
            task.status = status;
        }
        if let Some(due) = change.due {
            task.due = due;
        }
        if let Some(repeat) = change.repeat {
            task.repeat = repeat;
        }
        if let Some(at) = change.remind_at {
            task.remind_at = at;
        }
        // A repeating task that lost its next time (reopened, rule changed) gets the next one.
        if task.status.is_active() && task.remind_at.is_none() {
            task.remind_at = task
                .repeat
                .as_ref()
                .and_then(|r| r.next_after(now, &self.tz));
        }
        check_text(&task.title, &task.notes)?;
        check_schedule(&task, now, fresh_time)?;
        self.write(&task)?;
        Ok(task)
    }

    /// Deletes a visible task.
    pub(crate) fn delete(&self, who: &Requester, id: &str) -> Result<Task, TaskError> {
        let task = self.find(who, id)?;
        self.remove(&task.id)?;
        Ok(task)
    }

    /// Deletes a task by id (no visibility check: operators only).
    pub(crate) fn remove(&self, id: &str) -> Result<(), TaskError> {
        if !id.starts_with("tasks/") || id.split('/').any(|s| s.is_empty() || s == "..") {
            return Err(TaskError::NotFound(id.to_string()));
        }
        std::fs::remove_file(self.root.join(format!("{id}.md")))
            .map_err(|_| TaskError::NotFound(id.to_string()))?;
        let folder = id.rsplit_once('/').map_or("tasks", |(f, _)| f);
        reindex(&self.root, folder).map_err(backend)
    }

    /// Every task in the bundle (scheduler, operators), in display order.
    pub(crate) fn all(&self) -> Vec<Task> {
        fn walk(tasks: &Tasks, folder: &str, scope: MemoryScope, out: &mut Vec<Task>) {
            out.extend(tasks.read_folder(folder, scope));
            if let Ok(entries) = std::fs::read_dir(tasks.root.join(folder)) {
                for e in entries.filter_map(Result::ok) {
                    if e.path().is_dir() {
                        let sub = format!("{folder}/{}", e.file_name().to_string_lossy());
                        walk(tasks, &sub, scope, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(self, "tasks/rooms", MemoryScope::Room, &mut out);
        walk(self, "tasks/users", MemoryScope::User, &mut out);
        walk(self, "tasks/people", MemoryScope::User, &mut out);
        sort_tasks(&mut out);
        out
    }
}

/// The `task` tag, for bundles created before tasks existed.
const TASK_TERM: &str = "  task:\n    synonyms: [việc cần làm, nhắc nhở, to-do, reminder]\n    \
facet: topic\n    description: To-dos, reminders and routines.\n";

/// Brings a bundle created before tasks existed up to date: the `task` tag in the vocabulary and
/// `tasks/` in the root index.
fn link_area(root: &Path) -> std::io::Result<()> {
    let vocabulary = root.join("_meta/vocabulary.md");
    if let Ok(text) = std::fs::read_to_string(&vocabulary)
        && !text.contains("\n  task:")
        && let Some(at) = text.find("terms:\n")
    {
        let at = at + "terms:\n".len();
        write_atomic(
            &vocabulary,
            &format!("{}{TASK_TERM}{}", &text[..at], &text[at..]),
        )?;
    }
    let index = root.join("index.md");
    let Ok(text) = std::fs::read_to_string(&index) else {
        return Ok(());
    };
    if text.contains("/tasks/index.md") {
        return Ok(());
    }
    let mut text = text.trim_end().to_string();
    if !text.contains("## Subdirectories") {
        text.push_str("\n\n## Subdirectories");
    }
    text.push_str("\n* [tasks](/tasks/index.md) - To-dos, reminders and routines.\n");
    write_atomic(&index, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::UserRef;

    fn who(user: &str, room: &str) -> Requester {
        Requester {
            conversation: ConversationKey::room("zalo:shop".parse().unwrap(), room),
            user: UserRef {
                id: user.into(),
                display_name: user.into(),
                is_bot: false,
            },
            internal: false,
            person: None,
        }
    }

    fn tz() -> TimeZone {
        TimeZone::get("Asia/Ho_Chi_Minh").unwrap()
    }

    fn now() -> Timestamp {
        "2026-10-13T07:00:00Z".parse().unwrap()
    }

    #[test]
    fn round_trip_and_visibility() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let tasks = Tasks::new(dir.path(), tz());
        let ana = who("ana", "dm-ana");
        let mut new = NewTask::todo("Mua sữa");
        new.kind = TaskKind::Reminder;
        new.notes = "Hai hộp, loại không đường.".into();
        new.remind_at = Some(okbot_core::parse_time("2026-10-14 08:00", &tz()).unwrap());
        new.due = Some(okbot_core::parse_time("2026-10-14 18:00", &tz()).unwrap());
        let task = tasks.add(&ana, new, now()).unwrap();
        assert!(
            task.id.starts_with("tasks/users/zalo-shop/ana/t-"),
            "{}",
            task.id
        );

        let text = std::fs::read_to_string(dir.path().join(format!("{}.md", task.id))).unwrap();
        assert!(
            text.contains("remind_at: \"2026-10-14T08:00:00+07:00\""),
            "{text}"
        );
        assert!(text.contains("state: open"), "{text}");
        let listed = tasks.list(&ana);
        assert_eq!(listed, vec![task.clone()]);
        assert_eq!(tasks.list(&who("bob", "dm-bob")), Vec::<Task>::new());

        let short = task.id.rsplit('/').next().unwrap();
        let done = tasks
            .update(
                &ana,
                short,
                TaskChange {
                    status: Some(TaskStatus::Done),
                    ..TaskChange::default()
                },
                now(),
            )
            .unwrap();
        assert_eq!(done.status, TaskStatus::Done);
        assert!(matches!(
            tasks.delete(&who("bob", "dm-bob"), short),
            Err(TaskError::NotFound(_))
        ));
        tasks.delete(&ana, short).unwrap();
        assert_eq!(tasks.all(), Vec::<Task>::new());
        let index = std::fs::read_to_string(dir.path().join("index.md")).unwrap();
        assert_eq!(index.matches("/tasks/index.md").count(), 1);
    }

    #[test]
    fn older_bundles_get_the_task_tag_and_index_link() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let vocabulary = dir.path().join("_meta/vocabulary.md");
        let text = std::fs::read_to_string(&vocabulary).unwrap();
        std::fs::write(&vocabulary, text.replace(TASK_TERM, "")).unwrap();
        let index = dir.path().join("index.md");
        let text = std::fs::read_to_string(&index).unwrap();
        let old: Vec<&str> = text.lines().filter(|l| !l.contains("/tasks/")).collect();
        let old = old.join("\n");
        std::fs::write(&index, old).unwrap();

        let tasks = Tasks::new(dir.path(), tz());
        tasks
            .add(&who("ana", "dm"), NewTask::todo("x"), now())
            .unwrap();
        tasks
            .add(&who("ana", "dm"), NewTask::todo("y"), now())
            .unwrap();
        let text = std::fs::read_to_string(&vocabulary).unwrap();
        assert_eq!(text.matches("\n  task:").count(), 1, "{text}");
        let text = std::fs::read_to_string(&index).unwrap();
        assert_eq!(text.matches("/tasks/index.md").count(), 1, "{text}");
    }

    #[test]
    fn rules_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = Tasks::new(dir.path(), tz());
        let ana = who("ana", "dm");
        let mut past = NewTask::todo("x");
        past.remind_at = Some("2026-10-13T06:00:00Z".parse().unwrap());
        assert!(matches!(
            tasks.add(&ana, past, now()),
            Err(TaskError::Invalid(_))
        ));
        let mut reminder = NewTask::todo("x");
        reminder.kind = TaskKind::Reminder;
        assert!(tasks.add(&ana, reminder, now()).is_err());
        let mut routine = NewTask::todo("Morning brief");
        routine.kind = TaskKind::Routine;
        routine.repeat = Some("daily 07:00".parse().unwrap());
        assert!(
            tasks.add(&ana, routine.clone(), now()).is_err(),
            "no prompt"
        );
        routine.notes = "Summarise today's tasks.".into();
        let made = tasks.add(&ana, routine, now()).unwrap();
        // 07:00Z is 14:00 local, so the first run is tomorrow 07:00 local.
        assert_eq!(
            made.remind_at,
            Some("2026-10-14T00:00:00Z".parse().unwrap())
        );
        let mut global = NewTask::todo("x");
        global.scope = MemoryScope::Global;
        assert!(tasks.add(&ana, global, now()).is_err());
        assert!(tasks.add(&ana, NewTask::todo("  "), now()).is_err());
    }

    #[test]
    fn room_tasks_are_shared_in_the_room() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = Tasks::new(dir.path(), tz());
        let mut new = NewTask::todo("Order cups");
        new.scope = MemoryScope::Room;
        tasks.add(&who("ana", "team"), new, now()).unwrap();
        assert_eq!(tasks.list(&who("bob", "team")).len(), 1);
        assert_eq!(tasks.list(&who("bob", "other")), Vec::<Task>::new());
        assert_eq!(tasks.all()[0].scope, MemoryScope::Room);
    }
}
