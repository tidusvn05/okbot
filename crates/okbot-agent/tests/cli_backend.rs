//! `CliBackend` against fake agent CLIs (no quota, no network) and, opt-in, the real ones.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::time::Duration;

use okbot_agent::{CliBackend, CliBackendConfig, EnvScrub, Provider};
use okbot_core::{AgentBackend, AgentError};
use okbot_testkit::{fake_cli, turn};

fn backend(dir: &std::path::Path, body: &str) -> CliBackend {
    let binary = fake_cli(dir, "claude", body);
    CliBackend::new(CliBackendConfig {
        provider: Provider::Claude,
        binary: Some(binary),
        model: None,
        scrub: EnvScrub::default(),
    })
    .unwrap()
}

#[tokio::test]
async fn runs_claude_and_inlines_the_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // Echo the prompt back inside Claude's JSON envelope.
    let cli = backend(
        dir.path(),
        r#"prompt="$(cat)"
printf '{"is_error":false,"result":%s,"session_id":"s1","usage":{"input_tokens":3,"output_tokens":4}}\n' "$(printf '%s' "$prompt" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')""#,
    );
    let mut t = turn("hello");
    t.system = "be brief".into();
    t.workdir = dir.path().join("work/conv");
    let reply = cli.run(t).await.unwrap();
    assert_eq!(
        reply.text,
        "<instructions>\nbe brief\n</instructions>\n\nhello"
    );
    assert_eq!(reply.backend, "claude");
    assert_eq!(reply.session_id.as_deref(), Some("s1"));
    assert_eq!(reply.usage.unwrap().output, 4);
    assert!(dir.path().join("work/conv").is_dir(), "workdir created");
}

#[tokio::test]
async fn secrets_are_removed_from_the_agent_environment() {
    let dir = tempfile::tempdir().unwrap();
    // `set_var` is unsafe in edition 2024 and unsafe code is forbidden, so the test checks that
    // the variable is on the scrub list and that the child does not see it.
    let cli = backend(
        dir.path(),
        r#"cat >/dev/null
printf '{"is_error":false,"result":"%s"}\n' "${ZALO_BOT_TOKEN:-absent}""#,
    );
    let mut t = turn("x");
    t.workdir = dir.path().to_path_buf();
    // The scrub list always names known secret prefixes, whatever the parent has.
    assert!(
        EnvScrub::default()
            .names_to_remove(["ZALO_BOT_TOKEN".into()])
            .contains(&"ZALO_BOT_TOKEN".into())
    );
    assert_eq!(cli.run(t).await.unwrap().text, "absent");
}

#[tokio::test]
async fn maps_failures() {
    let dir = tempfile::tempdir().unwrap();
    let cli = backend(dir.path(), "cat >/dev/null; echo boom >&2; exit 7");
    let mut t = turn("x");
    t.workdir = dir.path().to_path_buf();
    match cli.run(t).await {
        Err(AgentError::Failed { message, retryable }) => {
            assert!(retryable);
            assert!(message.contains("boom"), "{message}");
        }
        other => panic!("unexpected: {other:?}"),
    }

    let slow = backend(dir.path(), "sleep 5");
    let mut t = turn("x");
    t.workdir = dir.path().to_path_buf();
    t.timeout = Duration::from_millis(300);
    assert!(matches!(slow.run(t).await, Err(AgentError::Timeout(_))));
}

#[test]
fn missing_cli_is_unavailable() {
    let err = CliBackend::new(CliBackendConfig {
        provider: Provider::Codex,
        binary: Some("/nonexistent/codex".into()),
        model: None,
        scrub: EnvScrub::default(),
    })
    .unwrap_err();
    assert!(matches!(err, AgentError::Unavailable(_)), "{err}");
}

/// Real CLI smoke test: `OKBOT_SMOKE_PROVIDER=claude cargo test -p okbot-agent --test cli_backend -- --ignored`
#[tokio::test]
#[ignore = "uses a real, logged-in agent CLI and its quota"]
async fn real_cli_smoke() {
    let name = std::env::var("OKBOT_SMOKE_PROVIDER").unwrap_or_else(|_| "claude".into());
    let provider = okbot_agent::parse_provider(&name).expect("known provider");
    let cli = CliBackend::new(CliBackendConfig {
        provider,
        binary: None,
        model: std::env::var("OKBOT_SMOKE_MODEL").ok(),
        scrub: EnvScrub::default(),
    })
    .expect("CLI installed");
    let dir = tempfile::tempdir().unwrap();
    let mut t = turn("Reply with exactly the word: pong");
    t.workdir = dir.path().to_path_buf();
    t.timeout = Duration::from_secs(180);
    let reply = cli.run(t).await.expect("answer");
    assert!(reply.text.to_lowercase().contains("pong"), "{}", reply.text);
}
