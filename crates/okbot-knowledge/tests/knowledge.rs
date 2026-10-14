//! The knowledge provider against the default bundle, including scope isolation over real MCP.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::path::Path;
use std::time::Duration;

use okbot_core::{
    CancellationToken, ConversationKey, KnowledgeProvider, Requester, RunGrant, UserRef,
};
use okbot_knowledge::{Knowledge, KnowledgeOptions, write_default_bundle};
use serde_json::{Value, json};

fn who(user: &str, room: &str) -> Requester {
    Requester {
        conversation: ConversationKey::room("zalo:shop".parse().unwrap(), room),
        user: UserRef {
            id: user.into(),
            display_name: user.into(),
            is_bot: false,
        },
        internal: false,
        person: None,
        personal: true,
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn profile(name: &str, likes: &str) -> String {
    format!(
        "---\ntype: UserProfile\ntitle: {name}\ndescription: Profile of {name}.\nuser: {name}\nchannel: zalo:shop\n---\n\n{name} likes {likes}.\n"
    )
}

/// A bundle with the default template, two user profiles and a refund policy.
fn bundle() -> (tempfile::TempDir, std::sync::Arc<Knowledge>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("kb");
    write_default_bundle(&root).unwrap();
    write(
        &root,
        "memory/users/zalo-shop/u1.md",
        &profile("u1", "green tea"),
    );
    write(
        &root,
        "memory/users/zalo-shop/u2.md",
        &profile("u2", "black coffee"),
    );
    write(
        &root,
        "knowledge/refunds.md",
        "---\ntype: Policy\ntitle: Refunds\ndescription: Customers may return products within 7 days.\n---\n\nReturns are accepted within 7 days with the receipt.\n",
    );
    let mut options = KnowledgeOptions::new(&root);
    options.index_dir = Some(dir.path().join("index"));
    let knowledge = Knowledge::open(options).unwrap();
    (dir, std::sync::Arc::new(knowledge))
}

#[tokio::test]
async fn context_has_persona_catalog_and_own_profile() {
    let (_dir, k) = bundle();
    let ctx = k.context(&who("u1", "r1")).await.unwrap();
    assert!(
        ctx.persona.contains("friendly and concise"),
        "{}",
        ctx.persona
    );
    assert!(ctx.catalog.contains("knowledge/refunds"), "{}", ctx.catalog);
    assert!(
        !ctx.catalog.contains("memory/users"),
        "catalog must not list personal memory"
    );
    assert!(ctx.user_profile.unwrap().contains("green tea"));
    assert_eq!(
        k.context(&who("u3", "r1")).await.unwrap().user_profile,
        None
    );
}

#[tokio::test]
async fn excerpts_find_knowledge_but_not_other_users() {
    let (_dir, k) = bundle();
    let found = k
        .excerpts(&who("u1", "r1"), "refunds receipt days")
        .await
        .unwrap();
    assert!(found.contains("[knowledge/refunds]"), "{found}");
    let leak = k.excerpts(&who("u1", "r1"), "black coffee").await.unwrap();
    assert!(!leak.contains("black coffee"), "{leak}");
}

#[tokio::test]
async fn template_lints_clean_at_l2() {
    let dir = tempfile::tempdir().unwrap();
    write_default_bundle(&dir.path().join("kb")).unwrap();
    let mut options = KnowledgeOptions::new(dir.path().join("kb"));
    options.index_dir = Some(dir.path().join("index"));
    let k = Knowledge::open(options).unwrap();
    let report = k
        .bundle()
        .lint(&okbase::LintConfig::level(okbase::Level::L2))
        .unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["errors"], 0, "{json:#}");
    assert_eq!(json["warnings"], 0, "{json:#}");
}

/// A minimal MCP client over streamable HTTP.
struct Mcp {
    http: reqwest::Client,
    url: String,
    token: String,
    session: Option<String>,
}

impl Mcp {
    fn new(grant: &RunGrant) -> Self {
        Self {
            http: {
                let _ = rustls::crypto::ring::default_provider().install_default();
                reqwest::Client::new()
            },
            url: grant.mcp[0].url.clone(),
            token: grant.env[0].1.expose().clone(),
            session: None,
        }
    }

    async fn post(&mut self, body: Value) -> (u16, Value) {
        let mut req = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .header("accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(s) = &self.session {
            req = req.header("mcp-session-id", s);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        if let Some(s) = resp.headers().get("mcp-session-id") {
            self.session = Some(s.to_str().unwrap().to_string());
        }
        let text = resp.text().await.unwrap();
        // Streamable HTTP may answer as an SSE stream: take the last `data:` line.
        let data = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or(text.trim())
            .to_string();
        (status, serde_json::from_str(&data).unwrap_or(Value::Null))
    }

    async fn initialize(&mut self) -> u16 {
        let (status, _) = self
            .post(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-03-26","capabilities":{},
                "clientInfo":{"name":"okbot-test","version":"0"}}}),
            )
            .await;
        if status == 200 {
            self.post(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await;
        }
        status
    }

    async fn get(&mut self, id: &str) -> String {
        let (_, v) = self
            .post(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"kb_get","arguments":{"id":id}}}))
            .await;
        v.to_string()
    }
}

#[tokio::test]
async fn mcp_tools_respect_the_requester_scope() {
    let (_dir, k) = bundle();
    let stop = CancellationToken::new();
    k.start_mcp(stop.clone()).await.unwrap();

    let grant = k
        .grant(
            &who("u1", "r1"),
            &okbot_core::RunInfo {
                tools: okbot_core::ToolPolicy::Web,
                ttl: Duration::from_secs(60),
                attachments: &[],
                admin: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(grant.mcp[0].bearer_token_env, "KB_MCP_TOKEN");
    let mut client = Mcp::new(&grant);
    assert_eq!(client.initialize().await, 200);
    let own = client.get("memory/users/zalo-shop/u1").await;
    assert!(own.contains("green tea"), "{own}");
    let other = client.get("memory/users/zalo-shop/u2").await;
    assert!(
        !other.contains("black coffee"),
        "user A read user B's profile: {other}"
    );
    let shared = client.get("knowledge/refunds").await;
    assert!(shared.contains("7 days"), "{shared}");

    let mut forged = Mcp::new(&grant);
    forged.token = "0".repeat(64);
    assert_eq!(forged.initialize().await, 401);

    let mut revoked = Mcp::new(&grant);
    drop(grant);
    assert_eq!(
        revoked.initialize().await,
        401,
        "token revoked when the grant is dropped"
    );
    stop.cancel();
}
