//! The apps of one bot: the built-in ones and those installed in `<bot>/apps/`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use include_dir::{Dir, include_dir};
use okbot_core::{AppDirectory, AppInfo, AppSource};

use crate::manifest::{ENTRY, MANIFEST, MAX_APP, MAX_FILE, MAX_FILES, Manifest, valid_path};

static BUILTIN: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/builtin/apps");

/// The file in an installed app's folder that records where it came from (written by the
/// installer; `local` when missing).
pub const SOURCE_FILE: &str = ".okbot-source";

/// How often the folder is looked at again for new or changed apps.
const RESCAN: Duration = Duration::from_secs(2);

/// Where an app's files are.
#[derive(Debug, Clone)]
enum Files {
    Embedded(&'static Dir<'static>),
    Disk(PathBuf),
}

/// One app with its files.
#[derive(Debug, Clone)]
pub struct App {
    /// Its manifest.
    pub manifest: Manifest,
    files: Files,
}

impl App {
    /// A file of the app, by its path inside the app; `None` when it is missing or not a file an
    /// app may serve (type, size, a link).
    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        if !valid_path(path) {
            return None;
        }
        match &self.files {
            // Embedded paths are relative to the top of the embedded tree.
            Files::Embedded(dir) => dir
                .get_file(dir.path().join(path))
                .map(|f| f.contents().to_vec()),
            Files::Disk(root) => read_inside(root, path),
        }
    }
}

/// Reads `root/path` when no part of it is a symbolic link and the file is small enough.
fn read_inside(root: &Path, path: &str) -> Option<Vec<u8>> {
    let mut at = root.to_path_buf();
    for part in path.split('/') {
        at.push(part);
        let meta = std::fs::symlink_metadata(&at).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
    }
    let meta = std::fs::symlink_metadata(&at).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    std::fs::read(&at).ok()
}

/// Checks an app folder against the file rules and returns its manifest: every file of an
/// allowed type and size, no links, an entry page, at most [`MAX_APP`] bytes.
pub fn check_folder(dir: &Path, source: AppSource) -> Result<Manifest, crate::Error> {
    let bad = |m: String| crate::Error::Files(m);
    let mut total = 0u64;
    let mut count = 0usize;
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((at, prefix)) = stack.pop() {
        let entries = std::fs::read_dir(&at).map_err(|e| bad(format!("{}: {e}", at.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| bad(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // okbot's own records (where it came from, yanked) are not the app's.
            if prefix.is_empty() && name.starts_with(".okbot-") && kind_is_file(&entry) {
                continue;
            }
            let rel = format!("{prefix}{name}");
            let kind = entry.file_type().map_err(|e| bad(e.to_string()))?;
            if kind.is_symlink() {
                return Err(bad(format!("{rel}: links are not allowed")));
            }
            if kind.is_dir() {
                if name.starts_with('.') {
                    return Err(bad(format!("{rel}: hidden folders are not allowed")));
                }
                stack.push((entry.path(), format!("{rel}/")));
                continue;
            }
            if !valid_path(&rel) {
                return Err(bad(format!(
                    "{rel}: not a file an app may contain (html, js, css, json, svg, png, webp, woff2, txt, md)"
                )));
            }
            let len = entry.metadata().map_err(|e| bad(e.to_string()))?.len();
            if len > MAX_FILE {
                return Err(bad(format!("{rel}: larger than {} MB", MAX_FILE >> 20)));
            }
            total += len;
            count += 1;
        }
    }
    if total > MAX_APP {
        return Err(bad(format!("the app is larger than {} MB", MAX_APP >> 20)));
    }
    if count > MAX_FILES {
        return Err(bad(format!("more than {MAX_FILES} files")));
    }
    if !dir.join(ENTRY).is_file() {
        return Err(bad(format!("no {ENTRY}")));
    }
    let text =
        std::fs::read_to_string(dir.join(MANIFEST)).map_err(|e| bad(format!("{MANIFEST}: {e}")))?;
    let manifest = Manifest::parse(&text, source)?;
    let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned());
    if folder.as_deref() != Some(manifest.info.name.as_str()) {
        return Err(bad(format!(
            "the folder must be named like the app (`{}`)",
            manifest.info.name
        )));
    }
    Ok(manifest)
}

fn kind_is_file(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|t| t.is_file())
}

/// The file the installer writes into an app the registry yanked: the reason.
pub const YANKED_FILE: &str = ".okbot-yanked";

/// Where an installed app says it came from.
pub fn source_of(dir: &Path) -> AppSource {
    match std::fs::read_to_string(dir.join(SOURCE_FILE)) {
        Ok(s) if s.lines().next().map(str::trim) == Some("registry") => AppSource::Registry,
        _ => AppSource::Local,
    }
}

/// The built-in apps.
pub fn builtin() -> Vec<App> {
    let mut apps = Vec::new();
    for dir in BUILTIN.dirs() {
        let Some(text) = dir
            .get_file(dir.path().join(MANIFEST))
            .and_then(|f| f.contents_utf8())
        else {
            continue;
        };
        match Manifest::parse(text, AppSource::Builtin) {
            Ok(manifest) => apps.push(App {
                manifest,
                files: Files::Embedded(dir),
            }),
            Err(error) => tracing::error!(%error, "a built-in app is broken"),
        }
    }
    apps.sort_by(|a, b| a.manifest.info.name.cmp(&b.manifest.info.name));
    apps
}

#[derive(Debug, Default)]
struct Scan {
    at: Option<Instant>,
    modified: Option<SystemTime>,
    installed: Vec<App>,
}

/// Every app of a bot, rescanned when its folder changes.
#[derive(Debug, Clone)]
pub struct Catalog {
    builtin: Arc<Vec<App>>,
    dir: Option<PathBuf>,
    scan: Arc<Mutex<Scan>>,
    refuse_yanked: bool,
}

impl Catalog {
    /// The built-in apps and those in `dir` (usually `<bot>/apps`), when given.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            builtin: Arc::new(builtin()),
            dir,
            scan: Arc::default(),
            refuse_yanked: true,
        }
    }

    /// Serves installed versions the registry yanked too (`[apps] refuse_yanked = false`).
    #[must_use]
    pub fn serving_yanked(mut self, serve: bool) -> Self {
        self.refuse_yanked = !serve;
        self
    }

    /// Where installed apps live.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn installed(&self) -> Vec<App> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let mut scan = self
            .scan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if scan.at.is_some_and(|at| at.elapsed() < RESCAN) {
            return scan.installed.clone();
        }
        scan.at = Some(Instant::now());
        // A changed app changes its folder's time (the installer renames it into place).
        let modified = newest(dir);
        if modified.is_some() && modified == scan.modified {
            return scan.installed.clone();
        }
        scan.modified = modified;
        scan.installed = self.read_installed(dir);
        scan.installed.clone()
    }

    fn read_installed(&self, dir: &Path) -> Vec<App> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut apps = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if self.refuse_yanked && path.join(YANKED_FILE).is_file() {
                tracing::warn!(app = %name, "the registry yanked this version; not served");
                continue;
            }
            if self.builtin.iter().any(|a| a.manifest.info.name == name) {
                tracing::warn!(app = %name, "an installed app has the name of a built-in app; ignored");
                continue;
            }
            match check_folder(&path, source_of(&path)) {
                Ok(manifest) => apps.push(App {
                    manifest,
                    files: Files::Disk(path),
                }),
                Err(error) => tracing::warn!(app = %name, %error, "an installed app is not served"),
            }
        }
        apps.sort_by(|a, b| a.manifest.info.name.cmp(&b.manifest.info.name));
        apps
    }

    /// Every app, built-in first.
    pub fn all(&self) -> Vec<App> {
        let mut all: Vec<App> = self.builtin.iter().cloned().collect();
        all.extend(self.installed());
        all
    }

    /// One app.
    pub fn get(&self, name: &str) -> Option<App> {
        self.all()
            .into_iter()
            .find(|a| a.manifest.info.name == name)
    }
}

