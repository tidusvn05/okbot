//! Artifacts (ADR 0021): one OKF document per artifact under `artifacts/`, in the same owner
//! folders as tasks, beside a folder with one file per version:
//!
//! ```text
//! artifacts/people/p-1/a-7k2m9q4x2c.md      title, kind, summary, versions (indexed)
//! artifacts/people/p-1/a-7k2m9q4x2c/v1.html (left out of the index by `.okbaseignore`)
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use okbot_core::{
    Artifact, ArtifactError, ArtifactLimits, ArtifactVersion, ConversationKey, MemoryScope,
    NewArtifact, Requester, format_time,
};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::memory::{author_of, language, quote, reindex, truncate, write_atomic};
use crate::paths;

/// The area's folder.
const AREA: &str = "artifacts";
/// Longest title, in characters.
const MAX_TITLE_CHARS: usize = 80;
/// Longest summary, in characters.
const MAX_SUMMARY_CHARS: usize = 500;
/// Longest version note, in characters.
const MAX_NOTE_CHARS: usize = 200;
/// Artifacts in one owner folder.
const MAX_PER_FOLDER: usize = 500;
/// The line that keeps version files out of the knowledge index.
pub(crate) const IGNORE_LINE: &str = "artifacts/**/a-*/";

fn backend(e: impl std::error::Error + Send + Sync + 'static) -> ArtifactError {
    ArtifactError::Backend(Box::new(e))
}

/// A fresh id: `a-` and ten characters without look-alikes.
fn new_id() -> String {
    const ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    let bytes = rand::random::<[u8; 10]>();
    let tail: String = bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect();
    format!("a-{tail}")
}

fn scope_name(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Room => "room",
        _ => "user",
    }
}

/// The folder for new `scope` artifacts of `who`.
fn folder(scope: MemoryScope, who: &Requester) -> Result<String, ArtifactError> {
    match scope {
        MemoryScope::User if !who.personal => Err(ArtifactError::Invalid(paths::NOT_HERE.into())),
        MemoryScope::User => Ok(paths::in_area(AREA, &paths::owner_dir(who))),
        MemoryScope::Room => Ok(paths::in_area(
            AREA,
            &paths::room_dir(&who.conversation.channel, &who.conversation.room),
        )),
        MemoryScope::Global => Err(ArtifactError::Invalid(
            "artifacts are personal (`user`) or for this chat (`room`)".into(),
        )),
    }
}

/// Every folder whose artifacts `who` sees, with the scope they belong to.
fn visible_folders(who: &Requester) -> Vec<(String, MemoryScope)> {
    let mut out: Vec<(String, MemoryScope)> = paths::personal_dirs(who)
        .iter()
        .map(|d| (paths::in_area(AREA, d), MemoryScope::User))
        .collect();
    out.push((
        paths::in_area(
            AREA,
            &paths::room_dir(&who.conversation.channel, &who.conversation.room),
        ),
        MemoryScope::Room,
    ));
    out
}

fn check(title: &str, summary: &str, note: &str) -> Result<(), ArtifactError> {
    let invalid = |m: String| Err(ArtifactError::Invalid(m));
    if title.trim().is_empty() {
        return invalid("the artifact needs a title".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return invalid(format!(
            "the title is longer than {MAX_TITLE_CHARS} characters"
        ));
    }
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        return invalid(format!(
            "the summary is longer than {MAX_SUMMARY_CHARS} characters"
        ));
    }
    if note.chars().count() > MAX_NOTE_CHARS {
        return invalid(format!(
            "the note is longer than {MAX_NOTE_CHARS} characters"
        ));
    }
    Ok(())
}

fn version_json(v: &ArtifactVersion) -> Value {
    json!({"n": v.n, "at": v.at.to_string(), "by": v.by, "bytes": v.bytes, "sha256": v.sha256,
           "note": v.note, "pruned": v.pruned})
}

