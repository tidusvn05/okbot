//! One agent CLI as an [`AgentBackend`].

use std::path::PathBuf;
use std::time::Instant;

use agent_core::{AgentCli, McpServer, Provider, RunRequest, ToolPolicy as CoreToolPolicy};
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
}

/// An agent CLI run through agent-core. Each turn starts a fresh process.
#[derive(Debug)]
pub struct CliBackend {
    cli: AgentCli,
    name: String,
    model: Option<String>,
    scrub: EnvScrub,
}

impl CliBackend {
    /// Finds the CLI and checks that it starts (`--version`).
    pub fn new(config: CliBackendConfig) -> Result<Self, AgentError> {
        let provider = config.provider;
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
        })
    }

    /// The CLI this backend runs.
    pub fn provider(&self) -> Provider {
        self.cli.provider()
    }
}

/// Maps agent-core errors to the port's error, deciding which ones justify trying another backend.
fn map_error(error: agent_core::Error) -> AgentError {
    use agent_core::Error as E;
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
    }
}

fn last_line(text: &str) -> &str {
    text.trim().lines().last().unwrap_or("")
}

impl CliBackend {
    /// Builds agent-core's request, using native features where this CLI enforces them.
    fn request(&self, turn: &AgentTurn) -> RunRequest {
        let native = self.caps();
        let base = RunRequest {
            cwd: turn.workdir.clone(),
            model: self.model.clone(),
            timeout: turn.timeout,
            env_remove: self.scrub.current(),
            ..RunRequest::default()
        };
        if !native.enforces(turn.tools) {
            // Only reached when the chain allows unenforced tools (development).
            return RunRequest {
                prompt: inline_system_prompt(&turn.system, &turn.prompt),
                ..base
            };
        }
        RunRequest {
            prompt: turn.prompt.clone(),
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
            env: turn
                .env
                .iter()
                .map(|(k, v)| (k.into(), v.expose().into()))
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
        // agent-core 0.4 enforces tool policies and per-run MCP servers for Claude and Codex
        // only; Devin and OpenCode reject them, so they get the system prompt inlined and
        // knowledge excerpts instead.
        match self.cli.provider() {
            Provider::Claude | Provider::Codex => AgentCaps {
                system_prompt: true,
                mcp: true,
                tool_policies: vec![ToolPolicy::Web, ToolPolicy::McpOnly],
                resume: false,
                stream: false,
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
        let result = self.cli.run(request).await.map_err(map_error)?;
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
