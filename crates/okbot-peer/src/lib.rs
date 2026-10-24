//! Peers and teams (ADR 0014): okbots that delegate work to each other.
//!
//! - [`key`]: each bot's Ed25519 key and the public keys of members and teams.
//! - [`token`]: signed request and response tokens (`okp1`).
//! - [`manifest`]: team manifests, their signatures and links.
//! - [`a2a`]: the A2A subset spoken between members.
//! - [`Node`]: a bot's peer side, the [`Peers`](okbot_core::Peers) port and one `peer:<team>`
//!   channel per team that accepts tasks.

pub mod a2a;
mod channel;
mod inbound;
pub mod key;
pub mod manifest;
mod node;
mod outbound;
mod relay;
pub mod token;

pub use key::{PublicKey, SigningKey};
pub use manifest::Manifest;
pub use node::{ManifestSource, NodeConfig, SIGNATURE_HEADER, SOCKET_FILE, TeamConfig};
pub use outbound::Node;

/// A peer failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A key is missing or invalid.
    #[error("key: {0}")]
    Key(String),
    /// A token is invalid.
    #[error("token: {0}")]
    Token(String),
    /// A manifest is invalid, unsigned, expired or older than the one in use.
    #[error("team manifest: {0}")]
    Manifest(String),
    /// A file could not be read or written.
    #[error("{0}: {1}")]
    Io(String, #[source] std::io::Error),
}
