//! Finding installed agent CLIs (design §6.3.1).
//!
//! okbot usually runs on a VPS where CLIs live in user-level directories that a service's `PATH`
//! misses, so detection also looks in the usual install locations and records absolute paths.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use agent_call::Provider;
use tokio::process::Command;

use crate::{PROVIDERS, provider_name};

/// Whether a CLI is logged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// The CLI reports a login.
    LoggedIn,
    /// The CLI reports no login.
    LoggedOut,
    /// The CLI has no reliable status command (OpenCode) or the check failed.
    Unknown,
}

/// One installed CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedCli {
    /// Which CLI.
    pub provider: Provider,
    /// Absolute, symlink-resolved path.
    pub path: PathBuf,
    /// `--version` output (first line).
    pub version: String,
    /// Login state.
    pub login: Login,
}

impl DetectedCli {
    /// Ready to use without asking the operator anything.
    pub fn is_ready(&self) -> bool {
        self.login == Login::LoggedIn
    }
}

/// Where to look.
#[derive(Debug, Clone)]
pub struct SearchPaths {
    /// Directories searched in order.
    pub dirs: Vec<PathBuf>,
}

impl SearchPaths {
    /// `PATH`, then common install locations under `home`.
    pub fn standard(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Self {
        let mut dirs: Vec<PathBuf> = path_var
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default();
        if let Some(home) = home {
            for rel in [
                ".local/bin",
                ".npm-global/bin",
                ".bun/bin",
                ".claude/local",
                ".cargo/bin",
                ".opencode/bin",
            ] {
                dirs.push(home.join(rel));
            }
        }
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        Self { dirs }
    }

    /// The current process's `PATH` and `HOME`.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::standard(std::env::var_os("PATH").as_deref(), home.as_deref())
    }

    /// The first executable named `name`.
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        self.dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

async fn output(
    path: &Path,
    args: &[&str],
    env: &[(String, String)],
) -> Option<std::process::Output> {
    let mut command = Command::new(path);
    command
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(CHECK_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()
}

async fn login(provider: Provider, path: &Path, env: &[(String, String)]) -> Login {
    let args: &[&str] = match provider {
        Provider::Claude | Provider::Devin => &["auth", "status"],
        Provider::Codex => &["login", "status"],
        Provider::OpenCodeV2 => return Login::Unknown,
    };
    let Some(out) = output(path, args, env).await else {
        return Login::Unknown;
    };
    if provider == Provider::Claude {
        // `claude auth status` prints JSON with `loggedIn`.
        return match serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .ok()
            .and_then(|v| v["loggedIn"].as_bool())
        {
            Some(true) => Login::LoggedIn,
            Some(false) => Login::LoggedOut,
            None => Login::Unknown,
        };
    }
    if out.status.success() {
        Login::LoggedIn
    } else {
        Login::LoggedOut
    }
}

/// Detects one CLI.
pub async fn detect_one(provider: Provider, paths: &SearchPaths) -> Option<DetectedCli> {
    detect_one_with_env(provider, paths, &[]).await
}

/// Detects one CLI, checking its login with extra environment (a bot's separate agent home).
pub async fn detect_one_with_env(
    provider: Provider,
    paths: &SearchPaths,
    env: &[(String, String)],
) -> Option<DetectedCli> {
    let path = paths.find(provider.executable())?;
    detect_path(provider, &path, env).await
}

/// Checks exactly this executable, including custom names and paths outside `PATH`.
pub async fn detect_path(
    provider: Provider,
    path: &Path,
    env: &[(String, String)],
) -> Option<DetectedCli> {
    if !is_executable(path) {
        return None;
    }
    let path = std::fs::canonicalize(path).ok()?;
    let out = output(&path, &["--version"], env).await?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let login = login(provider, &path, env).await;
    Some(DetectedCli {
        provider,
        path,
        version,
        login,
    })
}

/// Detects every known CLI, ready ones first, otherwise in [`PROVIDERS`] order.
pub async fn detect(paths: &SearchPaths) -> Vec<DetectedCli> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for provider in PROVIDERS {
        if let Some(cli) = detect_one(provider, paths).await
            && seen.insert(provider_name(provider))
        {
            found.push(cli);
        }
    }
    found.sort_by_key(|c| !c.is_ready());
    found
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use okbot_testkit::fake_cli;

    #[tokio::test]
    async fn finds_cli_outside_path_and_reads_login() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        fake_cli(
            &bin,
            "claude",
            r#"if [ "$1" = auth ]; then echo '{"loggedIn": true}'; fi"#,
        );
        fake_cli(&bin, "codex", r#"if [ "$1" = login ]; then exit 1; fi"#);
        let paths = SearchPaths::standard(Some("/nonexistent".as_ref()), Some(home.path()));
        let found = detect(&paths).await;
        let summary: Vec<(&str, Login)> = found
            .iter()
            .map(|c| (provider_name(c.provider), c.login))
            .collect();
        assert_eq!(
            summary,
            [("claude", Login::LoggedIn), ("codex", Login::LoggedOut)]
        );
        assert_eq!(found[0].version, "1.0.0");
        assert!(found[0].path.is_absolute());
    }

    #[tokio::test]
    async fn nothing_found() {
        let empty = tempfile::tempdir().unwrap();
        let paths = SearchPaths {
            dirs: vec![empty.path().to_path_buf()],
        };
        assert_eq!(detect(&paths).await, []);
    }
}
