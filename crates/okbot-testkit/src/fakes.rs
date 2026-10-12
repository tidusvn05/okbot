//! Fake adapters.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use okbot_core::{
    AgentBackend, AgentCaps, AgentError, AgentReply, AgentTurn, ToolPolicy, async_trait,
};

/// What a [`FakeAgent`] does.
#[derive(Debug, Clone)]
enum Behaviour {
    Echo,
    Fail { retryable: bool },
    Reply(String),
    Sleep(Duration),
}

/// An [`AgentBackend`] for tests: echoes, replies, fails or sleeps, and records its turns.
#[derive(Debug)]
pub struct FakeAgent {
    name: String,
    behaviour: Behaviour,
    caps: AgentCaps,
    calls: AtomicUsize,
    turns: Mutex<Vec<AgentTurn>>,
}

impl FakeAgent {
    fn with(name: &str, behaviour: Behaviour) -> Self {
        Self {
            name: name.into(),
            behaviour,
            caps: AgentCaps::default(),
            calls: AtomicUsize::new(0),
            turns: Mutex::new(Vec::new()),
        }
    }

    /// Answers `echo: <prompt>`.
    pub fn echo(name: &str) -> Self {
        Self::with(name, Behaviour::Echo)
    }

    /// Always answers `text`.
    pub fn replying(name: &str, text: &str) -> Self {
        Self::with(name, Behaviour::Reply(text.into()))
    }

    /// Always fails.
    pub fn failing(name: &str, retryable: bool) -> Self {
        Self::with(name, Behaviour::Fail { retryable })
    }

    /// Sleeps for `d`, then echoes.
    pub fn sleeping(name: &str, d: Duration) -> Self {
        Self::with(name, Behaviour::Sleep(d))
    }

    /// Declares the tool policies it enforces.
    pub fn enforcing(mut self, policies: &[ToolPolicy]) -> Self {
        self.caps.tool_policies = policies.to_vec();
        self
    }

    /// Declares native capabilities.
    pub fn with_caps(mut self, caps: AgentCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Number of turns started.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The turns received, oldest first.
    pub fn turns(&self) -> Vec<AgentTurn> {
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl AgentBackend for FakeAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn caps(&self) -> AgentCaps {
        self.caps.clone()
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(turn.clone());
        let text = match &self.behaviour {
            Behaviour::Echo => format!("echo: {}", turn.prompt),
            Behaviour::Reply(text) => text.clone(),
            Behaviour::Fail { retryable } => {
                return Err(AgentError::Failed {
                    message: format!("{} failed", self.name),
                    retryable: *retryable,
                });
            }
            Behaviour::Sleep(d) => {
                tokio::time::sleep(*d).await;
                format!("echo: {}", turn.prompt)
            }
        };
        Ok(AgentReply {
            text,
            backend: self.name.clone(),
            model: None,
            session_id: None,
            usage: None,
            duration: Duration::ZERO,
        })
    }
}

/// A minimal turn with `prompt`, the default tool policy and a temp working directory.
pub fn turn(prompt: &str) -> AgentTurn {
    AgentTurn {
        system: String::new(),
        prompt: prompt.into(),
        tools: ToolPolicy::default(),
        mcp: Vec::new(),
        env: Vec::new(),
        workdir: std::env::temp_dir().join("okbot-testkit-work"),
        timeout: Duration::from_secs(10),
        resume: None,
    }
}

/// Writes an executable shell script `dir/name` that prints `1.0.0` for `--version` and runs
/// `body` otherwise (Unix only). Mirrors agent-core's test helper.
#[cfg(unix)]
pub fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0.0; exit 0; fi\n{body}\n"),
    )
    .expect("write fake CLI");
    let mut permissions = std::fs::metadata(&path).expect("stat").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod");
    // Some filesystems briefly report ETXTBSY right after creating an executable.
    std::thread::sleep(Duration::from_millis(10));
    path
}
