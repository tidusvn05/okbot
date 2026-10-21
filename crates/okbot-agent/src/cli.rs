//! One agent CLI as an [`AgentBackend`].

use std::path::PathBuf;
use std::time::Instant;

use agent_call::{AgentCli, McpServer, Provider, RunRequest, ToolPolicy as CoreToolPolicy};
use okbot_core::{
    AgentBackend, AgentCaps, AgentError, AgentReply, AgentTurn, TokenUsage, ToolPolicy, async_trait,
};

use crate::env::EnvScrub;
use crate::prompt::inline_system_prompt;
use crate::provider_name;

/// How to run one agent CLI.
#[derive(Debug, Clone)]
pub struct CliBackendConfig {
    /// Which CLI.
    pub provider: Provider,
    /// Executable path; `None` looks it up on `PATH`.
    pub binary: Option<PathBuf>,
    /// Model id in the CLI's format (`sonnet@high`); `None` uses the CLI's default.
    pub model: Option<String>,
    /// Environment variables kept away from the agent.
    pub scrub: EnvScrub,
    /// Extra environment for the CLI (for example a separate `HOME` with its own login).
    pub env: Vec<(String, String)>,
}

/// An agent CLI run through agent-call. Each turn starts a fresh process.
#[derive(Debug)]
pub struct CliBackend {
    cli: AgentCli,
    name: String,
    model: Option<String>,
    scrub: EnvScrub,
    env: Vec<(String, String)>,
    sessions: bool,
}

impl CliBackend {
    /// Finds the CLI and checks that it starts (`--version`).
    pub fn new(config: CliBackendConfig) -> Result<Self, AgentError> {
        let provider = config.provider;
        if let Some(model) = &config.model {
            crate::validate_model(provider, model)?;
        }
        let cli = match &config.binary {
            Some(path) => AgentCli::with_binary(provider, path.clone()),
            None => AgentCli::discover(provider),
        }
        .map_err(|e| AgentError::Unavailable(format!("{}: {e}", provider_name(provider))))?;
        Ok(Self {
            cli,
            name: provider_name(provider).to_string(),
            model: config.model,
            scrub: config.scrub,
            env: config.env,
            sessions: true,
        })
    }

    /// The CLI this backend runs.
    pub fn provider(&self) -> Provider {
        self.cli.provider()
    }

    /// Disables native session persistence and resume, for isolated operator probes.
    #[must_use]
    pub fn without_sessions(mut self) -> Self {
        self.sessions = false;
        self
    }
}

/// Maps agent-call errors to the port's error, deciding which ones justify trying another backend.
fn map_error(error: agent_call::Error) -> AgentError {
    use agent_call::Error as E;
    match error {
        E::NotFound(path) => AgentError::Unavailable(format!("not found: {}", path.display())),
        E::VersionProbe(msg) | E::UnsupportedVersion(msg) => AgentError::Unavailable(msg),
        E::InvalidRequest(msg) => AgentError::Failed {
            message: format!("invalid request: {msg}"),
            retryable: false,
        },
        E::Timeout(d) => AgentError::Timeout(d),
        E::Exit {
            status,
            stderr_tail,
        } => AgentError::Failed {
            message: format!("exited with {status:?}: {}", last_line(&stderr_tail)),
            retryable: true,
        },
        E::Io(e) => AgentError::Failed {
            message: format!("I/O: {e}"),
            retryable: true,
        },
        E::Agent(msg) => AgentError::Failed {
            message: format!("agent error: {msg}"),
            retryable: true,
        },
        E::Protocol(msg) => AgentError::Failed {
            message: format!("unexpected output: {msg}"),
            retryable: true,
        },
        E::EmptyResponse => AgentError::Failed {
            message: "empty answer".into(),
            retryable: true,
        },
        E::SessionNotFound(id) => AgentError::Failed {
            message: format!("session {id} not found"),
            retryable: true,
        },
        // agent-call may add variants; treat them as retryable failures.
        #[allow(unreachable_patterns)]
        other => AgentError::Failed {
            message: other.to_string(),
            retryable: true,
        },
    }
}

