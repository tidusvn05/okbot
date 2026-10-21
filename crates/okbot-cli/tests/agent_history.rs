//! Local operator workflows with isolated fake CLIs, services and retained databases.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use okbot_core::{AgentRunRecord, ConversationStore, ToolPolicy};
use serde_json::{Value, json};
use std::path::Path;

struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["claude", "codex", "opencode", "devin", "renamed-claude"] {
            okbot_testkit::fake_cli(
                &bin,
                name,
                r#"
if [ "$1" = auth ]; then
  if [ "${OKBOT_TEST_REQUIRE_HOME:-}" = yes ] && [ "$HOME" != "$OKBOT_TEST_AGENT_HOME" ]; then
    echo '{"loggedIn":false}'; exit 1
  fi
  echo '{"loggedIn":true}'; exit 0
fi
if [ "$1" = login ]; then exit 0; fi
printf '%s\n' "$@" > "$AGENT_TEST_ARGS"
printf '%s' "${BOT_TEST_SECRET:-absent}" > "$AGENT_TEST_ENV"
cat >/dev/null
if [ "${AGENT_TEST_FAIL:-}" = yes ]; then echo 'bot123456:secret-test' >&2; exit 7; fi
if [ "$1" = exec ]; then
  previous=
  for arg in "$@"; do
    if [ "$previous" = -o ]; then printf OK > "$arg"; fi
    previous="$arg"
  done
  echo '{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":1}}'
  exit 0
fi
echo '{"is_error":false,"result":"OK","usage":{"input_tokens":3,"output_tokens":1}}'
"#,
            );
        }
        okbot_testkit::fake_cli(
            &bin,
            "systemctl",
            r#"
printf '%s\n' "$*" >> "$OKBOT_TEST_SYSTEM_LOG"
for arg in "$@"; do
  if [ "$arg" = restart ] && [ "${OKBOT_TEST_RESTART_FAIL:-}" = yes ]; then echo 'test restart failure' >&2; exit 1; fi
done
exit 0
"#,
        );
        let bot = root.path().join("shop");
        std::fs::create_dir(&bot).unwrap();
        std::fs::write(bot.join("okbot.toml"),
            "# keep this configuration\nconfig_version = 1\n[bot]\nname = \"shop\" # name\ntimezone = \"Asia/Tokyo\"\n[agent]\nchain = [\"claude\"] # chain\n[agent.providers.claude]\nmodel = \"sonnet@high\" # model\n").unwrap();
        std::fs::write(bot.join(".env"), "BOT_TEST_SECRET=bot123456:secret-test\n").unwrap();
        Self { root }
    }
    fn bot(&self) -> std::path::PathBuf {
        self.root.path().join("shop")
    }
    fn command(&self) -> Command {
        let mut c = Command::cargo_bin("okbot").unwrap();
        c.current_dir(self.bot())
            .env("HOME", self.root.path())
            .env("OKBOT_HOME", self.root.path().join("registry"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.path().join("bin").display()),
            )
            .env("AGENT_TEST_ARGS", self.root.path().join("args"))
            .env("AGENT_TEST_ENV", self.root.path().join("child-env"))
            .env("OKBOT_TEST_SYSTEM_LOG", self.root.path().join("system-log"))
            .env("OKBOT_SYSTEMCTL", self.root.path().join("bin/systemctl"))
            .env("OKBOT_SYSTEMD_DIR", self.root.path().join("units"))
            .env("BOT_TEST_SECRET", "bot123456:secret-test")
            .timeout(std::time::Duration::from_secs(30));
        c
    }
    fn json(&self, args: &[&str], code: i32) -> Value {
        let out = self
            .command()
            .args(args)
            .arg("--json")
            .write_stdin("")
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn config(&self) -> okbot_config::Config {
        okbot_config::Config::load(&self.bot()).unwrap()
    }
    fn text(&self) -> String {
        std::fs::read_to_string(self.bot().join("okbot.toml")).unwrap()
    }
}

