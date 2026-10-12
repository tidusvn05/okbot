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
    assert!(
        toml.contains(&format!(
            "cli = \"{}",
            env.root().join("bin").canonicalize().unwrap().display()
        )),
        "{toml}"
    );

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
