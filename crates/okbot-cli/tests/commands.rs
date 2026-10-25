//! `okbot new`, `list`, `doctor`, `kb`, `remove` and `help-agent` with a fake agent CLI.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

struct Env {
    dir: tempfile::TempDir,
    path: String,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        okbot_testkit::fake_cli(
            &bin,
            "claude",
            r#"if [ "$1" = auth ]; then echo '{"loggedIn": true}'; exit 0; fi
cat >/dev/null; printf '%s\n' '{"is_error":false,"result":"ok"}'"#,
        );
        let path = format!("{}:/usr/bin:/bin", bin.display());
        Self { dir, path }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn okbot(&self) -> Command {
        let mut cmd = Command::cargo_bin("okbot").unwrap();
        cmd.current_dir(self.root())
            .env("PATH", &self.path)
            .env("HOME", self.root())
            .env("OKBOT_HOME", self.root().join("okbot-home"))
            .env_remove("XDG_CONFIG_HOME")
            .timeout(std::time::Duration::from_secs(120));
        cmd
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.okbot().args(args).write_stdin("").output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let out = self.okbot().args(args).write_stdin("").output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
        (out.status.code().unwrap_or(-1), value)
    }
}

#[test]
fn missing_answers_exit_3_with_the_flag() {
    let env = Env::new();
    let (code, json) = env.json(&["new", "shop", "--agent", "claude", "--json"]);
    assert_eq!(code, 3, "{json}");
    assert_eq!(json["error"]["code"], "needs_input");
    assert_eq!(json["error"]["flag"], "--platform");
}

#[test]
fn new_list_doctor_kb_remove() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "shop",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    let dir = PathBuf::from(json["dir"].as_str().unwrap());
    assert!(dir.is_absolute());
    for file in [
        "okbot.toml",
        "kb/index.md",
        "kb/persona/assistant.md",
        ".gitignore",
    ] {
        assert!(dir.join(file).exists(), "{file}");
    }
    let toml = std::fs::read_to_string(dir.join("okbot.toml")).unwrap();
    assert!(toml.contains("chain = [\"claude\"]"));
    // Found at each start, not written: the CLI may move (an update, another machine).
    assert!(!toml.contains("\ncli = "), "{toml}");

    let (code, _) = env.json(&[
        "new",
        "shop",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--json",
    ]);
    assert_eq!(code, 1, "existing okbot.toml is not replaced");

    let list = env.okbot().arg("list").output().unwrap();
    assert!(String::from_utf8_lossy(&list.stdout).contains("shop"));

    let (code, report) = env.json(&["doctor", "shop", "--json"]);
    assert_eq!(code, 0, "{report:#}");
    let checks: Vec<(&str, &str)> = report["bots"][0]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["check"].as_str().unwrap(), c["status"].as_str().unwrap()))
        .collect();
    assert!(checks.contains(&("agent claude", "ok")), "{checks:?}");
    assert!(checks.contains(&("knowledge MCP", "ok")), "{checks:?}");

    // A configured CLI that is gone: the bot falls back to the one it finds, and doctor says so.
    std::fs::write(
        dir.join("okbot.toml"),
        toml.replace(
            "[agent.providers.claude]\n",
            "[agent.providers.claude]\ncli = \"/gone/claude\"\n",
        ),
    )
    .unwrap();
    let (code, report) = env.json(&["doctor", "shop", "--json"]);
    assert_eq!(code, 0, "{report:#}");
    let checks: Vec<(&str, &str)> = report["bots"][0]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["check"].as_str().unwrap(), c["status"].as_str().unwrap()))
        .collect();
    assert!(checks.contains(&("agent claude", "ok")), "{checks:?}");
    assert!(checks.contains(&("agent claude cli", "warn")), "{checks:?}");
    // The chain is built (CliBackend::new) from the CLI found instead.
    assert!(checks.contains(&("tool policy", "ok")), "{checks:?}");
    env.okbot()
        .args(["agent", "set", "claude", "--default-cli", "--bot", "shop"])
        .assert()
        .success();
    let toml = std::fs::read_to_string(dir.join("okbot.toml")).unwrap();
    assert!(!toml.contains("/gone/claude"), "{toml}");

    env.okbot().args(["kb", "lint", "shop"]).assert().success();
    std::fs::remove_file(dir.join("kb/persona/assistant.md")).unwrap();
    let upgrade = env
        .okbot()
        .args(["kb", "upgrade", "shop"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&upgrade.stdout).contains("persona/assistant.md"));

    env.okbot().args(["remove", "shop"]).assert().success();
    let list = env.okbot().arg("list").output().unwrap();
    assert!(!String::from_utf8_lossy(&list.stdout).contains("shop "));
    assert!(
        dir.join("okbot.toml").exists(),
        "remove keeps the directory"
    );
}

