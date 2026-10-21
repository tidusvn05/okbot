//! `okbot run` end to end through a bridge adapter (the Node reference adapter) and a fake
//! `claude`.
#![cfg(unix)]
#![cfg(feature = "bridge")]
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::fmt::Write as _;
use std::io::BufRead as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn answers_through_a_bridge_adapter() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    okbot_testkit::fake_cli(
        &bin,
        "claude",
        r#"if [ "$1" = auth ]; then echo '{"loggedIn": true}'; exit 0; fi
cat >/dev/null
printf '%s\n' '{"is_error":false,"result":"Hello over the bridge"}'"#,
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let bot = dir.path().join("bot");
    std::fs::create_dir_all(&bot).unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_okbot"))
        .current_dir(&bot)
        .env("PATH", &path)
        .env("HOME", dir.path())
        .args([
            "init",
            "--name",
            "b",
            "--agent",
            "claude",
            "--platform",
            "none",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let adapter = format!(
        "{}/../../examples/bridge/echo-channel.mjs",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut config = std::fs::read_to_string(bot.join("okbot.toml")).unwrap();
    writeln!(
        config,
        "\n[channels.bridge.main]\nplatform = \"demo\"\ncommand = \"node\"\nargs = [{adapter:?}]\n\
         env = [\"DEMO_TOKEN\"]\n[channels.bridge.main.settings]\ngreeting = \"xin chào\""
    )
    .unwrap();
    std::fs::write(bot.join("okbot.toml"), config).unwrap();

    let mut run = Command::new(env!("CARGO_BIN_EXE_okbot"))
        .current_dir(&bot)
        .env("PATH", &path)
        .env("HOME", dir.path())
        .env("DEMO_TOKEN", "t0ps3cret")
        .args(["run"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let stderr = std::io::BufReader::new(run.stderr.take().unwrap());
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut log = String::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !log.contains("send to demo-room: Hello over the bridge") {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                log.push_str(&line);
                log.push('\n');
            }
            Err(_) => break,
        }
    }
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(run.id().to_string())
        .status();
    let status = run.wait().unwrap();
    log.extend(rx.try_iter().map(|l| l + "\n"));
    assert!(
        log.contains("send to demo-room: Hello over the bridge"),
        "{log}"
    );
    assert!(!log.contains("t0ps3cret"), "secrets are redacted: {log}");
    assert!(status.code().is_some(), "okbot exited on SIGTERM: {status}");
}
