//! `okbot start|stop|restart|status|logs`: one systemd unit per bot (design §6.5).
//!
//! As a normal user the unit is a user unit (`~/.config/systemd/user/okbot-<name>.service`, kept
//! running after logout with `loginctl enable-linger`); as root it is a system unit. The unit runs
//! `okbot run` in the bot directory with the `PATH` of the shell that installed it, because agent
//! CLIs usually live in user directories a service's default `PATH` misses.
//!
//! `OKBOT_SYSTEMCTL`, `OKBOT_JOURNALCTL` and `OKBOT_SYSTEMD_DIR` override the tools and the unit
//! directory (tests, unusual layouts).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// `okbot` service subcommands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Start,
    Stop,
    Restart,
    Status,
}

/// Arguments shared by the service commands.
#[derive(Debug, clap::Args)]
pub(crate) struct ServiceArgs {
    /// Bot name; default: the bot in the current directory.
    name: Option<String>,
}

/// Arguments of `okbot logs`.
#[derive(Debug, clap::Args)]
pub(crate) struct LogsArgs {
    /// Bot name; default: the bot in the current directory.
    name: Option<String>,
    /// Keep printing new lines.
    #[arg(long, short = 'f')]
    follow: bool,
    /// Number of lines.
    #[arg(long, short = 'n', default_value_t = 100)]
    lines: u32,
}

/// Whether okbot runs as root (then system units are used).
fn is_root() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // `/proc/self` belongs to the effective user; fall back to $USER elsewhere.
        if let Ok(meta) = std::fs::metadata("/proc/self") {
            return meta.uid() == 0;
        }
    }
    std::env::var("USER").is_ok_and(|u| u == "root")
}

fn tool(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

/// `--user` for user units.
fn scope_args() -> Vec<&'static str> {
    if is_root() { vec![] } else { vec!["--user"] }
}

fn unit_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("OKBOT_SYSTEMD_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if is_root() {
        return Ok(PathBuf::from("/etc/systemd/system"));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("HOME is not set")?;
    Ok(base.join("systemd/user"))
}

/// The unit name for a bot.
pub(crate) fn unit_name(bot: &str) -> String {
    let safe: String = bot
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("okbot-{safe}.service")
}

/// The unit file text.
pub(crate) fn render_unit(bot: &str, dir: &Path, okbot: &Path, path_var: &str) -> String {
    format!(
        "# Written by `okbot start {bot}`. Edit okbot.toml instead of this file.\n\
         [Unit]\n\
         Description=okbot bot {bot}\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         WorkingDirectory={dir}\n\
         ExecStart={okbot} run\n\
         Environment=PATH={path_var}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # Stop gives running answers time to finish (okbot waits up to 30 s).\n\
         TimeoutStopSec=45\n\
         NoNewPrivileges=true\n\
         \n\
         [Install]\n\
         WantedBy={wanted}\n",
        dir = dir.display(),
        okbot = okbot.display(),
        wanted = if is_root() {
            "multi-user.target"
        } else {
            "default.target"
        },
    )
}

fn systemctl(args: &[&str]) -> anyhow::Result<std::process::Output> {
    let program = tool("OKBOT_SYSTEMCTL", "systemctl");
    let mut command = Command::new(&program);
    command.args(scope_args()).args(args);
    command
        .output()
        .with_context(|| format!("cannot run {program} (is systemd available? use `okbot run` under your own supervisor otherwise)"))
}

fn check(output: &std::process::Output, what: &str) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// Writes (or refreshes) the unit file. Returns its path.
fn install(bot: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let okbot = std::env::current_exe().context("cannot locate the okbot executable")?;
    let okbot = std::fs::canonicalize(&okbot).unwrap_or(okbot);
    let path_var = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    let text = render_unit(bot, dir, &okbot, &path_var);
    let units = unit_dir()?;
    std::fs::create_dir_all(&units)?;
    let file = units.join(unit_name(bot));
    if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&file, text).with_context(|| format!("cannot write {}", file.display()))?;
        check(&systemctl(&["daemon-reload"])?, "systemctl daemon-reload")?;
    }
    Ok(file)
}

