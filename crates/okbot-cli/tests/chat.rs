//! `okbot chat` end to end with a fake `claude` on PATH.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use assert_cmd::Command;

#[test]
fn chats_through_the_console() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    okbot_testkit::fake_cli(
        &bin,
        "claude",
        r#"if [ "$1" = auth ]; then echo '{"loggedIn": true}'; exit 0; fi
cat >/dev/null
printf '%s\n' '{"is_error":false,"result":"Hello from **fake** claude"}'"#,
    );
    let output = Command::cargo_bin("okbot")
        .unwrap()
        .current_dir(dir.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", dir.path())
        .args(["chat"])
        .write_stdin("hello\n/whoami\n")
        .timeout(std::time::Duration::from_secs(60))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Hello from **fake** claude"), "{stdout}");
    assert!(stdout.contains("`you`"), "{stdout}");
    assert!(dir.path().join("state/okbot.db").exists());
}

#[test]
fn refuses_unenforced_tools_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    okbot_testkit::fake_cli(&bin, "devin", "exit 0");
    let output = Command::cargo_bin("okbot")
        .unwrap()
        .current_dir(dir.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .args(["chat", "--agent", "devin"])
        .write_stdin("")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--unsafe-tools"));
}
