//! OAuth for `[[mcp]]` servers end to end (ADR 0012): discovery from the 401 challenge, client
//! registration, `/connect` with PKCE, the callback page, per-person tokens through the gateway
//! (direct chats only), refresh with rotation, `/connections` and `/disconnect`.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

mod support;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use base64::Engine as _;
use okbot_core::{CancellationToken, ChannelCaps, Secret, SystemClock};
use okbot_runtime::oauth::{Connector, SecretKey};
use okbot_runtime::{
    Credentials, ExtraMcp, HttpOptions, McpRules, OAuthSettings, Runtime, RuntimeConfig,
};
use okbot_testkit::{FakeChannel, MemoryStore, PlainRenderer, inbound};
use serde_json::{Value, json};
use sha2::Digest as _;
use support::{Step, ToolAgent};

/// What the provider saw and issued.
#[derive(Default)]
struct Provider {
    /// Registration requests.
    registrations: Vec<Value>,
    /// code → (challenge, redirect_uri, resource)
    codes: HashMap<String, (String, String, String)>,
    /// Valid access tokens.
    access: HashSet<String>,
    /// Valid refresh tokens.
    refresh: HashSet<String>,
    issued: u32,
    /// Bearer tokens of tool calls.
    calls: Vec<String>,
    /// `expires_in` of the next token.
    expires_in: i64,
    /// The token endpoint fails with this status (not `invalid_grant`).
    failing: Option<u16>,
}

type Shared = Arc<Mutex<Provider>>;

fn lock(p: &Shared) -> std::sync::MutexGuard<'_, Provider> {
    p.lock().unwrap()
}