#[test]
fn agent_settings_keep_fallbacks_comments_and_inactive_overrides() {
    let env = Env::new();
    let before = env.text();
    let preview = env.json(&["agent", "use", "codex", "--print"], 0);
    assert_eq!(preview["saved"], false);
    assert_eq!(env.text(), before);
    let result = env.json(&["agent", "use", "codex"], 0);
    assert_eq!(result["pending_restart"], true);
    assert!(!env.root.path().join("system-log").exists());
    assert_eq!(env.config().agent.chain, ["codex", "claude"]);
    assert!(env.text().contains("# chain") && env.text().contains("# model"));
    let invalid_before = env.text();
    env.json(&["agent", "set", "--effort", "high"], 3);
    assert_eq!(env.text(), invalid_before);
    env.json(
        &["agent", "set", "--model", "test-model", "--effort", "ultra"],
        0,
    );
    assert_eq!(
        env.config().agent.providers["codex"].model.as_deref(),
        Some("test-model@ultra")
    );
    env.json(&["agent", "set", "--model", "next-model"], 0);
    assert_eq!(
        env.config().agent.providers["codex"].model.as_deref(),
        Some("next-model@ultra")
    );
    let invalid_before = env.text();
    env.json(&["agent", "set", "claude", "--effort", "ultra"], 1);
    env.json(&["agent", "chain", "codex,codex"], 1);
    env.json(&["agent", "chain", "devin"], 1);
    assert_eq!(env.text(), invalid_before);
    env.json(&["agent", "chain", "claude"], 0);
    assert!(env.config().agent.providers.contains_key("codex"));
    env.json(&["agent", "use", "codex"], 0);
    assert_eq!(
        env.config().agent.providers["codex"].model.as_deref(),
        Some("next-model@ultra")
    );
    env.json(&["agent", "set", "--default-effort"], 0);
    assert_eq!(
        env.config().agent.providers["codex"].model.as_deref(),
        Some("next-model")
    );
    env.json(&["agent", "set", "--default-model", "--default-cli"], 0);
    assert_eq!(env.config().agent.providers["codex"].model, None);
    assert_eq!(env.config().agent.providers["codex"].cli, None);
    let no_op = env.text();
    assert_eq!(
        env.json(&["agent", "use", "codex", "--default-cli"], 0)["changed"],
        false
    );
    assert_eq!(env.text(), no_op);
}

