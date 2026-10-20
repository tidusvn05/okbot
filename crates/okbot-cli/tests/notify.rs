//! `[[notify]]` end to end: `okbot run` with the Node reference bridge adapter, then
//! `okbot send` posts through the running bot to the configured room.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::fmt::Write as _;
use std::io::BufRead as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Reads log lines into `log` until one contains `needle` or a minute passes.
fn wait_for(rx: &Receiver<String>, log: &mut String, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !log.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                log.push_str(&line);
                log.push('\n');
            }
            Err(_) => return false,
        }
    }
    true
}

/// A bot directory with a fake `claude`, the echo bridge adapter and one `[[notify]]` target.
struct Fixture {
    dir: tempfile::TempDir,
    bot: PathBuf,
    path: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        okbot_testkit::fake_cli(
            &bin,
            "claude",
            r#"if [ "$1" = auth ]; then echo '{"loggedIn": true}'; exit 0; fi
cat >/dev/null
printf '%s\n' '{"is_error":false,"result":"ok"}'"#,
        );
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let bot = dir.path().join("bot");
        std::fs::create_dir_all(&bot).unwrap();
        let me = Self { dir, bot, path };
        let init = me.okbot(&[
            "init",
            "--name",
            "b",
            "--agent",
            "claude",
            "--platform",
            "none",
            "--yes",
        ]);
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let adapter = format!(
            "{}/../../examples/bridge/echo-channel.mjs",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut config = std::fs::read_to_string(me.bot.join("okbot.toml")).unwrap();
        writeln!(
            config,
            "\n[http]\nlisten = \"127.0.0.1:{port}\"\n\
             [channels.bridge.main]\nplatform = \"demo\"\ncommand = \"node\"\nargs = [{adapter:?}]\n\
             [[notify]]\nname = \"alerts\"\ntoken_env = \"NOTIFY_TOKEN\"\nto = [\"room:demo:main|-|ops-room|-\"]"
        )
        .unwrap();
        std::fs::write(me.bot.join("okbot.toml"), config).unwrap();
        me
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_okbot"));
        command
            .current_dir(&self.bot)
            .env("PATH", &self.path)
            .env("HOME", self.dir.path())
            .env("NOTIFY_TOKEN", "n0tify-s3cret")
            .args(args);
        command
    }

    fn okbot(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
}

#[test]
fn okbot_send_reaches_the_configured_room() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let b = Fixture::new();
    let mut run = b
        .command(&["run"])
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
    assert!(wait_for(&rx, &mut log, "HTTP server listening"), "{log}");

    let sent = b.okbot(&["send", "alerts", "**disk** 91%", "--id", "evt-1"]);
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert!(String::from_utf8_lossy(&sent.stderr).contains("Sent to 1 chat"));
    assert!(
        wait_for(&rx, &mut log, "send to ops-room: **disk** 91%"),
        "{log}"
    );

    let again = b.okbot(&["send", "alerts", "**disk** 91%", "--id", "evt-1", "--json"]);
    assert!(again.status.success());
    let answer: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(answer["duplicate"], true);

    let unknown = b.okbot(&["send", "nope", "x"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("there is alerts"));

    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(run.id().to_string())
        .status();
    run.wait().unwrap();
    log.extend(rx.try_iter().map(|l| l + "\n"));
    assert_eq!(
        log.matches("send to ops-room").count(),
        1,
        "the retried id is sent once: {log}"
    );
    assert!(
        !log.contains("n0tify-s3cret"),
        "the token is never logged: {log}"
    );
}
