//! Serves a bundle over okbot's scoped MCP endpoint and prints a run token, for trying agent
//! CLIs against it by hand:
//!
//! ```sh
//! cargo run -p okbot-knowledge --example serve_scoped -- path/to/kb zalo:shop user-1
//! ```
#![allow(clippy::unwrap_used)] // example: a panic is an acceptable error report

use std::time::Duration;

use okbot_core::{CancellationToken, ConversationKey, KnowledgeProvider, Requester, UserRef};
use okbot_knowledge::{Knowledge, KnowledgeOptions};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| "kb".into());
    let channel = args.next().unwrap_or_else(|| "console:local".into());
    let user = args.next().unwrap_or_else(|| "you".into());
    let knowledge = std::sync::Arc::new(Knowledge::open(KnowledgeOptions::new(root)).unwrap());
    let stop = CancellationToken::new();
    knowledge.start_mcp(stop.clone()).await.unwrap();
    let who = Requester {
        conversation: ConversationKey::room(channel.parse().unwrap(), "manual"),
        user: UserRef {
            id: user.as_str().into(),
            display_name: user.clone(),
            is_bot: false,
        },
        internal: false,
        person: None,
        personal: true,
    };
    let grant = knowledge
        .grant(
            &who,
            &okbot_core::RunInfo {
                tools: okbot_core::ToolPolicy::Web,
                ttl: Duration::from_secs(3600),
                attachments: &[],
                admin: false,
            },
        )
        .await
        .unwrap();
    println!("url={}", grant.mcp[0].url);
    println!("token={}", grant.env[0].1.expose());
    tokio::signal::ctrl_c().await.unwrap();
    drop(grant);
    stop.cancel();
}