/// Forwards agent-call events as okbot progress.
fn events(progress: Option<&okbot_core::ProgressFn>) -> Option<agent_call::EventHandler> {
    let progress = progress?.clone();
    Some(agent_call::EventHandler::new(move |event| {
        let mapped = match event {
            agent_call::AgentEvent::TextDelta(t) => okbot_core::AgentProgress::TextDelta(t),
            agent_call::AgentEvent::Text(t) => okbot_core::AgentProgress::Text(t),
            agent_call::AgentEvent::ToolUse { name } => okbot_core::AgentProgress::ToolUse(name),
            _ => return,
        };
        progress.report(mapped);
    }))
}

fn last_line(text: &str) -> &str {
    text.trim().lines().last().unwrap_or("")
}

impl CliBackend {
    /// Builds agent-call's request, using native features where this CLI enforces them.
    fn request(&self, turn: &AgentTurn) -> RunRequest {
        let native = self.caps();
        let base = RunRequest {
            cwd: turn.workdir.clone(),
            model: self.model.clone(),
            timeout: turn.timeout,
            env_remove: self.scrub.current(),
            env: self.env.iter().map(|(k, v)| (k.into(), v.into())).collect(),
            on_event: events(turn.progress.as_ref()),
            ..RunRequest::default()
        };
        if !native.enforces(turn.tools) {
            // Only reached when the chain allows unenforced tools (development).
            return RunRequest {
                prompt: inline_system_prompt(&turn.system, &turn.prompt),
                ..base
            };
        }
        // Resume this backend's own session; otherwise start one (kept for the next turn).
        let resume = turn
            .resume
            .as_ref()
            .filter(|r| self.sessions && r.backend == self.name);
        RunRequest {
            prompt: resume.map_or_else(|| turn.prompt.clone(), |r| r.prompt.clone()),
            resume: resume.map(|r| r.session.clone()),
            persist_session: native.resume && self.sessions,
            system_prompt: Some(turn.system.clone()).filter(|s| !s.trim().is_empty()),
            tool_policy: match turn.tools {
                ToolPolicy::McpOnly => CoreToolPolicy::McpOnly,
                _ => CoreToolPolicy::Web,
            },
            mcp_servers: turn
                .mcp
                .iter()
                .map(|s| McpServer {
                    name: s.name.clone(),
                    url: s.url.clone(),
                    bearer_token_env: Some(s.bearer_token_env.clone()),
                })
                .collect(),
            env: base
                .env
                .iter()
                .cloned()
                .chain(turn.env.iter().map(|(k, v)| (k.into(), v.expose().into())))
                .collect(),
            ..base
        }
    }
}

#[async_trait]
impl AgentBackend for CliBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn caps(&self) -> AgentCaps {
        // agent-call 0.4 enforces tool policies and per-run MCP servers for Claude and Codex
        // only; Devin and OpenCode reject them, so they get the system prompt inlined and
        // knowledge excerpts instead.
        match self.cli.provider() {
            Provider::Claude | Provider::Codex => AgentCaps {
                system_prompt: true,
                mcp: true,
                tool_policies: vec![ToolPolicy::Web, ToolPolicy::McpOnly],
                resume: true,
                stream: true,
            },
            _ => AgentCaps::default(),
        }
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        std::fs::create_dir_all(&turn.workdir).map_err(|e| AgentError::Failed {
            message: format!("cannot create {}: {e}", turn.workdir.display()),
            retryable: false,
        })?;
        let request = self.request(&turn);
        let started = Instant::now();
        let result = match self.cli.run(request).await {
            // The session is gone (expired, CLI data cleared): start over with the full history.
            Err(agent_call::Error::SessionNotFound(id)) if self.sessions => {
                tracing::info!(backend = %self.name, session = %id, "session not found; starting a new one");
                let fresh = AgentTurn {
                    resume: None,
                    ..turn.clone()
                };
                self.cli.run(self.request(&fresh)).await
            }
            other => other,
        }
        .map_err(map_error)?;
        Ok(AgentReply {
            text: result.text,
            backend: self.name.clone(),
            model: self.model.clone(),
            session_id: result.session_id,
            usage: result.usage.map(|u| TokenUsage {
                input: u.input,
                output: u.output,
                reasoning: u.reasoning,
                cache_read: u.cache_read,
                cache_write: u.cache_write,
            }),
            duration: started.elapsed(),
        })
    }
}
