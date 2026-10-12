//! Ordered fallback across backends, with tool-policy enforcement and a concurrency limit.

use std::sync::Arc;

use okbot_core::{AgentBackend, AgentCaps, AgentError, AgentReply, AgentTurn, async_trait};
use tokio::sync::Semaphore;

/// Tries backends in order until one answers.
///
/// - A backend that cannot enforce the turn's tool policy is skipped, unless the chain was built
///   with [`AgentChain::allow_unenforced_tools`] (development only: the agent may then use every
///   tool its CLI has, including a shell).
/// - Errors that [`AgentError::should_fall_back`] move on to the next backend; others stop.
/// - At most `max_concurrent` turns run at once across the whole chain.
pub struct AgentChain {
    backends: Vec<Arc<dyn AgentBackend>>,
    name: String,
    permits: Arc<Semaphore>,
    allow_unenforced: bool,
}

impl std::fmt::Debug for AgentChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentChain")
            .field("name", &self.name)
            .field("allow_unenforced", &self.allow_unenforced)
            .finish_non_exhaustive()
    }
}

impl AgentChain {
    /// A chain over `backends` (tried in order) running at most `max_concurrent` turns at once.
    pub fn new(backends: Vec<Arc<dyn AgentBackend>>, max_concurrent: usize) -> Self {
        let names: Vec<&str> = backends.iter().map(|b| b.name()).collect();
        Self {
            name: names.join(","),
            backends,
            permits: Arc::new(Semaphore::new(max_concurrent.max(1))),
            allow_unenforced: false,
        }
    }

    /// Lets backends run turns whose tool policy they cannot enforce. For local development with
    /// a trusted user only; never for a bot reachable by others.
    pub fn allow_unenforced_tools(mut self, allow: bool) -> Self {
        if allow {
            tracing::warn!(
                chain = %self.name,
                "tool policies are NOT enforced: the agent may run shell commands (development only)"
            );
        }
        self.allow_unenforced = allow;
        self
    }

    /// The backends, in order.
    pub fn backends(&self) -> &[Arc<dyn AgentBackend>] {
        &self.backends
    }
}

#[async_trait]
impl AgentBackend for AgentChain {
    fn name(&self) -> &str {
        &self.name
    }

    /// Native features shared by every backend; any backend's enforceable tool policies.
    fn caps(&self) -> AgentCaps {
        let all: Vec<AgentCaps> = self.backends.iter().map(|b| b.caps()).collect();
        let every = |f: fn(&AgentCaps) -> bool| !all.is_empty() && all.iter().all(f);
        let mut tool_policies = Vec::new();
        for caps in &all {
            for policy in &caps.tool_policies {
                if !tool_policies.contains(policy) {
                    tool_policies.push(*policy);
                }
            }
        }
        AgentCaps {
            system_prompt: every(|c| c.system_prompt),
            mcp: every(|c| c.mcp),
            tool_policies,
            resume: every(|c| c.resume),
            stream: every(|c| c.stream),
        }
    }

    async fn run(&self, turn: AgentTurn) -> Result<AgentReply, AgentError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| AgentError::Unavailable("agent chain is shutting down".into()))?;
        let mut last_error = AgentError::Unavailable("no agent backend configured".into());
        for backend in &self.backends {
            if !self.allow_unenforced && !backend.caps().enforces(turn.tools) {
                tracing::debug!(backend = backend.name(), policy = ?turn.tools, "skipped: cannot enforce tool policy");
                last_error = AgentError::Unsupported(format!(
                    "{} cannot enforce tool policy {:?}",
                    backend.name(),
                    turn.tools
                ));
                continue;
            }
            match backend.run(turn.clone()).await {
                Ok(reply) => return Ok(reply),
                Err(error) if error.should_fall_back() => {
                    tracing::warn!(backend = backend.name(), %error, "agent failed, trying the next one");
                    last_error = error;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::ToolPolicy;
    use okbot_testkit::{FakeAgent, turn};

    fn chain(backends: Vec<Arc<dyn AgentBackend>>) -> AgentChain {
        AgentChain::new(backends, 2)
    }

    #[tokio::test]
    async fn falls_back_on_retryable_errors() {
        let a = Arc::new(FakeAgent::failing("a", true).enforcing(&[ToolPolicy::Web]));
        let b = Arc::new(FakeAgent::echo("b").enforcing(&[ToolPolicy::Web]));
        let reply = chain(vec![a.clone(), b.clone()])
            .run(turn("hi"))
            .await
            .unwrap();
        assert_eq!(reply.backend, "b");
        assert_eq!((a.calls(), b.calls()), (1, 1));
    }

    #[tokio::test]
    async fn stops_on_permanent_errors() {
        let a = Arc::new(FakeAgent::failing("a", false).enforcing(&[ToolPolicy::Web]));
        let b = Arc::new(FakeAgent::echo("b").enforcing(&[ToolPolicy::Web]));
        assert!(chain(vec![a, b.clone()]).run(turn("hi")).await.is_err());
        assert_eq!(b.calls(), 0);
    }

    #[tokio::test]
    async fn skips_backends_that_cannot_enforce_the_policy() {
        let a = Arc::new(FakeAgent::echo("a"));
        let b = Arc::new(FakeAgent::echo("b").enforcing(&[ToolPolicy::Web]));
        let reply = chain(vec![a.clone(), b]).run(turn("hi")).await.unwrap();
        assert_eq!(reply.backend, "b");
        assert_eq!(a.calls(), 0);

        let only_unenforced = chain(vec![Arc::new(FakeAgent::echo("a"))]);
        assert!(matches!(
            only_unenforced.run(turn("hi")).await,
            Err(AgentError::Unsupported(_))
        ));
        let dev = chain(vec![Arc::new(FakeAgent::echo("a"))]).allow_unenforced_tools(true);
        assert_eq!(dev.run(turn("hi")).await.unwrap().backend, "a");
    }

    #[tokio::test]
    async fn caps_combine() {
        let a = Arc::new(FakeAgent::echo("a").enforcing(&[ToolPolicy::Web]));
        let b = Arc::new(FakeAgent::echo("b").enforcing(&[ToolPolicy::McpOnly]));
        let caps = chain(vec![a, b]).caps();
        assert!(caps.enforces(ToolPolicy::Web) && caps.enforces(ToolPolicy::McpOnly));
        assert!(!caps.system_prompt);
    }
}