#[test]
fn doctor_reports_failures_with_exit_4() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "shop",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    std::fs::remove_dir_all(env.root().join("shop/kb")).unwrap();
    let (code, report) = env.json(&["doctor", "shop", "--json"]);
    assert_eq!(code, 4, "{report:#}");
    assert_eq!(report["ok"], false);
}

#[test]
fn help_agent_prints_the_contract() {
    let env = Env::new();
    let out = env.okbot().arg("help-agent").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Exit codes"));
}

#[test]
fn service_commands_drive_systemd() {
    let env = Env::new();
    let bin = env.root().join("bin");
    let calls = env.root().join("systemctl.log");
    for tool in ["systemctl", "loginctl"] {
        okbot_testkit::fake_cli(
            &bin,
            tool,
            &format!(
                r#"echo "{tool} $*" >> "{log}"
case "$*" in *is-active*) echo active;; *is-enabled*) echo enabled;; esac"#,
                log = calls.display()
            ),
        );
    }
    let okbot = |args: &[&str]| {
        let mut cmd = env.okbot();
        cmd.env("ZTOKEN", "1:x")
            .env("OKBOT_SYSTEMD_DIR", env.root().join("units"))
            .env("OKBOT_SYSTEMCTL", bin.join("systemctl"))
            .env("OKBOT_LOGINCTL", bin.join("loginctl"))
            .args(args)
            .write_stdin("");
        cmd.output().unwrap()
    };
    let out = okbot(&[
        "new",
        "shop",
        "--agent",
        "claude",
        "--platform",
        "zalo",
        "--zalo-token-env",
        "ZTOKEN",
        "--delivery",
        "polling",
        "--no-verify",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = okbot(&["start", "shop"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unit = std::fs::read_to_string(env.root().join("units/okbot-shop.service")).unwrap();
    assert!(unit.contains("ExecStart="), "{unit}");
    assert!(unit.contains(&format!(
        "WorkingDirectory={}",
        env.root().join("shop").canonicalize().unwrap().display()
    )));
    assert!(unit.contains(&format!("Environment=PATH={}", env.path)));

    let status = okbot(&["status", "shop"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("shop: active (enabled)"));
    let all = okbot(&["status", "--all"]);
    assert!(String::from_utf8_lossy(&all.stdout).contains("shop: active (enabled)"));
    assert!(okbot(&["stop", "shop"]).status.success());
    assert!(okbot(&["remove", "shop"]).status.success());
    assert!(!env.root().join("units/okbot-shop.service").exists());

    let log = std::fs::read_to_string(&calls).unwrap();
    for expected in [
        "systemctl --user daemon-reload",
        "systemctl --user enable --now okbot-shop.service",
        "loginctl enable-linger",
        "systemctl --user stop okbot-shop.service",
        "systemctl --user disable --now okbot-shop.service",
    ] {
        assert!(log.contains(expected), "missing {expected:?} in\n{log}");
    }
}

#[test]
fn channels_and_people() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "home",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    let run = |args: &[&str]| {
        let out = env.okbot().args(args).write_stdin("").output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    // Missing answers name their flag (exit 3), like `okbot new`.
    let (code, _, err) = run(&["channel", "add", "discord", "--bot", "home", "--yes"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("--discord-token-env"), "{err}");

    let (code, _, err) = run(&[
        "channel",
        "add",
        "discord",
        "--bot",
        "home",
        "--discord-token-env",
        "MY_DISCORD",
        "-y",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Added discord:home"), "{err}");
    let (code, _, err) = run(&[
        "channel",
        "add",
        "discord",
        "--bot",
        "home",
        "--discord-token-env",
        "MY_DISCORD",
        "-y",
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("already exists"), "{err}");
    let (code, _, err) = run(&[
        "channel",
        "add",
        "slack",
        "--bot",
        "home",
        "--slack-app-token-env",
        "SA",
        "--slack-bot-token-env",
        "SB",
        "-y",
    ]);
    assert_eq!(code, 0, "{err}");

    let (_, out, _) = run(&["channel", "list", "--bot", "home"]);
    assert!(
        out.contains("discord:home") && out.contains("missing MY_DISCORD"),
        "{out}"
    );
    assert!(out.contains("slack:home"), "{out}");
    let toml = std::fs::read_to_string(env.root().join("home/okbot.toml")).unwrap();
    assert!(
        toml.contains("# okbot bot configuration"),
        "comments are kept"
    );

    // People: link accounts across platforms.
    let (code, _, err) = run(&[
        "people",
        "link",
        "luan",
        "discord:home/123",
        "slack:home/U9",
        "--bot",
        "home",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run(&["people", "link", "Luan", "discord:home/5", "--bot", "home"]);
    assert_ne!(code, 0, "invalid person id");
    assert!(err.contains("may only contain"), "{err}");
    let (_, out, _) = run(&["people", "list", "--bot", "home"]);
    assert!(
        out.contains("luan") && out.contains("discord:home/123"),
        "{out}"
    );

    // Removing a channel warns about accounts that pointed at it.
    let (code, _, err) = run(&["channel", "remove", "discord:home", "--bot", "home"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("discord:home/123"), "{err}");
    let (_, out, _) = run(&["channel", "list", "--bot", "home"]);
    assert!(!out.contains("discord:home"), "{out}");

    let (code, _, err) = run(&["people", "unlink", "luan", "slack:home/U9", "--bot", "home"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = run(&["people", "remove", "luan", "--bot", "home"]);
    assert_eq!(code, 0);
    let (_, out, _) = run(&["people", "list", "--bot", "home"]);
    assert!(out.contains("no people linked"), "{out}");
}

#[test]
fn personal_profile_and_pairing_codes() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "me",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--profile",
        "personal",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    assert_eq!(json["profile"], "personal");
    let toml = std::fs::read_to_string(env.root().join("me/okbot.toml")).unwrap();
    assert!(toml.contains("profile = \"personal\""), "{toml}");

    let run = |args: &[&str]| {
        let out = env.okbot().args(args).write_stdin("").output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (_, out, _) = run(&["profile", "--bot", "me"]);
    assert!(out.starts_with("personal"), "{out}");
    let (code, out, _) = run(&["pair", "me", "--list"]);
    assert_eq!(code, 0);
    assert!(out.contains("no paired accounts"), "{out}");
    let (code, out, err) = run(&["pair", "me"]);
    assert_eq!(code, 0, "{err}");
    let pair_line = out
        .lines()
        .find(|l| l.trim().starts_with("/pair "))
        .unwrap();
    let digits = pair_line.trim().trim_start_matches("/pair ");
    assert_eq!(digits.len(), 6, "{out}");
    assert!(digits.chars().all(|c| c.is_ascii_digit()), "{out}");
    let (code, _, err) = run(&["pair", "me", "--remove", "zalo:me/nobody"]);
    assert_ne!(code, 0);
    assert!(err.contains("not paired"), "{err}");

    let (code, _, err) = run(&["profile", "team", "--bot", "me"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run(&["pair", "me"]);
    assert_ne!(code, 0);
    assert!(err.contains("team bot"), "{err}");

    // Without --profile and without a terminal, new bots are team bots.
    let (code, json) = env.json(&[
        "new",
        "shop",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    assert_eq!(json["profile"], "team");
}

#[test]
fn tasks_from_the_terminal() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "me",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--profile",
        "personal",
        "--timezone",
        "Asia/Ho_Chi_Minh",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    let run = |args: &[&str]| {
        let out = env.okbot().args(args).write_stdin("").output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (code, out, err) = run(&[
        "task",
        "add",
        "Buy milk",
        "--bot",
        "me",
        "--due",
        "2099-01-02",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("tasks/people/owner/t-"), "{out}");
    assert!(out.contains("due 2099-01-02 09:00"), "{out}");
    let (code, _, err) = run(&[
        "task",
        "add",
        "Standup",
        "--bot",
        "me",
        "--repeat",
        "weekdays 09:00",
    ]);
    assert_eq!(code, 0, "{err}");
    let (_, _, err) = run(&[
        "task",
        "add",
        "Late",
        "--bot",
        "me",
        "--remind",
        "2001-01-01 08:00",
    ]);
    assert!(err.contains("in the past"), "{err}");

    let (_, out, _) = run(&["task", "list", "me"]);
    assert!(out.contains("[todo, open] Buy milk"), "{out}");
    assert!(out.contains("[reminder, open] Standup"), "{out}");
    assert!(out.contains("(weekdays 09:00)"), "{out}");
    let (_, upcoming, _) = run(&["task", "list", "me", "--upcoming"]);
    assert!(
        upcoming.contains("Standup") && !upcoming.contains("Buy milk"),
        "{upcoming}"
    );
    let id = out
        .lines()
        .find(|l| l.contains("Buy milk"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    let (code, out, err) = run(&["task", "done", &id, "--bot", "me"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("done\n") && out.contains("Buy milk"),
        "{out}"
    );
    let (_, out, _) = run(&["task", "list", "me"]);
    assert!(!out.contains("Buy milk"), "{out}");
    let (code, _, err) = run(&["task", "rm", &id, "--bot", "me"]);
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = run(&["task", "list", "me", "--all"]);
    assert!(!out.contains("Buy milk"), "{out}");

    let kb = env.root().join("me/kb");
    let index = std::fs::read_to_string(kb.join("tasks/people/owner/index.md")).unwrap();
    assert!(index.contains("Standup"), "{index}");
    // Written tasks keep the bundle lint-clean.
    let (code, out, err) = run(&["kb", "lint", "me"]);
    assert_eq!(code, 0, "{out}{err}");
}

#[test]
fn routines_from_the_terminal() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "me",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--profile",
        "personal",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    let (code, out, err) = env.run(&[
        "task",
        "add",
        "Morning brief",
        "--bot",
        "me",
        "--repeat",
        "daily 07:00",
        "--routine",
        "Summarise my tasks for today",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("[routine, open] Morning brief"), "{out}");
    let routine = out
        .lines()
        .find(|l| l.contains("Morning brief"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_string();
    let (_, out, _) = env.run(&["task", "pause", &routine, "--bot", "me"]);
    assert!(out.contains("[routine, paused]"), "{out}");
    let (_, out, _) = env.run(&["task", "resume", &routine, "--bot", "me"]);
    assert!(
        out.starts_with("resumed") && out.contains("[routine, open]"),
        "{out}"
    );
}

#[test]
fn documents_and_notes() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "me",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--profile",
        "personal",
        "--timezone",
        "Asia/Ho_Chi_Minh",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    std::fs::write(
        env.root().join("refunds.md"),
        "---\ntitle: x\n---\n# Refunds\n\nRefunds within 7 days.\n",
    )
    .unwrap();
    std::fs::write(env.root().join("hours.txt"), "Open 8:00-17:00.\n").unwrap();
    let (code, out, err) = env.run(&[
        "kb",
        "add",
        "refunds.md",
        "--bot",
        "me",
        "--title",
        "Refund policy",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("saved knowledge/inbox/"), "{out}");
    let (code, _, err) = env.run(&["kb", "add", "hours.txt", "--bot", "me"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = env.run(&["kb", "add", "missing.pdf", "--bot", "me"]);
    assert_ne!(code, 0);
    assert!(err.contains("cannot read"), "{err}");
    let (_, out, _) = env.run(&["kb", "inbox", "me"]);
    assert!(
        out.contains("Refund policy") && out.contains("hours"),
        "{out}"
    );

    // `/note` from the bot's own terminal (the owner) lands in today's journal.
    let out = env
        .okbot()
        .current_dir(env.root().join("me"))
        .args(["chat"])
        .write_stdin("/note call the bank\n")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Noted."), "{stdout}");
    let notes = env.root().join("me/kb/notes/people/owner");
    let year = std::fs::read_dir(&notes)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .unwrap();
    let day = std::fs::read_dir(&year)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name() != "index.md")
        .unwrap()
        .path();
    let text = std::fs::read_to_string(day).unwrap();
    assert!(text.contains(" call the bank\n"), "{text}");

    let (code, out, err) = env.run(&["kb", "lint", "me"]);
    assert_eq!(code, 0, "{out}{err}");
}

#[test]
fn roles_and_audit_from_the_terminal() {
    let env = Env::new();
    let (code, json) = env.json(&[
        "new",
        "shop",
        "--agent",
        "claude",
        "--platform",
        "none",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    let path = env.root().join("shop/okbot.toml");
    let mut toml = std::fs::read_to_string(&path).unwrap();
    toml.push_str("\n[roles]\nops = [\"discord:main/1\"]\n");
    std::fs::write(&path, toml).unwrap();

    let (code, out, err) = env.run(&["role", "list", "shop"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("ops: discord:main/1"), "{out}");
    let (code, _, err) = env.run(&[
        "role",
        "grant",
        "admins",
        "discord:main/2",
        "--for",
        "2h",
        "--bot",
        "shop",
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("not in [roles]"), "{err}");
    let (code, _, err) = env.run(&[
        "role",
        "grant",
        "ops",
        "discord:main/2",
        "--for",
        "40d",
        "--bot",
        "shop",
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("1m to 30d"), "{err}");
    let (code, _, err) = env.run(&[
        "role",
        "grant",
        "ops",
        "person:nobody",
        "--for",
        "2h",
        "--bot",
        "shop",
    ]);
    assert_ne!(code, 0, "{err}");
    let (code, _, err) = env.run(&[
        "role",
        "grant",
        "ops",
        "discord:main/2",
        "--for",
        "2h",
        "--bot",
        "shop",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("discord:main/2 has `ops` until"), "{err}");
    let (_, out, _) = env.run(&["role", "list", "shop"]);
    assert!(out.contains("ops: discord:main/2 until"), "{out}");
    let (code, _, err) = env.run(&["role", "revoke", "ops", "discord:main/2", "--bot", "shop"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = env.run(&["role", "revoke", "ops", "discord:main/2", "--bot", "shop"]);
    assert_ne!(code, 0);
    assert!(err.contains("no grant"), "{err}");

    let (code, out, err) = env.run(&["audit", "shop", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "", "no calls yet");
    let (code, _, err) = env.run(&["audit", "shop", "--since", "2w"]);
    assert_ne!(code, 0);
    assert!(err.contains("not a duration"), "{err}");
}

#[test]
fn team_renew_signs_only_when_due() {
    let env = Env::new();
    let run = |args: &[&str]| {
        let out = env.okbot().args(args).write_stdin("").output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (code, _, err) = run(&["team", "init", "ops", "--owner"]);
    assert_eq!(code, 0, "{err}");
    // Not signed yet: renew signs.
    let (code, out, err) = run(&["team", "renew", "ops"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("version 2"), "{out}");
    assert!(err.contains("valid for 365 days"), "{err}");
    // Valid for a year: nothing to do.
    let (code, out, err) = run(&["team", "renew", "ops"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("nothing to do"), "{out}");
    // Not due within 60 days; due within 366, so signed again and published.
    let site = env.root().join("site");
    let (code, out, err) = run(&[
        "team",
        "renew",
        "ops",
        "--within",
        "60",
        "--days",
        "90",
        "--to",
        site.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("nothing to do"), "{out}");
    let (code, out, err) = run(&[
        "team",
        "renew",
        "ops",
        "--within",
        "366",
        "--days",
        "400",
        "--to",
        site.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("version 3"), "{out}");
    assert!(site.join("ops.toml.sig").exists());
    let (code, _, err) = run(&["team", "sign", "ops", "--days", "90"]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("note:"), "{err}");
    let (code, _, err) = run(&["team", "renew", "ops", "--within", "90", "--days", "90"]);
    assert_ne!(code, 0);
    assert!(err.contains("--within"), "{err}");
}
