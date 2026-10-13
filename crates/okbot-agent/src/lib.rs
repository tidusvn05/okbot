//! Agent backends for okbot: agent CLIs through agent-call, fallback chains and CLI detection.
//!
//! - [`CliBackend`] runs one agent CLI (Claude Code, Codex, OpenCode, Devin) per turn.
//! - [`AgentChain`] tries backends in order, enforces the tool policy and limits concurrency.
//! - [`detect`] finds installed CLIs and whether they are logged in, for `okbot init` and `doctor`.

mod chain;
mod cli;
pub mod detect;
mod env;
mod prompt;
mod slots;

pub use agent_call::Provider;
pub use chain::AgentChain;
pub use cli::{CliBackend, CliBackendConfig};
pub use env::EnvScrub;
pub use prompt::inline_system_prompt;
pub use slots::{MachineSlots, Slot};

/// Parses a provider name as used in configuration (`claude`, `codex`, `opencode`, `devin`).
pub fn parse_provider(name: &str) -> Option<Provider> {
    match name {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        "opencode" => Some(Provider::OpenCodeV2),
        "devin" => Some(Provider::Devin),
        _ => None,
    }
}

/// The configuration name of a provider.
pub fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
        Provider::OpenCodeV2 => "opencode",
        Provider::Devin => "devin",
    }
}

/// Every provider okbot knows, in the order the setup wizard lists them.
pub const PROVIDERS: [Provider; 4] = [
    Provider::Claude,
    Provider::Codex,
    Provider::OpenCodeV2,
    Provider::Devin,
];