/// An MCP server that is its own authorization server, at `http://<addr>`.
#[allow(clippy::too_many_lines)] // the whole fake provider in one place
async fn provider() -> (SocketAddr, Shared) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let state: Shared = Arc::new(Mutex::new(Provider {
        expires_in: 3600,
        ..Provider::default()
    }));
    let mcp = {
        let (state, base) = (state.clone(), base.clone());
        move |headers: HeaderMap, body: String| {
            let (state, base) = (state.clone(), base.clone());
            async move {
                let bearer = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .unwrap_or_default()
                    .to_string();
                if !lock(&state).access.contains(&bearer) {
                    let challenge = format!(
                        "Bearer error=\"invalid_token\", resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""
                    );
                    return (
                        StatusCode::UNAUTHORIZED,
                        [(header::WWW_AUTHENTICATE, challenge)],
                    )
                        .into_response();
                }
                let msg: Value = serde_json::from_str(&body).unwrap();
                let Some(id) = msg.get("id").cloned() else {
                    return StatusCode::ACCEPTED.into_response();
                };
                let result = match msg["method"].as_str().unwrap() {
                    "initialize" => json!({"protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}}, "serverInfo": {"name": "g", "version": "0"}}),
                    "tools/list" => json!({"tools": [{"name": "search_mail",
                        "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}]}),
                    "tools/call" => {
                        lock(&state).calls.push(bearer);
                        json!({"content": [{"type": "text", "text": "2 unread"}]})
                    }
                    _ => json!({}),
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
            }
        }
    };
    let resource = {
        let base = base.clone();
        move || {
            let base = base.clone();
            async move {
                axum::Json(json!({"resource": format!("{base}/mcp"),
                    "authorization_servers": [base], "scopes_supported": ["mail.read"]}))
            }
        }
    };
    let metadata = {
        let base = base.clone();
        move || {
            let base = base.clone();
            async move {
                axum::Json(json!({
                    "issuer": base,
                    "authorization_endpoint": format!("{base}/authorize"),
                    "token_endpoint": format!("{base}/token"),
                    "registration_endpoint": format!("{base}/register"),
                    "code_challenge_methods_supported": ["S256"],
                }))
            }
        }
    };
    let register = {
        let state = state.clone();
        move |axum::Json(body): axum::Json<Value>| {
            let state = state.clone();
            async move {
                lock(&state).registrations.push(body);
                (
                    StatusCode::CREATED,
                    axum::Json(json!({"client_id": "client-1"})),
                )
            }
        }
    };
    let authorize = {
        let state = state.clone();
        move |uri: axum::http::Uri| {
            let state = state.clone();
            async move {
                let url = reqwest::Url::parse(&format!("http://x{uri}")).unwrap();
                let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
                assert_eq!(q["response_type"], "code");
                assert_eq!(q["client_id"], "client-1");
                assert_eq!(q["code_challenge_method"], "S256");
                assert_eq!(q["scope"], "mail.read");
                let code = format!("code-{}", rand::random::<u32>());
                lock(&state).codes.insert(
                    code.clone(),
                    (
                        q["code_challenge"].clone(),
                        q["redirect_uri"].clone(),
                        q["resource"].clone(),
                    ),
                );
                let to = format!("{}?code={code}&state={}", q["redirect_uri"], q["state"]);
                (StatusCode::FOUND, [(header::LOCATION, to)]).into_response()
            }
        }
    };
    let token = {
        let state = state.clone();
        move |body: String| {
            let state = state.clone();
            async move {
                let form: HashMap<String, String> =
                    reqwest::Url::parse(&format!("http://x/?{body}"))
                        .unwrap()
                        .query_pairs()
                        .into_owned()
                        .collect();
                let mut p = lock(&state);
                assert_eq!(form["client_id"], "client-1");
                if let Some(status) = p.failing {
                    return (
                        StatusCode::from_u16(status).unwrap(),
                        axum::Json(json!({"error": "temporarily_unavailable"})),
                    )
                        .into_response();
                }
                let refused = || {
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({"error": "invalid_grant"})),
                    )
                };
                match form["grant_type"].as_str() {
                    "authorization_code" => {
                        let Some((challenge, redirect, resource)) = p.codes.remove(&form["code"])
                        else {
                            return refused().into_response();
                        };
                        let digest = sha2::Sha256::digest(form["code_verifier"].as_bytes());
                        let expected =
                            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
                        if expected != challenge
                            || redirect != form["redirect_uri"]
                            || resource != form["resource"]
                        {
                            return refused().into_response();
                        }
                    }
                    "refresh_token" => {
                        // Rotation: a refresh token works once.
                        if !p.refresh.remove(&form["refresh_token"]) {
                            return refused().into_response();
                        }
                    }
                    other => panic!("grant {other}"),
                }
                p.issued += 1;
                let (access, refresh) = (format!("at-{}", p.issued), format!("rt-{}", p.issued));
                p.access.insert(access.clone());
                p.refresh.insert(refresh.clone());
                let expires_in = p.expires_in;
                axum::Json(json!({"access_token": access, "token_type": "Bearer",
                    "refresh_token": refresh, "expires_in": expires_in}))
                .into_response()
            }
        }
    };
    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(mcp))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            axum::routing::get(resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(metadata),
        )
        .route("/register", axum::routing::post(register))
        .route("/authorize", axum::routing::get(authorize))
        .route("/token", axum::routing::post(token));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Bot {
    channel: Arc<FakeChannel>,
    agent: Arc<ToolAgent>,
    handle: okbot_runtime::RuntimeHandle,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Bot {
    async fn say(&self, room: &str, user: &str, direct: bool, text: &str) {
        let mut event = inbound("fake:test", room, user, text);
        event.is_direct = direct;
        event.mentions_bot = true;
        self.channel.inject(event).await;
        tokio::time::timeout(Duration::from_secs(20), self.handle.wait_idle())
            .await
            .unwrap();
    }

    fn last(&self) -> String {
        self.channel
            .sent_texts()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

fn start(provider: SocketAddr, credentials: Credentials, steps: Vec<Step>) -> (Bot, u16) {
    let port = free_port();
    let mut config = RuntimeConfig::new("okbot", std::env::temp_dir().join("okbot-oauth-tests"));
    config.mcp = vec![ExtraMcp {
        server: okbot_core::McpServer {
            name: "google".into(),
            url: format!("http://{provider}/mcp"),
            bearer_token_env: "OKBOT_MCP_GOOGLE_TOKEN".into(),
        },
        token: Secret::new(String::new()),
        admins_only: false,
        signed: false,
        rules: McpRules::default(),
        stdio: None,
        oauth: Some(OAuthSettings {
            credentials,
            ..OAuthSettings::default()
        }),
    }];
    config
        .access
        .admins
        .insert(("fake:test".parse().unwrap(), okbot_core::UserId::new("ana")));
    let store = Arc::new(MemoryStore::new(Arc::new(SystemClock)));
    let connector = Arc::new(Connector::new(
        store.clone(),
        SecretKey::new(&SecretKey::generate()),
        Arc::new(SystemClock),
        Some(format!("http://127.0.0.1:{port}/oauth/callback")),
        vec!["http://127.0.0.1:47823/oauth/callback".into()],
        "okbot",
    ));
    let channel = Arc::new(FakeChannel::new("fake:test", ChannelCaps::basic()));
    let agent = Arc::new(ToolAgent::new(steps));
    let runtime = Runtime::builder(config)
        .store(store)
        .agent(agent.clone())
        .renderer(Arc::new(PlainRenderer))
        .channel(channel.clone())
        .oauth(connector)
        .http(HttpOptions::new(SocketAddr::from(([127, 0, 0, 1], port))))
        .build()
        .unwrap();
    let handle = runtime.handle();
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let stop = stop.clone();
        async move { runtime.run(stop).await.unwrap() }
    });
    (
        Bot {
            channel,
            agent,
            handle,
            stop,
            task,
        },
        port,
    )
}

/// The code an "Almost done" page asks to send.
fn code_on(page: &str) -> String {
    let at = page.find("<code>/connect ").expect("a code on the page") + "<code>/connect ".len();
    page[at..at + 6].to_string()
}

/// Opens a connect link as a browser would; the callback page's status and text.
async fn open_link(reply: &str) -> (u16, String) {
    let link = reply
        .lines()
        .find(|l| l.starts_with("http"))
        .expect("a link in the reply");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let authorize = client.get(link).send().await.unwrap();
    assert_eq!(authorize.status(), 302);
    let callback = authorize.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let page = client.get(&callback).send().await.unwrap();
    (page.status().as_u16(), page.text().await.unwrap())
}

#[tokio::test]
async fn people_connect_their_own_accounts_and_use_them_only_privately() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (addr, provider) = provider().await;
    let steps = vec![Step::Call("google", "search_mail", json!({}))];
    let (bot, port) = start(addr, Credentials::Person, steps);

    // Not connected: the tool is missing and the prompt says how to connect.
    bot.say("dm-ana", "ana", true, "how many unread mails?")
        .await;
    assert_eq!(bot.agent.results(), [json!("not offered")]);
    let system = &bot.agent.turns()[0].system;
    assert!(
        system.contains("Not connected yet") && system.contains("`google`"),
        "{system}"
    );

    // In a group, no link: whoever opened it would connect their account.
    bot.say("team", "ana", false, "/connect google").await;
    assert!(bot.last().contains("in a direct chat"), "{}", bot.last());

    bot.say("dm-ana", "ana", true, "/connect google").await;
    let reply = bot.last();
    assert!(reply.contains("Do not share it"), "{reply}");
    let (status, page) = open_link(&reply).await;
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("Almost done"), "{page}");
    assert!(
        page.contains("ana (fake:test/ana)"),
        "the page names who asked: {page}"
    );
    let registration = lock(&provider).registrations[0].clone();
    assert_eq!(
        registration["redirect_uris"],
        json!([
            format!("http://127.0.0.1:{port}/oauth/callback"),
            "http://127.0.0.1:47823/oauth/callback"
        ])
    );
    assert_eq!(registration["token_endpoint_auth_method"], "none");
    // The chat asks for the code; nothing is kept before it arrives.
    for _ in 0..100 {
        if bot.last().contains("send `/connect <code>`") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        bot.last().contains("send `/connect <code>`"),
        "{}",
        bot.last()
    );
    let code = code_on(&page);
    // Someone else, or another chat, cannot use the code.
    bot.say("dm-bob", "bob", true, &format!("/connect {code}"))
        .await;
    assert!(bot.last().contains("not waiting here"), "{}", bot.last());
    bot.say("team", "ana", false, &format!("/connect {code}"))
        .await;
    assert!(bot.last().contains("not waiting here"), "{}", bot.last());
    bot.say("dm-ana", "ana", true, &format!("/connect {code}"))
        .await;
    assert_eq!(bot.last(), "✓ Connected **google**.");

    // A link works once.
    let again = reqwest::get(format!(
        "http://127.0.0.1:{port}/oauth/callback?code=x&state=used"
    ))
    .await
    .unwrap();
    assert_eq!(again.status(), 400);

    bot.say("dm-ana", "ana", true, "how many unread mails?")
        .await;
    assert_eq!(bot.agent.results()[1], json!("2 unread"));
    assert_eq!(lock(&provider).calls, ["at-1"]);
    assert!(!bot.agent.turns()[1].system.contains("Not connected yet"));

    // Personal credentials stay out of groups, and other people have their own.
    bot.say("team", "ana", false, "unread?").await;
    bot.say("dm-bob", "bob", true, "unread?").await;
    assert_eq!(
        bot.agent.results()[2..],
        [json!("not offered"), json!("not offered")]
    );

    bot.say("dm-ana", "ana", true, "/connections").await;
    assert!(
        bot.last().contains("**google**: connected"),
        "{}",
        bot.last()
    );
    bot.say("dm-ana", "ana", true, "/disconnect google").await;
    assert!(
        bot.last().starts_with("Disconnected `google`"),
        "{}",
        bot.last()
    );
    bot.say("dm-ana", "ana", true, "unread?").await;
    assert_eq!(bot.agent.results()[4], json!("not offered"));
    bot.stop.cancel();
    bot.task.await.unwrap();
}

#[tokio::test]
async fn tokens_are_refreshed_once_and_shared_accounts_need_an_admin() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (addr, provider) = provider().await;
    // Tokens that are about to expire are refreshed before each turn.
    lock(&provider).expires_in = 30;
    let steps = vec![Step::Call("google", "search_mail", json!({}))];
    let (bot, _) = start(addr, Credentials::Shared, steps);

    bot.say("dm-bob", "bob", true, "/connect google").await;
    assert!(bot.last().contains("Only administrators"), "{}", bot.last());
    bot.say("dm-ana", "ana", true, "/connect google").await;
    let (status, page) = open_link(&bot.last()).await;
    assert_eq!(status, 200);
    bot.say(
        "dm-ana",
        "ana",
        true,
        &format!("/connect {}", code_on(&page)),
    )
    .await;
    assert_eq!(bot.last(), "✓ Connected **google**.");

    // Everyone uses the shared account, in groups too; each turn refreshes the short token, and
    // the rotated refresh token keeps working.
    bot.say("team", "bob", false, "unread?").await;
    bot.say("dm-bob", "bob", true, "unread?").await;
    assert_eq!(bot.agent.results(), [json!("2 unread"), json!("2 unread")]);
    assert_eq!(lock(&provider).calls, ["at-2", "at-3"]);
    assert_eq!(lock(&provider).registrations.len(), 1, "registered once");

    // A provider that is busy or rate-limits keeps the account: it works again afterwards.
    lock(&provider).failing = Some(429);
    bot.say("team", "bob", false, "unread?").await;
    assert_eq!(bot.agent.results()[2], json!("not offered"));
    lock(&provider).failing = None;
    bot.say("team", "bob", false, "unread?").await;
    assert_eq!(bot.agent.results()[3], json!("2 unread"));

    // A refused refresh (`invalid_grant`) forgets the account.
    lock(&provider).refresh.clear();
    bot.say("team", "bob", false, "unread?").await;
    assert_eq!(bot.agent.results()[4], json!("not offered"));
    bot.say("dm-ana", "ana", true, "/connections").await;
    assert!(
        bot.last().contains("shared account not connected"),
        "{}",
        bot.last()
    );
    bot.stop.cancel();
    bot.task.await.unwrap();
}
