//! Local `[[mcp]]` servers that speak MCP over stdio (ADR 0011).
//!
//! The gateway starts one process per agent session (on `initialize`) and ends it with the session
//! or the turn. A process gets an empty environment (only `PATH`, `HOME` set to its own empty
//! directory, and the variables the server names), so okbot's secrets never reach it.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::config::StdioServer;
use crate::mcp_client::{MAX_BODY, McpError};

/// How long a process may take to exit after its input closes.
const EXIT_GRACE: Duration = Duration::from_secs(2);

struct Io {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// One running server process.
pub(crate) struct Process {
    io: tokio::sync::Mutex<Option<Io>>,
    child: std::sync::Mutex<Option<Child>>,
    dir: PathBuf,
}

impl Process {
    /// Starts `server` in a new empty directory under `parent`.
    pub(crate) fn spawn(server: &StdioServer, parent: &std::path::Path) -> std::io::Result<Self> {
        let dir = parent.join(hex::encode(rand::random::<[u8; 8]>()));
        std::fs::create_dir_all(&dir)?;
        let (program, args) = server
            .command
            .split_first()
            .ok_or_else(|| std::io::Error::other("empty command"))?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("HOME", &dir)
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Its diagnostics may hold anything; they are not kept.
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for (name, value) in &server.env {
            command.env(name, value.expose());
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(std::io::Error::other("no pipes to the server"));
        };
        Ok(Self {
            io: tokio::sync::Mutex::new(Some(Io {
                stdin,
                stdout: BufReader::new(stdout),
            })),
            child: std::sync::Mutex::new(Some(child)),
            dir,
        })
    }

    /// Sends one message. For a request, waits up to `limit` for its response, answering the
    /// server's own requests (sampling, roots, elicitation) with "not supported" meanwhile.
    pub(crate) async fn exchange(
        &self,
        message: &Value,
        limit: Duration,
    ) -> Result<Option<Value>, McpError> {
        let mut guard = self.io.lock().await;
        let io = guard
            .as_mut()
            .ok_or_else(|| McpError::Protocol("the server has stopped".into()))?;
        write(&mut io.stdin, message).await?;
        let Some(id) = message
            .get("id")
            .filter(|_| message.get("method").is_some())
        else {
            return Ok(None);
        };
        tokio::time::timeout(limit, async {
            let mut line = String::new();
            loop {
                line.clear();
                let read = (&mut io.stdout)
                    .take(u64::try_from(MAX_BODY).unwrap_or(u64::MAX))
                    .read_line(&mut line)
                    .await
                    .map_err(|e| McpError::Protocol(e.to_string()))?;
                if read == 0 {
                    return Err(McpError::Protocol("the server exited".into()));
                }
                if !line.ends_with('\n') && read >= MAX_BODY {
                    return Err(McpError::Protocol("reply too large".into()));
                }
                let Ok(reply) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                if reply.get("method").is_some() {
                    if let Some(theirs) = reply.get("id") {
                        let refusal = json!({"jsonrpc": "2.0", "id": theirs,
                            "error": {"code": -32601, "message": "not supported by okbot"}});
                        write(&mut io.stdin, &refusal).await?;
                    }
                    continue;
                }
                if reply.get("id") == Some(id) {
                    return Ok(Some(reply));
                }
            }
        })
        .await
        .map_err(|_| McpError::Timeout)?
    }
}

async fn write(stdin: &mut ChildStdin, message: &Value) -> Result<(), McpError> {
    let mut line = message.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| McpError::Protocol(format!("cannot write to the server: {e}")))?;
    stdin
        .flush()
        .await
        .map_err(|e| McpError::Protocol(format!("cannot write to the server: {e}")))
}

impl Drop for Process {
    /// Closes the server's input (servers exit on end of input, children included), then kills
    /// it if it is still running after [`EXIT_GRACE`], and removes its directory.
    fn drop(&mut self) {
        let io = self.io.get_mut().take();
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let dir = std::mem::take(&mut self.dir);
        let fallback = dir.clone();
        let cleanup = async move {
            drop(io);
            if let Some(mut child) = child
                && tokio::time::timeout(EXIT_GRACE, child.wait())
                    .await
                    .is_err()
            {
                let _ = child.kill().await;
            }
            let _ = tokio::fs::remove_dir_all(&dir).await;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(cleanup);
        } else {
            // Outside a runtime: dropping the child kills it.
            drop(cleanup);
            let _ = std::fs::remove_dir_all(&fallback);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn server(script: &str) -> StdioServer {
        StdioServer {
            command: vec!["sh".into(), "-c".into(), script.into()],
            env: vec![("GREETING".into(), okbot_core::Secret::new("hi".into()))],
        }
    }

    #[tokio::test]
    async fn requests_get_their_response_and_server_requests_are_refused() {
        let parent = tempfile::tempdir().unwrap();
        // Asks okbot something first, then answers with its environment.
        let script = r#"read line; printf '%s\n' '{"jsonrpc":"2.0","id":"s1","method":"roots/list"}'; read refusal; printf '{"jsonrpc":"2.0","method":"notifications/progress"}\n{"jsonrpc":"2.0","id":7,"result":{"greeting":"%s","home":"%s","leak":"%s","refusal":%s}}\n' "$GREETING" "$HOME" "$CARGO_PKG_NAME" "$refusal"; read rest"#;
        let process = Process::spawn(&server(script), parent.path()).unwrap();
        let reply = process
            .exchange(
                &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
                Duration::from_secs(10),
            )
            .await
            .unwrap()
            .unwrap();
        let result = &reply["result"];
        assert_eq!(result["greeting"], "hi");
        assert_eq!(result["leak"], "", "okbot's environment is not passed");
        assert!(
            result["home"]
                .as_str()
                .unwrap()
                .starts_with(parent.path().to_str().unwrap())
        );
        assert_eq!(result["refusal"]["id"], "s1");
        assert_eq!(result["refusal"]["error"]["code"], -32601);
        // A notification needs no answer.
        let none = process
            .exchange(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert!(none.is_none());
        let dir = process.dir.clone();
        drop(process);
        for _ in 0..100 {
            if !dir.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!dir.exists(), "the directory goes with the process");
    }

    #[tokio::test]
    async fn exits_and_silence_are_errors() {
        let parent = tempfile::tempdir().unwrap();
        let gone = Process::spawn(&server("exit 0"), parent.path()).unwrap();
        let error = gone
            .exchange(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
                Duration::from_secs(10),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::Protocol(_)), "{error}");
        let silent = Process::spawn(&server("read line; sleep 30"), parent.path()).unwrap();
        let error = silent
            .exchange(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
                Duration::from_millis(200),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::Timeout), "{error}");
        assert!(
            Process::spawn(
                &StdioServer {
                    command: vec!["/nonexistent/okbot-test".into()],
                    env: Vec::new(),
                },
                parent.path()
            )
            .is_err()
        );
    }
}