/// The newest change time of `dir` and its direct children.
fn newest(dir: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(dir).and_then(|m| m.modified()).ok()?;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if let Ok(t) = entry.metadata().and_then(|m| m.modified()) {
            newest = newest.max(t);
        }
    }
    Some(newest)
}

impl AppDirectory for Catalog {
    fn apps(&self) -> Vec<AppInfo> {
        self.all().into_iter().map(|a| a.manifest.info).collect()
    }

    fn app(&self, name: &str) -> Option<AppInfo> {
        self.get(name).map(|a| a.manifest.info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_app(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST),
            format!("name = \"{name}\"\ntitle = \"T\"\nversion = \"1.0.0\"\napi = 1\n"),
        )
        .unwrap();
        std::fs::write(dir.join(ENTRY), "<p>hi</p>").unwrap();
        dir
    }

    #[test]
    fn builtin_apps_are_valid() {
        let apps = builtin();
        assert!(apps.iter().any(|a| a.manifest.info.name == "tasks"));
        for app in apps {
            assert!(
                app.file(ENTRY).is_some(),
                "{} has an entry page",
                app.manifest.info.name
            );
            assert_eq!(app.manifest.info.source, AppSource::Builtin);
        }
    }

    #[test]
    fn installed_apps_are_checked_and_found() {
        let root = tempfile::tempdir().unwrap();
        write_app(root.path(), "shop");
        let bad = write_app(root.path(), "bad");
        std::fs::write(bad.join("run.sh"), "rm -rf /").unwrap();
        write_app(root.path(), "tasks");
        let renamed = write_app(root.path(), "renamed");
        std::fs::write(
            renamed.join(MANIFEST),
            "name = \"other\"\ntitle = \"T\"\nversion = \"1\"\napi = 1\n",
        )
        .unwrap();
        let catalog = Catalog::new(Some(root.path().to_path_buf()));
        let names: Vec<String> = catalog.apps().into_iter().map(|a| a.name).collect();
        assert!(names.contains(&"shop".to_string()));
        assert!(
            !names.contains(&"bad".to_string()),
            "a shell script is not served"
        );
        assert!(
            !names.contains(&"other".to_string()),
            "folder and name must match"
        );
        assert_eq!(
            names.iter().filter(|n| *n == "tasks").count(),
            1,
            "built-in wins"
        );
        let shop = catalog.get("shop").unwrap();
        assert_eq!(shop.manifest.info.source, AppSource::Local);
        assert_eq!(shop.file("index.html").unwrap(), b"<p>hi</p>");
        assert!(shop.file("../bad/run.sh").is_none());
        assert!(shop.file("app.toml").is_some(), "the manifest is public");
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed() {
        let root = tempfile::tempdir().unwrap();
        let dir = write_app(root.path(), "shop");
        let secret = root.path().join("secret.txt");
        std::fs::write(&secret, "token").unwrap();
        std::os::unix::fs::symlink(&secret, dir.join("leak.txt")).unwrap();
        assert!(check_folder(&dir, AppSource::Local).is_err());
        let app = App {
            manifest: Manifest::parse(
                "name = \"shop\"\ntitle = \"T\"\nversion = \"1\"\napi = 1\n",
                AppSource::Local,
            )
            .unwrap(),
            files: Files::Disk(dir),
        };
        assert!(app.file("leak.txt").is_none());
    }
}