#[test]
fn discovery_and_status_use_exact_paths_and_separate_login_without_model_calls() {
    let env = Env::new();
    let binary = env.root.path().join("bin/renamed-claude");
    env.json(&["agent", "set", "--cli", binary.to_str().unwrap()], 0);
    let mut e = okbot_config::ConfigEditor::open(&env.bot()).unwrap();
    // The home is deliberately independent from the service's login.
    e.append("\n").unwrap();
    e.save().unwrap();
    let text = env
        .text()
        .replace("[agent]\n", "[agent]\nhome = \"agent-home\"\n");
    std::fs::write(env.bot().join("okbot.toml"), text).unwrap();
    let mut cmd = env.command();
    let out = cmd
        .args(["agent", "status", "--json"])
        .env("OKBOT_TEST_REQUIRE_HOME", "yes")
        // okbot resolves the bot directory (`/var` is `/private/var` on macOS).
        .env(
            "OKBOT_TEST_AGENT_HOME",
            env.bot().canonicalize().unwrap().join("agent-home"),
        )
        .write_stdin("")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let mut report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report["agents"][0]["path"],
        binary.canonicalize().unwrap().to_str().unwrap()
    );
    report["agents"][0]["path"] = json!("<CLI>");
    insta::assert_json_snapshot!("agent_status", report);
    assert!(
        !env.root.path().join("args").exists(),
        "no model calls during configuration/status"
    );
    assert_eq!(
        env.json(&["agent", "detect"], 0)["agents"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let out = env
        .command()
        .current_dir(env.root.path())
        .args(["agent", "detect", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["bot"],
        Value::Null
    );
    std::fs::remove_file(&binary).unwrap();
    assert_eq!(
        env.json(&["agent", "status"], 4)["agents"][0]["status"],
        "missing"
    );
}

#[test]
fn explicit_probe_is_isolated_and_restart_results_are_single_json_documents() {
    let env = Env::new();
    let probe = env.json(&["agent", "test"], 0);
    assert_eq!(probe["usage"]["input"], 3);
    let args = std::fs::read_to_string(env.root.path().join("args")).unwrap();
    assert!(args.contains("--model\nsonnet\n--effort\nhigh"), "{args}");
    assert!(!args.contains("WebSearch") && !args.contains("--resume"));
    assert!(args.contains("--no-session-persistence"));
    assert_eq!(
        std::fs::read_to_string(env.root.path().join("child-env")).unwrap(),
        "absent"
    );
    assert!(
        !env.bot().join("state").exists(),
        "probe does not touch bot history"
    );
    let out = env
        .command()
        .args(["agent", "test", "--json"])
        .env("AGENT_TEST_FAIL", "yes")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let output = String::from_utf8(out.stdout).unwrap();
    assert!(!output.contains("bot123456:secret-test"), "{output}");
    assert_eq!(serde_json::from_str::<Value>(&output).unwrap()["ok"], false);
    let saved = env.json(&["agent", "set", "--effort", "low", "--restart"], 0);
    assert_eq!(saved["restarted"], true);
    let out = env
        .command()
        .args(["agent", "set", "--effort", "high", "--restart", "--json"])
        .env("OKBOT_TEST_RESTART_FAIL", "yes")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["saved"], true);
    assert_eq!(result["pending_restart"], true);
    assert_eq!(result["error"]["code"], "restart_failed");
    assert_eq!(
        env.config().agent.providers["claude"].model.as_deref(),
        Some("sonnet@high")
    );
}

#[test]
fn codex_probe_uses_selected_model_effort_and_an_ephemeral_session() {
    let env = Env::new();
    env.json(
        &[
            "agent",
            "set",
            "codex",
            "--model",
            "test-model",
            "--effort",
            "high",
        ],
        0,
    );
    let result = env.json(&["agent", "test", "codex"], 0);
    assert_eq!(result["provider"], "codex");
    let args = std::fs::read_to_string(env.root.path().join("args")).unwrap();
    assert!(
        args.contains("-m\ntest-model\n-c\nmodel_reasoning_effort=\"high\""),
        "{args}"
    );
    assert!(args.contains("--ephemeral") && !args.contains("--resume"));
    assert_eq!(
        env.config().agent.chain,
        ["claude"],
        "testing does not switch providers"
    );
}

async fn seed(path: &Path) -> okbot_store::SqliteStore {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let store = okbot_store::SqliteStore::open(path).unwrap();
    let conversation = okbot_testkit::conversation("discord:main", "room|1");
    let at = "2026-01-01T00:00:00Z".parse().unwrap();
    store
        .append(okbot_testkit::user_message(
            &conversation,
            "HỢP ĐỒNG 日本語",
            at,
        ))
        .await
        .unwrap();
    store.reset(&conversation).await.unwrap();
    store
        .append(okbot_testkit::bot_message(&conversation, "new context", at))
        .await
        .unwrap();
    store
        .record_run(AgentRunRecord {
            run_id: "r1".into(),
            conversation,
            backend: "claude".into(),
            model: None,
            tools: ToolPolicy::McpOnly,
            usage: None,
            duration: std::time::Duration::from_millis(12),
            error: Some("cancelled bot123456:secret-test".into()),
            started_at: at,
            person: None,
        })
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn history_reads_live_wal_filters_unicode_and_keeps_old_context() {
    let env = Env::new();
    assert_eq!(env.json(&["history", "list"], 0)["items"], json!([]));
    assert!(!env.bot().join("state").exists());
    let _writer = seed(&env.bot().join("state/okbot.db")).await;
    let list = env.json(&["history", "list"], 0);
    assert_eq!(list["items"][0]["messages"], 2);
    let show = env.json(&["history", "show", "1"], 0);
    insta::assert_json_snapshot!("history_messages", show);
    let page = env.json(
        &["history", "show", "1", "--limit", "1", "--offset", "1"],
        0,
    );
    assert_eq!(page["items"][0]["before_reset"], true);
    assert_eq!(
        env.json(&["history", "search", "hợp đồng"], 0)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        env.json(&["history", "search", "日本語", "--conversation", "1"], 0)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        env.json(&["history", "list", "--channel", "zalo:main"], 0)["items"],
        json!([])
    );
    let runs = env.json(&["history", "runs", "--errors"], 0);
    assert_eq!(runs["items"][0]["model"], Value::Null);
    assert!(!runs.to_string().contains("bot123456:secret-test"));
    assert_eq!(
        env.json(&["history", "runs", "--until", "2026-01-01T00:00:00Z"], 0)["items"],
        json!([])
    );
    env.json(&["history", "show", "999"], 1);
    env.json(
        &[
            "history",
            "list",
            "--since",
            "2026-02-01T00:00:00Z",
            "--until",
            "2026-01-01T00:00:00Z",
        ],
        1,
    );
    let out = env
        .command()
        .args(["history", "show", "1"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("before latest /reset") && text.contains("2026-01-01 09:00:00 +09:00"));
    let path = env.bot();
    assert_eq!(
        env.json(&["history", "list", "--bot", path.to_str().unwrap()], 0)["items"][0]["id"],
        1
    );
}

#[test]
fn help_contracts_are_discoverable() {
    let env = Env::new();
    for (command, snapshot) in [("agent", "agent_help"), ("history", "history_help")] {
        let out = env.command().args([command, "--help"]).output().unwrap();
        assert!(out.status.success());
        insta::assert_snapshot!(snapshot, String::from_utf8(out.stdout).unwrap());
    }
}