fn version_of(v: &Value) -> Option<ArtifactVersion> {
    Some(ArtifactVersion {
        n: u32::try_from(v.get("n")?.as_u64()?).ok()?,
        at: v.get("at")?.as_str()?.parse().ok()?,
        by: v.get("by")?.as_str()?.to_string(),
        bytes: v.get("bytes")?.as_u64()?,
        sha256: v.get("sha256")?.as_str()?.to_string(),
        note: v
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pruned: v.get("pruned").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Artifacts in one bundle. Writes are serialized by the caller.
#[derive(Debug, Clone)]
pub(crate) struct Artifacts {
    root: PathBuf,
    tz: TimeZone,
    limits: ArtifactLimits,
}

impl Artifacts {
    pub(crate) fn new(root: &Path, tz: TimeZone, limits: ArtifactLimits) -> Self {
        Self {
            root: root.to_path_buf(),
            tz,
            limits,
        }
    }

    fn render(&self, a: &Artifact) -> String {
        let tz = &self.tz;
        let mut doc = String::new();
        let _ = writeln!(doc, "---");
        let _ = writeln!(doc, "type: Artifact");
        let _ = writeln!(doc, "title: {}", quote(&a.title));
        let _ = writeln!(
            doc,
            "description: {}",
            quote(&truncate(
                &format!("{} ({}). {}", a.title, a.kind, a.summary),
                160
            ))
        );
        let _ = writeln!(doc, "tags: [artifact]");
        let _ = writeln!(
            doc,
            "lang: {}",
            language(&format!("{} {}", a.title, a.summary))
        );
        let _ = writeln!(doc, "status: stable");
        let _ = writeln!(
            doc,
            "updated: {}",
            a.updated().to_zoned(tz.clone()).strftime("%Y-%m-%d")
        );
        let _ = writeln!(doc, "id: {}", a.short_id());
        let _ = writeln!(doc, "kind: {}", a.kind);
        let _ = writeln!(doc, "scope: {}", scope_name(a.scope));
        if let Some(o) = &a.origin {
            let _ = writeln!(doc, "origin_channel: {}", quote(&o.channel.to_string()));
            if let Some(space) = &o.space {
                let _ = writeln!(doc, "origin_space: {}", quote(space));
            }
            let _ = writeln!(doc, "origin_room: {}", quote(&o.room));
        }
        let _ = writeln!(doc, "author: {}", quote(&a.author));
        let _ = writeln!(doc, "created: {}", quote(&format_time(a.created, tz)));
        // JSON is valid YAML flow syntax, and okbase reads frontmatter as JSON values.
        let versions: Vec<Value> = a.versions.iter().map(version_json).collect();
        let _ = writeln!(doc, "versions: {}", Value::Array(versions));
        let day = a.created.to_zoned(tz.clone()).strftime("%Y-%m-%d");
        let _ = writeln!(doc, "generated: {{by: okbot, at: \"{day}\"}}");
        let _ = writeln!(doc, "---\n\n# {}\n", a.title);
        if !a.summary.trim().is_empty() {
            let _ = writeln!(doc, "{}", a.summary.trim());
        }
        doc
    }

    fn read(folder: &str, path: &Path, scope: MemoryScope) -> Option<Artifact> {
        let text = std::fs::read_to_string(path).ok()?;
        let (fm, body) = okbase::core::Frontmatter::split(&text);
        if fm.get_str("type") != Some("Artifact") {
            return None;
        }
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        let title = fm.get_str("title").unwrap_or(&stem).to_string();
        let summary = body.trim_start();
        let summary = summary
            .strip_prefix(&format!("# {title}"))
            .unwrap_or(summary)
            .trim()
            .to_string();
        let versions: Vec<ArtifactVersion> = fm
            .get("versions")?
            .as_array()?
            .iter()
            .filter_map(version_of)
            .collect();
        if versions.is_empty() {
            return None;
        }
        let origin = fm
            .get_str("origin_channel")
            .and_then(|c| c.parse().ok())
            .zip(fm.get_str("origin_room"))
            .map(|(channel, room)| ConversationKey {
                channel,
                space: fm.get_str("origin_space").map(str::to_string),
                room: room.to_string(),
                thread: None,
            });
        Some(Artifact {
            id: format!("{folder}/{stem}"),
            title,
            kind: fm.get_str("kind")?.parse().ok()?,
            summary,
            scope,
            author: fm.get_str("author").unwrap_or_default().to_string(),
            origin,
            created: fm
                .get_str("created")
                .and_then(|v| v.parse().ok())
                .unwrap_or(Timestamp::UNIX_EPOCH),
            versions,
        })
    }

    fn read_folder(&self, folder: &str, scope: MemoryScope) -> Vec<Artifact> {
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

    /// Every artifact in the bundle (quotas, operators).
    pub(crate) fn all(&self) -> Vec<Artifact> {
        fn walk(a: &Artifacts, folder: &str, out: &mut Vec<Artifact>) {
            let scope = if folder.contains("/rooms/") {
                MemoryScope::Room
            } else {
                MemoryScope::User
            };
            out.extend(a.read_folder(folder, scope));
            let Ok(entries) = std::fs::read_dir(a.root.join(folder)) else {
                return;
            };
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir() && !name.starts_with("a-") && !name.starts_with('.') {
                    walk(a, &format!("{folder}/{name}"), out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, AREA, &mut out);
        out
    }

    /// Every artifact `who` sees, newest change first.
    pub(crate) fn list(&self, who: &Requester) -> Vec<Artifact> {
        let mut out: Vec<Artifact> = visible_folders(who)
            .into_iter()
            .flat_map(|(folder, scope)| self.read_folder(&folder, scope))
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.updated()));
        out
    }

    fn find(&self, who: &Requester, id: &str) -> Result<Artifact, ArtifactError> {
        let id = id.trim().trim_end_matches(".md");
        let short = id.rsplit('/').next().unwrap_or(id);
        self.list(who)
            .into_iter()
            .find(|a| a.id == id || (!id.contains('/') && a.short_id() == short))
            .ok_or_else(|| ArtifactError::NotFound(id.to_string()))
    }

    fn version_path(&self, a: &Artifact, n: u32) -> PathBuf {
        self.root
            .join(&a.id)
            .join(format!("v{n}.{}", a.kind.extension()))
    }

    fn write_meta(&self, a: &Artifact) -> Result<(), ArtifactError> {
        let (folder, _) =
            a.id.rsplit_once('/')
                .ok_or_else(|| ArtifactError::NotFound(a.id.clone()))?;
        write_atomic(&self.root.join(format!("{}.md", a.id)), &self.render(a)).map_err(backend)?;
        reindex(&self.root, folder).map_err(backend)?;
        crate::areas::ensure(&self.root, &crate::areas::ARTIFACTS).map_err(backend)
    }

    /// Keeps version files out of the knowledge index: raw pages never feed later answers.
    fn ensure_ignored(&self) -> Result<(), ArtifactError> {
        let path = self.root.join(okbase::core::IGNORE_FILE);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l.trim() == IGNORE_LINE) {
            return Ok(());
        }
        let mut text = text;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        let _ = writeln!(
            text,
            "# Artifact versions (ADR 0021): only their summaries are indexed."
        );
        let _ = writeln!(text, "{IGNORE_LINE}");
        write_atomic(&path, &text).map_err(backend)
    }

    /// Saves a new artifact or a new version of a visible one.
    pub(crate) fn save(
        &self,
        who: &Requester,
        new: &NewArtifact,
        now: Timestamp,
    ) -> Result<Artifact, ArtifactError> {
        let bytes = new.content.len() as u64;
        if new.content.trim().is_empty() {
            return Err(ArtifactError::Invalid("the content is empty".into()));
        }
        if bytes > self.limits.max_bytes {
            return Err(ArtifactError::TooLarge(format!(
                "the content is {} KB; at most {} KB per version",
                bytes / 1024,
                self.limits.max_bytes / 1024
            )));
        }
        let mut artifact = match &new.id {
            Some(id) => {
                let mut a = self.find(who, id)?;
                if !new.title.trim().is_empty() {
                    a.title = new.title.trim().to_string();
                }
                if !new.summary.trim().is_empty() {
                    a.summary = new.summary.trim().to_string();
                }
                a
            }
            None => {
                let folder = folder(new.scope, who)?;
                if self.read_folder(&folder, new.scope).len() >= MAX_PER_FOLDER {
                    return Err(ArtifactError::TooLarge(format!(
                        "there are already {MAX_PER_FOLDER} artifacts here; delete some first"
                    )));
                }
                let mut id = format!("{folder}/{}", new_id());
                while self.root.join(format!("{id}.md")).exists() {
                    id = format!("{folder}/{}", new_id());
                }
                Artifact {
                    id,
                    title: new.title.trim().to_string(),
                    kind: new.kind,
                    summary: new.summary.trim().to_string(),
                    scope: new.scope,
                    author: author_of(who),
                    origin: Some(who.conversation.clone()),
                    created: now,
                    versions: Vec::new(),
                }
            }
        };
        check(&artifact.title, &artifact.summary, &new.note)?;
        let folder = artifact.id.rsplit_once('/').map_or(AREA, |(f, _)| f);
        let owner_total: u64 = self
            .read_folder(folder, artifact.scope)
            .iter()
            .map(Artifact::stored_bytes)
            .sum();
        if owner_total + bytes > self.limits.quota_owner {
            return Err(ArtifactError::TooLarge(format!(
                "artifacts here already take {} MB of {} MB; delete some first",
                owner_total >> 20,
                self.limits.quota_owner >> 20
            )));
        }
        let bot_total: u64 = self.all().iter().map(Artifact::stored_bytes).sum();
        if bot_total + bytes > self.limits.quota_bot {
            return Err(ArtifactError::TooLarge(
                "this bot's artifacts are full; ask its operator to clean up".into(),
            ));
        }
        let n = artifact.versions.last().map_or(1, |v| v.n + 1);
        artifact.versions.push(ArtifactVersion {
            n,
            at: now,
            by: author_of(who),
            bytes,
            sha256: hex::encode(sha2::Sha256::digest(new.content.as_bytes())),
            note: new.note.trim().to_string(),
            pruned: false,
        });
        self.ensure_ignored()?;
        let dir = self.root.join(&artifact.id);
        std::fs::create_dir_all(&dir).map_err(backend)?;
        write_atomic(&self.version_path(&artifact, n), &new.content).map_err(backend)?;
        // Keep the newest `keep_versions` files; older entries stay, marked pruned.
        let keep = usize::try_from(self.limits.keep_versions.max(1)).unwrap_or(usize::MAX);
        let live = artifact.versions.iter().filter(|v| !v.pruned).count();
        let mut excess = live.saturating_sub(keep);
        for v in &mut artifact.versions {
            if excess == 0 {
                break;
            }
            if !v.pruned {
                let _ = std::fs::remove_file(dir.join(format!(
                    "v{}.{}",
                    v.n,
                    artifact.kind.extension()
                )));
                v.pruned = true;
                excess -= 1;
            }
        }
        self.write_meta(&artifact)?;
        Ok(artifact)
    }

    /// A visible artifact with the content of `version` (default: the newest).
    pub(crate) fn open(
        &self,
        who: &Requester,
        id: &str,
        version: Option<u32>,
    ) -> Result<(Artifact, u32, String), ArtifactError> {
        let a = self.find(who, id)?;
        self.content(a, version)
    }

    /// Any artifact by short or full id (operators).
    pub(crate) fn find_any(&self, id: &str) -> Result<Artifact, ArtifactError> {
        let id = id.trim().trim_end_matches(".md");
        let mut found: Vec<Artifact> = self
            .all()
            .into_iter()
            .filter(|a| a.id == id || (!id.contains('/') && a.short_id() == id))
            .collect();
        match found.len() {
            0 => Err(ArtifactError::NotFound(id.to_string())),
            1 => Ok(found.remove(0)),
            _ => Err(ArtifactError::Invalid(format!(
                "`{id}` names more than one artifact; give the full id"
            ))),
        }
    }

    /// Any artifact with the content of `version` (operators).
    pub(crate) fn open_any(
        &self,
        id: &str,
        version: Option<u32>,
    ) -> Result<(Artifact, u32, String), ArtifactError> {
        self.content(self.find_any(id)?, version)
    }

    /// Deletes any artifact (operators).
    pub(crate) fn delete_any(&self, id: &str) -> Result<Artifact, ArtifactError> {
        self.remove(self.find_any(id)?)
    }

    /// The limits stores check.
    pub(crate) fn limits(&self) -> ArtifactLimits {
        self.limits
    }

    fn content(
        &self,
        a: Artifact,
        version: Option<u32>,
    ) -> Result<(Artifact, u32, String), ArtifactError> {
        let n = version.unwrap_or(a.latest().n);
        let v = a
            .versions
            .iter()
            .find(|v| v.n == n)
            .ok_or_else(|| ArtifactError::NotFound(format!("{} v{n}", a.short_id())))?;
        if v.pruned {
            return Err(ArtifactError::NotFound(format!(
                "{} v{n} (removed to keep the newest versions)",
                a.short_id()
            )));
        }
        let content = std::fs::read_to_string(self.version_path(&a, n))
            .map_err(|_| ArtifactError::NotFound(format!("{} v{n}", a.short_id())))?;
        Ok((a, n, content))
    }

    /// Renames a visible artifact.
    pub(crate) fn rename(
        &self,
        who: &Requester,
        id: &str,
        title: &str,
    ) -> Result<Artifact, ArtifactError> {
        let mut a = self.find(who, id)?;
        a.title = title.trim().to_string();
        check(&a.title, &a.summary, "")?;
        self.write_meta(&a)?;
        Ok(a)
    }

    /// Deletes a visible artifact with every version.
    pub(crate) fn delete(&self, who: &Requester, id: &str) -> Result<Artifact, ArtifactError> {
        self.remove(self.find(who, id)?)
    }

    fn remove(&self, a: Artifact) -> Result<Artifact, ArtifactError> {
        if !a.id.starts_with("artifacts/") || a.id.split('/').any(|s| s.is_empty() || s == "..") {
            return Err(ArtifactError::NotFound(a.id));
        }
        let dir = self.root.join(&a.id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(backend)?;
        }
        std::fs::remove_file(self.root.join(format!("{}.md", a.id))).map_err(backend)?;
        let folder = a.id.rsplit_once('/').map_or(AREA, |(f, _)| f);
        reindex(&self.root, folder).map_err(backend)?;
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{ArtifactKind, ConversationKey, UserRef};

    fn who(user: &str, room: &str, personal: bool) -> Requester {
        Requester {
            conversation: ConversationKey::room("zalo:me".parse().unwrap(), room),
            user: UserRef {
                id: user.into(),
                display_name: user.into(),
                is_bot: false,
            },
            internal: false,
            person: None,
            personal,
        }
    }

    fn new(title: &str, content: &str, scope: MemoryScope) -> NewArtifact {
        NewArtifact {
            id: None,
            title: title.into(),
            kind: ArtifactKind::Html,
            summary: format!("About {title}"),
            content: content.into(),
            scope,
            note: String::new(),
        }
    }

    fn store(dir: &Path, limits: ArtifactLimits) -> Artifacts {
        crate::write_default_bundle(dir).unwrap();
        Artifacts::new(dir, TimeZone::UTC, limits)
    }

    #[test]
    fn saves_versions_and_keeps_them_apart_from_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), ArtifactLimits::default());
        let ana = who("ana", "dm-ana", true);
        let a = s
            .save(
                &ana,
                &new("Report", "<h1>v1</h1>", MemoryScope::User),
                Timestamp::now(),
            )
            .unwrap();
        assert!(
            a.id.starts_with("artifacts/users/zalo-me/ana/a-"),
            "{}",
            a.id
        );
        let mut next = new("", "<h1>v2</h1>", MemoryScope::User);
        next.id = Some(a.short_id().into());
        next.summary = String::new();
        next.note = "wider table".into();
        let b = s.save(&ana, &next, Timestamp::now()).unwrap();
        assert_eq!(b.id, a.id);
        assert_eq!(b.title, "Report", "an empty title keeps the old one");
        assert_eq!(b.versions.len(), 2);
        let (_, n, content) = s.open(&ana, a.short_id(), None).unwrap();
        assert_eq!((n, content.as_str()), (2, "<h1>v2</h1>"));
        let (_, _, old) = s.open(&ana, &a.id, Some(1)).unwrap();
        assert_eq!(old, "<h1>v1</h1>");
        assert!(dir.path().join(&a.id).join("v2.html").is_file());
        let ignore = std::fs::read_to_string(dir.path().join(".okbaseignore")).unwrap();
        assert!(ignore.lines().any(|l| l == IGNORE_LINE), "{ignore}");
        let meta = std::fs::read_to_string(dir.path().join(format!("{}.md", a.id))).unwrap();
        assert!(
            meta.contains("type: Artifact") && meta.contains("About Report"),
            "{meta}"
        );

        let renamed = s.rename(&ana, a.short_id(), "Final report").unwrap();
        assert_eq!(s.list(&ana)[0].title, renamed.title);
        s.delete(&ana, a.short_id()).unwrap();
        assert_eq!(s.list(&ana).len(), 0);
        assert!(!dir.path().join(&a.id).exists());
    }

    #[test]
    fn operators_reach_every_artifact_by_short_or_full_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), ArtifactLimits::default());
        let ana = who("ana", "dm-ana", true);
        let bo = who("bo", "group", false);
        let a = s
            .save(
                &ana,
                &new("Mine", "<p>a</p>", MemoryScope::User),
                Timestamp::now(),
            )
            .unwrap();
        let b = s
            .save(
                &bo,
                &new("Ours", "<p>b</p>", MemoryScope::Room),
                Timestamp::now(),
            )
            .unwrap();
        assert_eq!(s.all().len(), 2);
        let (found, n, content) = s.open_any(a.short_id(), None).unwrap();
        assert_eq!(
            (found.id.as_str(), n, content.as_str()),
            (a.id.as_str(), 1, "<p>a</p>")
        );
        assert_eq!(s.find_any(&b.id).unwrap().scope, MemoryScope::Room);
        assert!(matches!(
            s.find_any("a-nothere"),
            Err(ArtifactError::NotFound(_))
        ));
        s.delete_any(b.short_id()).unwrap();
        assert_eq!(s.all().len(), 1);
        assert!(!dir.path().join(&b.id).exists());
    }

    #[test]
    fn personal_artifacts_need_a_direct_chat_and_rooms_see_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), ArtifactLimits::default());
        let in_group = who("ana", "group", false);
        let error = s
            .save(
                &in_group,
                &new("Mine", "x", MemoryScope::User),
                Timestamp::now(),
            )
            .unwrap_err();
        assert!(matches!(error, ArtifactError::Invalid(_)), "{error}");
        let shared = s
            .save(
                &in_group,
                &new("Ours", "x", MemoryScope::Room),
                Timestamp::now(),
            )
            .unwrap();
        assert_eq!(s.list(&who("bob", "group", false)).len(), 1);
        assert_eq!(s.list(&who("bob", "other", false)).len(), 0);
        assert!(
            s.open(&who("bob", "other", false), shared.short_id(), None)
                .is_err()
        );
    }

    #[test]
    fn sizes_quotas_and_old_versions_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let limits = ArtifactLimits {
            max_bytes: 10,
            quota_owner: 25,
            quota_bot: 1000,
            keep_versions: 2,
        };
        let s = store(dir.path(), limits);
        let ana = who("ana", "dm-ana", true);
        let error = s
            .save(
                &ana,
                &new("Big", "12345678901", MemoryScope::User),
                Timestamp::now(),
            )
            .unwrap_err();
        assert!(matches!(error, ArtifactError::TooLarge(_)), "{error}");
        let a = s
            .save(
                &ana,
                &new("A", "1234567890", MemoryScope::User),
                Timestamp::now(),
            )
            .unwrap();
        for content in ["abc", "def"] {
            let mut next = new("", content, MemoryScope::User);
            next.id = Some(a.id.clone());
            s.save(&ana, &next, Timestamp::now()).unwrap();
        }
        let (a, _, _) = s.open(&ana, &a.id, None).unwrap();
        assert!(a.versions[0].pruned, "only the newest 2 files are kept");
        assert!(s.open(&ana, &a.id, Some(1)).is_err());
        assert_eq!(a.stored_bytes(), 6);
        let error = s
            .save(
                &ana,
                &new("B", "1234567890", MemoryScope::User),
                Timestamp::now(),
            )
            .and_then(|_| {
                s.save(
                    &ana,
                    &new("C", "1234567890", MemoryScope::User),
                    Timestamp::now(),
                )
            })
            .unwrap_err();
        assert!(
            matches!(error, ArtifactError::TooLarge(_)),
            "the owner's quota: {error}"
        );
    }
}