/// Keeps user services running after logout. Failure is reported, not fatal.
fn enable_linger() {
    if is_root() {
        return;
    }
    let user = std::env::var("USER").unwrap_or_default();
    let linger = tool("OKBOT_LOGINCTL", "loginctl");
    let ok = Command::new(&linger)
        .args(["enable-linger", &user])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!(
            "note: run `sudo loginctl enable-linger {user}` so the bot keeps running after you log out and starts at boot"
        );
    }
}

pub(crate) fn run(action: Action, args: &ServiceArgs) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(args.name.as_deref())?;
    let config = okbot_config::Config::load(&dir)?;
    let bot = crate::registry::name_of(&dir);
    let unit = unit_name(&bot);
    match action {
        Action::Start => {
            if config.channels.is_empty() {
                bail!("`{bot}` has no channel; add one with `okbot init --force` first");
            }
            let file = install(&bot, &dir)?;
            check(
                &systemctl(&["enable", "--now", &unit])?,
                "systemctl enable --now",
            )?;
            enable_linger();
            println!("started {bot} ({})", file.display());
            println!("logs: okbot logs {bot} -f");
        }
        Action::Stop => {
            check(&systemctl(&["stop", &unit])?, "systemctl stop")?;
            println!("stopped {bot}");
        }
        Action::Restart => {
            install(&bot, &dir)?;
            check(&systemctl(&["restart", &unit])?, "systemctl restart")?;
            println!("restarted {bot}");
        }
        Action::Status => {
            let state = systemctl(&["is-active", &unit])?;
            let state = String::from_utf8_lossy(&state.stdout).trim().to_string();
            let enabled = systemctl(&["is-enabled", &unit])?;
            let enabled = String::from_utf8_lossy(&enabled.stdout).trim().to_string();
            println!(
                "{bot}: {} ({})",
                if state.is_empty() { "unknown" } else { &state },
                if enabled.is_empty() {
                    "not installed"
                } else {
                    &enabled
                }
            );
        }
    }
    Ok(())
}

pub(crate) fn logs(args: &LogsArgs) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(args.name.as_deref())?;
    let bot = crate::registry::name_of(&dir);
    let program = tool("OKBOT_JOURNALCTL", "journalctl");
    let lines = args.lines.to_string();
    let mut command = Command::new(&program);
    command.args(scope_args()).args([
        "-u",
        &unit_name(&bot),
        "-n",
        &lines,
        "--no-pager",
        "-o",
        "cat",
    ]);
    if args.follow {
        command.arg("-f");
    }
    let status = command
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// Stops and removes the bot's unit, if installed (`okbot remove`).
pub(crate) fn uninstall(bot: &str) -> anyhow::Result<bool> {
    let file = unit_dir()?.join(unit_name(bot));
    if !file.exists() {
        return Ok(false);
    }
    let unit = unit_name(bot);
    let _ = systemctl(&["disable", "--now", &unit]);
    std::fs::remove_file(&file)?;
    let _ = systemctl(&["daemon-reload"]);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_are_safe() {
        assert_eq!(unit_name("shop"), "okbot-shop.service");
        assert_eq!(unit_name("my shop/x"), "okbot-my-shop-x.service");
    }

    #[test]
    fn unit_text() {
        let text = render_unit(
            "shop",
            Path::new("/srv/bots/shop"),
            Path::new("/usr/local/bin/okbot"),
            "/home/u/.local/bin:/usr/bin",
        );
        assert!(text.contains("WorkingDirectory=/srv/bots/shop\n"));
        assert!(text.contains("ExecStart=/usr/local/bin/okbot run\n"));
        assert!(text.contains("Environment=PATH=/home/u/.local/bin:/usr/bin\n"));
        assert!(text.contains("Restart=on-failure"));
    }
}
