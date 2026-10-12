//! The machine's bot registry: `~/.config/okbot/bots.toml` maps bot names to directories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
struct File {
    #[serde(default)]
    bots: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Entry {
    dir: PathBuf,
}

/// okbot's per-user configuration directory: `$OKBOT_HOME`, else `$XDG_CONFIG_HOME/okbot`, else
/// `~/.config/okbot`.
pub(crate) fn home() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("OKBOT_HOME") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir).join("okbot"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/okbot"))
}

fn path() -> anyhow::Result<PathBuf> {
    Ok(home()?.join("bots.toml"))
}

fn load() -> anyhow::Result<File> {
    let path = path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("{} is invalid", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn save(file: &File) -> anyhow::Result<()> {
    let path = path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(file)?)
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Records `name` → `dir` (absolute). Fails if `name` points elsewhere.
pub(crate) fn register(name: &str, dir: &Path) -> anyhow::Result<()> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut file = load()?;
    if let Some(existing) = file.bots.get(name)
        && existing.dir != dir
        && existing.dir.join(okbot_config::CONFIG_FILE).exists()
    {
        bail!(
            "a bot named `{name}` already lives in {}; choose another name",
            existing.dir.display()
        );
    }
    file.bots.insert(name.to_string(), Entry { dir });
    save(&file)
}

/// Forgets `name`.
pub(crate) fn unregister(name: &str) -> anyhow::Result<bool> {
    let mut file = load()?;
    let removed = file.bots.remove(name).is_some();
    if removed {
        save(&file)?;
    }
    Ok(removed)
}

/// Registered bots, by name.
pub(crate) fn all() -> anyhow::Result<Vec<(String, PathBuf)>> {
    Ok(load()?.bots.into_iter().map(|(n, e)| (n, e.dir)).collect())
}

/// The bot directory for `name`, or the current directory when `name` is `None`.
pub(crate) fn resolve(name: Option<&str>) -> anyhow::Result<PathBuf> {
    match name {
        Some(name) => {
            let file = load()?;
            match file.bots.get(name) {
                Some(entry) => Ok(entry.dir.clone()),
                None => {
                    let as_path = PathBuf::from(name);
                    if as_path.join(okbot_config::CONFIG_FILE).exists() {
                        return Ok(as_path);
                    }
                    bail!("no bot named `{name}`; `okbot list` shows the registered bots")
                }
            }
        }
        None => {
            let cwd = std::env::current_dir()?;
            if cwd.join(okbot_config::CONFIG_FILE).exists() {
                Ok(cwd)
            } else {
                bail!(
                    "no okbot.toml in {}; pass a bot name or run `okbot init` here",
                    cwd.display()
                )
            }
        }
    }
}

/// The registered name of the bot in `dir`, or the directory name.
pub(crate) fn name_of(dir: &Path) -> String {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    all()
        .ok()
        .and_then(|bots| {
            bots.into_iter()
                .find(|(_, d)| *d == canonical)
                .map(|(n, _)| n)
        })
        .unwrap_or_else(|| {
            canonical
                .file_name()
                .map_or_else(|| "bot".into(), |n| n.to_string_lossy().into_owned())
        })
}
