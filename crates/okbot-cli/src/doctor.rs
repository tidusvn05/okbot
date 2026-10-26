//! `okbot doctor`: checks one bot (or all) and explains what to fix.

use std::time::Duration;

use okbot_config::Config;
use okbot_core::{CancellationToken, ConversationKey, KnowledgeProvider, Requester, UserRef};
use serde::Serialize;

use crate::exit::CliError;

/// Arguments of `okbot doctor`.
#[derive(Debug, clap::Args)]
pub(crate) struct DoctorArgs {
    /// Bot name; default: the bot in the current directory. `--all` checks every bot.
    name: Option<String>,
    /// Check every registered bot.
    #[arg(long, conflicts_with = "name")]
    all: bool,
    /// Also ask Zalo to call the webhook (rate-limited by Zalo: use sparingly).
    #[arg(long)]
    test_webhook: bool,
    /// Print JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct Check {
    #[serde(rename = "check")]
    name: String,
    status: Status,
    detail: String,
}

#[derive(Default)]
struct Report(Vec<Check>);

impl Report {
    fn add(&mut self, check: impl Into<String>, status: Status, detail: impl Into<String>) {
        self.0.push(Check {
            name: check.into(),
            status,
            detail: detail.into(),
        });
    }
}

async fn check_agents(config: &Config, r: &mut Report) {
    let paths = okbot_agent::detect::SearchPaths::from_env();
    let env = crate::bot::agent_env(config);
    for name in &config.agent.chain {
        let Some(provider) = okbot_agent::parse_provider(name) else {
            continue;
        };
        let configured = config.agent.providers.get(name).and_then(|p| p.cli.clone());
        let found = match &configured {
            Some(path) if config.path(path).exists() => {
                okbot_agent::detect::detect_path(provider, &config.path(path), &env).await
            }
            _ => okbot_agent::detect::detect_one_with_env(provider, &paths, &env).await,
        };
        if let (Some(path), Some(cli)) = (&configured, &found)
            && !config.path(path).exists()
        {
            r.add(
                format!("agent {name} cli"),
                Status::Warn,
                format!(
                    "{} no longer exists; the bot uses {} instead. `okbot agent set {name} \
                     --default-cli` drops the setting so it is found at each start",
                    path.display(),
                    cli.path.display()
                ),
            );
        }
        match found {
            None => r.add(
                format!("agent {name}"),
                Status::Fail,
                format!(
                    "{name} not found{}",
                    configured
                        .map(|p| format!(" at {}", p.display()))
                        .unwrap_or_default()
                ),
            ),
            Some(cli) => {
                let (status, login) = match cli.login {
                    okbot_agent::detect::Login::LoggedIn => (Status::Ok, "logged in"),
                    okbot_agent::detect::Login::LoggedOut if env.is_empty() => {
                        (Status::Fail, "not logged in")
                    }
                    okbot_agent::detect::Login::LoggedOut => (
                        Status::Fail,
                        "not logged in in [agent] home (log in with HOME=<home> <cli>)",
                    ),
                    okbot_agent::detect::Login::Unknown => (Status::Warn, "login unknown"),
                };
                r.add(
                    format!("agent {name}"),
                    status,
                    format!("{} {login} ({})", cli.version, cli.path.display()),
                );
            }
        }
    }
    match crate::bot::agent_chain(config) {
        Ok(_) => r.add(
            "tool policy",
            Status::Ok,
            format!("{:?} enforced", config.agent.tool_policy),
        ),
        Err(error) => r.add("tool policy", Status::Fail, format!("{error:#}")),
    }
    if interactive(config) {
        match crate::bot::agent_chain(config)
            .map(|chain| okbot_core::AgentBackend::caps(&chain).mcp)
        {
            Ok(true) => r.add("interactive replies", Status::Ok, "on"),
            Ok(false) => r.add(
                "interactive replies",
                Status::Warn,
                "reply_mode = \"interactive\" needs an agent that takes MCP servers (claude or \
                 codex); answers stay text",
            ),
            Err(_) => {}
        }
    }
    if config.charts.images {
        match crate::bot::charts(config) {
            Ok(_) if interactive(config) => r.add("chart images", Status::Ok, "on"),
            Ok(_) => r.add(
                "chart images",
                Status::Warn,
                "[charts] images = true only applies to reply_mode = \"interactive\" rooms",
            ),
            Err(error) => r.add("chart images", Status::Fail, format!("{error:#}")),
        }
    }
}

/// Whether the bot, a channel or a room has `reply_mode = "interactive"` (ADR 0015).
fn interactive(config: &Config) -> bool {
    let on = |m: Option<okbot_core::ReplyMode>| m == Some(okbot_core::ReplyMode::Interactive);
    on(Some(config.bot.reply_mode))
        || config
            .channels
            .access()
            .iter()
            .any(|(_, _, a)| on(a.reply_mode) || a.rooms.iter().any(|room| on(room.reply_mode)))
}

async fn check_knowledge(config: &Config, r: &mut Report) {
    let stop = CancellationToken::new();
    let knowledge = match crate::bot::knowledge(config, stop.clone()).await {
        Ok(k) => k,
        Err(error) => {
            r.add("knowledge", Status::Fail, format!("{error:#}"));
            return;
        }
    };
    let lint = knowledge
        .bundle()
        .lint(&okbase::LintConfig::level(okbase::Level::L2))
        .ok()
        .and_then(|report| serde_json::to_value(report).ok());
    match lint {
        Some(v) if v["errors"] == 0 => r.add(
            "knowledge lint",
            Status::Ok,
            format!("L2, {} warnings", v["warnings"]),
        ),
        Some(v) => r.add(
            "knowledge lint",
            Status::Warn,
            format!(
                "{} errors, {} warnings: run `okbot kb lint`",
                v["errors"], v["warnings"]
            ),
        ),
        None => r.add("knowledge lint", Status::Warn, "lint failed"),
    }
    // A real MCP round trip with a run token, as an agent would make it.
    let who = Requester {
        conversation: ConversationKey::room("console:local".parse().expect("valid id"), "doctor"),
        user: UserRef {
            id: "doctor".into(),
            display_name: "doctor".into(),
            is_bot: false,
        },
        internal: false,
        person: None,
        personal: true,
    };
    let result = async {
        let grant = knowledge
            .grant(&who, &okbot_core::RunInfo { tools: okbot_core::ToolPolicy::Web, ttl: Duration::from_secs(30), attachments: &[], admin: false, read_only: false })
            .await?;
        let server = grant.mcp.first().ok_or_else(|| anyhow::anyhow!("no MCP endpoint"))?;
        let status = reqwest::Client::new()
            .post(&server.url)
            .bearer_auth(grant.env[0].1.expose())
            .header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"okbot-doctor","version":"0"}}}))
            .send()
            .await?
            .status();
        anyhow::ensure!(status.is_success(), "initialize returned {status}");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => r.add("knowledge MCP", Status::Ok, "round trip with a run token"),
        Err(error) => r.add("knowledge MCP", Status::Fail, format!("{error:#}")),
    }
    stop.cancel();
}

#[cfg(feature = "zalo")]
async fn check_zalo(config: &Config, test_webhook: bool, r: &mut Report) {
    use okbot_channel_zalo::DEFAULT_API_BASE;
    for (instance, zalo) in &config.channels.zalo {
        let label = format!("zalo:{instance}");
        let token = match config.secret(&zalo.token_env) {
            Ok(t) => t,
            Err(error) => {
                r.add(&label, Status::Fail, error.to_string());
                continue;
            }
        };
        let base = std::env::var("OKBOT_ZALO_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.into());
        let api = okbot_channel_zalo::client(&base, &token);
        match api.get_me().await {
            Ok(me) => r.add(
                &label,
                Status::Ok,
                format!(
                    "bot `{}` ({}), {:?} delivery",
                    me.account_name, me.account_type, zalo.delivery
                ),
            ),
            Err(error) => {
                r.add(&label, Status::Fail, error.to_string());
                continue;
            }
        }
        if zalo.delivery == okbot_config::Delivery::Polling {
            r.add(
                format!("{label} delivery"),
                Status::Warn,
                "polling: Zalo recommends webhook delivery for production",
            );
            continue;
        }
        let expected = zalo.public_url.clone().unwrap_or_default();
        match api.get_webhook_info().await.map(|info| info.url) {
            Ok(url) if url == expected => r.add(format!("{label} webhook"), Status::Ok, url),
            Ok(url) if url.is_empty() => r.add(
                format!("{label} webhook"),
                Status::Warn,
                "not registered yet; `okbot run` registers it",
            ),
            Ok(url) => r.add(
                format!("{label} webhook"),
                Status::Warn,
                format!("registered {url}, configured {expected}; `okbot run` updates it"),
            ),
            Err(error) => r.add(format!("{label} webhook"), Status::Fail, error.to_string()),
        }
        if test_webhook {
            match api.test_webhook().await {
                Ok(check) if check.ok => {
                    r.add(
                        format!("{label} webhook test"),
                        Status::Ok,
                        check.outcome.to_string(),
                    );
                }
                Ok(check) => r.add(
                    format!("{label} webhook test"),
                    Status::Fail,
                    format!("{}: {}", check.outcome, check.hint),
                ),
                Err(error) => r.add(
                    format!("{label} webhook test"),
                    Status::Fail,
                    error.to_string(),
                ),
            }
        }
    }
}

#[cfg(not(feature = "zalo"))]
async fn check_zalo(_: &Config, _: bool, _: &mut Report) {}

#[cfg(feature = "gchat")]
async fn check_gchat(config: &Config, r: &mut Report) {
    use okbot_channel_gchat::{ServiceAccount, TokenSource};
    for (instance, g) in &config.channels.gchat {
        let label = format!("gchat:{instance}");
        let account = match ServiceAccount::from_file(&config.path(&g.credentials)) {
            Ok(a) => match std::env::var("OKBOT_GCHAT_API_BASE") {
                Ok(base) => a.with_token_uri(&format!("{base}/token")),
                Err(_) => a,
            },
            Err(error) => {
                r.add(&label, Status::Fail, error.to_string());
                continue;
            }
        };
        let email = account.client_email.clone();
        match TokenSource::new(account, reqwest::Client::new())
            .token()
            .await
        {
            Ok(_) => r.add(
                &label,
                Status::Ok,
                format!("service account {email}, {:?} delivery", g.delivery),
            ),
            Err(error) => r.add(&label, Status::Fail, format!("{email}: {error}")),
        }
    }
}

#[cfg(not(feature = "gchat"))]
async fn check_gchat(_: &Config, _: &mut Report) {}

#[cfg(feature = "discord")]
async fn check_discord(config: &Config, r: &mut Report) {
    use okbot_channel_discord::{DiscordChannel, DiscordConfig};
    for (instance, d) in &config.channels.discord {
        let label = format!("discord:{instance}");
        let token = match config.secret(&d.token_env) {
            Ok(t) => t,
            Err(error) => {
                r.add(&label, Status::Fail, error.to_string());
                continue;
            }
        };
        let channel = DiscordChannel::new(DiscordConfig {
            id: okbot_core::ChannelId::new("discord", instance).expect("validated id"),
            token,
            message_content: d.message_content,
            http_proxy: std::env::var("OKBOT_DISCORD_HTTP_PROXY").ok(),
        });
        match channel.current_user().await {
            Ok((_, name)) => {
                r.add(&label, Status::Ok, format!("bot `{name}`"));
                if let Ok(app) = channel.application_id().await {
                    r.add(
                        format!("{label} invite"),
                        Status::Ok,
                        okbot_channel_discord::install_url(app),
                    );
                }
            }
            Err(error) => r.add(&label, Status::Fail, error.to_string()),
        }
    }
}

#[cfg(not(feature = "discord"))]
async fn check_discord(_: &Config, _: &mut Report) {}

#[cfg(feature = "telegram")]
async fn check_telegram(config: &Config, r: &mut Report) {
    use okbot_channel_telegram::{API_BASE, TelegramApi};
    for (instance, t) in &config.channels.telegram {
        let label = format!("telegram:{instance}");
        let token = match config.secret(&t.token_env) {
            Ok(token) => token,
            Err(error) => {
                r.add(&label, Status::Fail, error.to_string());
                continue;
            }
        };
        let base = std::env::var("OKBOT_TELEGRAM_API_BASE").unwrap_or_else(|_| API_BASE.into());
        let api = TelegramApi::new(&base, token);
        let me = match api.get_me().await {
            Ok(me) => me,
            Err(error) => {
                r.add(&label, Status::Fail, error.to_string());
                continue;
            }
        };
        r.add(
            &label,
            Status::Ok,
            format!("bot @{}, {:?} delivery", me.username, t.delivery).to_lowercase(),
        );
        let reads_all = t.reply_on.contains(&okbot_config::ReplyOn::All);
        if reads_all && !me.can_read_all_group_messages {
            r.add(
                format!("{label} groups"),
                Status::Warn,
                "reply_on includes \"all\" but privacy mode is on: turn it off in @BotFather \
                 (/setprivacy), then re-add the bot to its groups",
            );
        }
        if t.delivery == okbot_config::Delivery::Webhook {
            if let Some(secret_env) = &t.secret_env
                && let Ok(secret) = config.secret(secret_env)
                && !okbot_channel_telegram::valid_secret(secret.expose())
            {
                r.add(
                    format!("{label} webhook"),
                    Status::Fail,
                    format!("{secret_env} may only hold A-Z, a-z, 0-9, `_` and `-` (1–256)"),
                );
            }
            match api.webhook_info().await {
                Ok(info) if info.url.is_empty() => r.add(
                    format!("{label} webhook"),
                    Status::Warn,
                    "not registered yet; it is set when the bot starts",
                ),
                Ok(info) => {
                    let expected = t.public_url.clone().unwrap_or_default();
                    let (status, note) = match &info.last_error_message {
                        Some(e) => (Status::Warn, format!("; last error: {e}")),
                        None if info.url != expected => (
                            Status::Warn,
                            format!("; okbot.toml says {expected}, restart the bot"),
                        ),
                        None => (Status::Ok, String::new()),
                    };
                    r.add(
                        format!("{label} webhook"),
                        status,
                        format!("{} ({} pending){note}", info.url, info.pending_update_count),
                    );
                }
                Err(error) => r.add(format!("{label} webhook"), Status::Warn, error.to_string()),
            }
        }
    }
}

#[cfg(not(feature = "telegram"))]
async fn check_telegram(_: &Config, _: &mut Report) {}

#[cfg(feature = "slack")]
async fn check_slack(config: &Config, r: &mut Report) {
    use okbot_channel_slack::{SLACK_API, SlackWeb};
    for (instance, sl) in &config.channels.slack {
        let label = format!("slack:{instance}");
        let (app, bot) = match (
            config.secret(&sl.app_token_env),
            config.secret(&sl.bot_token_env),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                r.add(&label, Status::Fail, e.to_string());
                continue;
            }
        };
        let base = std::env::var("OKBOT_SLACK_API_BASE").unwrap_or_else(|_| SLACK_API.into());
        let web = SlackWeb::new(&base, app, bot);
        match (web.auth_test().await, web.connections_open().await) {
            (Ok(user), Ok(_)) => r.add(
                &label,
                Status::Ok,
                format!("bot user {user}, Socket Mode reachable"),
            ),
            (Err(e), _) => r.add(&label, Status::Fail, format!("bot token: {e}")),
            (_, Err(e)) => r.add(
                &label,
                Status::Fail,
                format!("app token (Socket Mode): {e}"),
            ),
        }
    }
}

#[cfg(not(feature = "slack"))]
async fn check_slack(_: &Config, _: &mut Report) {}

/// Bridge adapters: the executable can be found and its variables are set.
fn check_bridge(config: &Config, r: &mut Report) {
    for (instance, b) in &config.channels.bridge {
        let label = format!("{}:{instance}", b.platform);
        let command = b
            .command
            .clone()
            .unwrap_or_else(|| format!("okbot-channel-{}", b.platform));
        let found = if command.contains('/') {
            config.dir.join(&command).is_file()
        } else {
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|d| d.join(&command).is_file())
            })
        };
        let missing: Vec<&str> = b
            .env
            .iter()
            .filter(|name| config.secret(name).is_err())
            .map(String::as_str)
            .collect();
        if !found {
            r.add(
                &label,
                Status::Fail,
                format!("adapter `{command}` not found"),
            );
        } else if !missing.is_empty() {
            r.add(
                &label,
                Status::Fail,
                format!("missing variables: {}", missing.join(", ")),
            );
        } else {
            r.add(&label, Status::Ok, format!("bridge adapter `{command}`"));
        }
    }
}

async fn check_bot(dir: &std::path::Path, args: &DoctorArgs) -> (String, Report) {
    let mut r = Report::default();
    let config = match Config::load(dir) {
        Ok(c) => c,
        Err(error) => {
            r.add("config", Status::Fail, error.to_string());
            return (crate::registry::name_of(dir), r);
        }
    };
    r.add(
        "config",
        Status::Ok,
        dir.join(okbot_config::CONFIG_FILE).display().to_string(),
    );
    if config.channels.is_empty() && config.teams.is_empty() && config.a2a.client.is_empty() {
        r.add(
            "channels",
            Status::Warn,
            "no channel configured; only `okbot chat` works",
        );
    }
    check_agents(&config, &mut r).await;
    check_knowledge(&config, &mut r).await;
    check_zalo(&config, args.test_webhook, &mut r).await;
    check_gchat(&config, &mut r).await;
    check_discord(&config, &mut r).await;
    check_slack(&config, &mut r).await;
    check_telegram(&config, &mut r).await;
    check_bridge(&config, &mut r);
    let state = config.state_dir();
    match std::fs::create_dir_all(&state).and_then(|()| {
        okbot_store::SqliteStore::open(&state.join("okbot.db")).map_err(std::io::Error::other)
    }) {
        Ok(store) => {
            r.add("state", Status::Ok, state.display().to_string());
            if config.bot.profile == okbot_config::BotProfile::Personal {
                check_owners(&store, &mut r).await;
            }
            check_reminders(&config, &store, &mut r).await;
            check_notify(&config, &store, &mut r).await;
            check_oauth(&config, &store, &mut r).await;
            check_automations(&config, &store, &mut r).await;
            check_teams(&config, store, &mut r).await;
        }
        Err(error) => r.add("state", Status::Fail, error.to_string()),
    }
    (config.bot.name.clone(), r)
}

/// `[teams]` (ADR 0014): manifests, membership, and the members this bot may call.
async fn check_teams(config: &Config, store: okbot_store::SqliteStore, r: &mut Report) {
    if config.teams.is_empty() && config.a2a.agent.is_empty() && config.a2a.client.is_empty() {
        return;
    }
    let node = match crate::team::node_unstarted(config, std::sync::Arc::new(store)) {
        Ok(node) => node,
        Err(error) => {
            r.add("teams", Status::Fail, format!("{error:#}"));
            return;
        }
    };
    for (label, ok, detail) in node.check().await {
        r.add(label, if ok { Status::Ok } else { Status::Warn }, detail);
    }
    let remote = config
        .teams
        .values()
        .any(|t| t.manifest.starts_with("https://"));
    if remote && config.http.public_url.is_none() {
        r.add(
            "teams",
            Status::Warn,
            "a team spans machines but [http] public_url is not set: members reach this bot only \
             through a relay",
        );
    }
}

/// `auth = "oauth"` servers (ADR 0012): where people connect, and whose accounts are connected.
async fn check_oauth(config: &Config, store: &okbot_store::SqliteStore, r: &mut Report) {
    use okbot_core::ConversationStore as _;
    let servers: Vec<&okbot_config::McpSection> = config
        .mcp
        .iter()
        .filter(|m| m.auth == okbot_config::McpAuth::Oauth)
        .collect();
    if servers.is_empty() {
        return;
    }
    match crate::mcp::link_redirect(config) {
        Some(url) if url.starts_with("https://") => r.add("oauth callback", Status::Ok, url),
        Some(url) => r.add(
            "oauth callback",
            Status::Warn,
            format!(
                "{url}: only a browser on this machine reaches it; set [http] public_url for \
                 people elsewhere, or use `okbot mcp login`"
            ),
        ),
        None => r.add(
            "oauth callback",
            Status::Warn,
            "no [http] listen: `/connect` cannot work; use `okbot mcp login <server>`",
        ),
    }
    if let Err(error) = crate::mcp::secret_key(config) {
        r.add("oauth key", Status::Fail, error.to_string());
    }
    let connected = store
        .sealed_entries("oauth-token")
        .await
        .unwrap_or_default();
    for m in servers {
        let who: Vec<&str> = connected
            .iter()
            .filter(|e| e.server == m.name)
            .map(|e| e.subject.as_str())
            .collect();
        let label = format!("mcp {}", m.name);
        if who.is_empty() {
            r.add(
                &label,
                Status::Warn,
                format!("no account connected yet (`/connect {}`)", m.name),
            );
        } else {
            r.add(&label, Status::Ok, format!("connected: {}", who.join(", ")));
        }
    }
}

/// `[[automation]]` (ADR 0013): webhook tokens, and the last run of each.
async fn check_automations(config: &Config, store: &okbot_store::SqliteStore, r: &mut Report) {
    use okbot_core::ConversationStore as _;
    for a in &config.automations {
        let label = format!("automation {}", a.name);
        if a.webhook && config.secret(&a.token_env).is_err() {
            r.add(
                &label,
                Status::Fail,
                format!(
                    "{} is not set (callers send it as a bearer token)",
                    a.token_env
                ),
            );
            continue;
        }
        if a.paused {
            r.add(&label, Status::Ok, "paused");
            continue;
        }
        let last = store
            .automation_runs(Some(&a.name), jiff::Timestamp::UNIX_EPOCH, 1)
            .await
            .ok()
            .and_then(|runs| runs.into_iter().next());
        match last {
            None => r.add(&label, Status::Ok, "no runs yet"),
            Some(run) => match run.error {
                Some(error) => r.add(&label, Status::Warn, format!("last run failed: {error}")),
                None => r.add(
                    &label,
                    Status::Ok,
                    format!(
                        "last run {}: {} ({} item(s))",
                        run.at, run.action, run.items
                    ),
                ),
            },
        }
    }
}

/// Platforms whose bots open a direct chat themselves (the others need a first message).
const OPENS_DIRECT: &[&str] = &["discord", "telegram"];

/// `[[notify]]`: the caller token is set, and every person it names can be reached.
async fn check_notify(config: &Config, store: &okbot_store::SqliteStore, r: &mut Report) {
    use okbot_core::{ConversationStore as _, NotifyTo};
    let reachable = async |account: &str| {
        let Ok((channel, user)) = okbot_config::parse_account(account) else {
            return false;
        };
        OPENS_DIRECT.contains(&channel.platform())
            || matches!(store.direct_chat(&channel, &user).await, Ok(Some(_)))
    };
    for n in &config.notify {
        let label = format!("notify {}", n.name);
        if config.secret(&n.token_env).is_err() {
            r.add(
                &label,
                Status::Fail,
                format!(
                    "{} is not set (callers send it as a bearer token)",
                    n.token_env
                ),
            );
            continue;
        }
        let mut waiting = Vec::new();
        for target in n.to.iter().filter_map(|t| t.parse::<NotifyTo>().ok()) {
            match target {
                NotifyTo::User(channel, user) => {
                    let account = format!("{channel}/{user}");
                    if !reachable(&account).await {
                        waiting.push(account);
                    }
                }
                NotifyTo::Admins => {
                    for account in &config.bot.admins {
                        if !reachable(account).await {
                            waiting.push(account.clone());
                        }
                    }
                }
                // One chat per person: reachable when any of their accounts is.
                NotifyTo::Person(id) => {
                    let accounts = config.people.get(&id).map(|p| p.accounts.as_slice());
                    let mut any = false;
                    for account in accounts.unwrap_or_default() {
                        any = any || reachable(account).await;
                    }
                    if !any && id != okbot_runtime::OWNER {
                        waiting.push(format!("person:{id}"));
                    }
                }
                _ => {}
            }
        }
        if waiting.is_empty() {
            r.add(
                &label,
                Status::Ok,
                format!("{} target(s); POST /notify/{}", n.to.len(), n.name),
            );
        } else {
            r.add(
                &label,
                Status::Warn,
                format!(
                    "not reachable until they write to the bot in a direct chat: {}",
                    waiting.join(", ")
                ),
            );
        }
    }
}

/// Reminders: the time zone they are read in, and firings that were not delivered.
async fn check_reminders(config: &Config, store: &okbot_store::SqliteStore, r: &mut Report) {
    use okbot_core::{ConversationStore as _, FireState};
    if !config.schedule.enabled {
        r.add("reminders", Status::Ok, "off ([schedule] enabled = false)");
        return;
    }
    if config.bot.timezone.is_none() {
        r.add(
            "reminders",
            Status::Warn,
            "[bot] timezone is not set, so reminder times are read as UTC; set it, for example \
             timezone = \"Asia/Ho_Chi_Minh\"",
        );
    }
    match store.recent_fires(50).await {
        Ok(fires) => {
            let bad: Vec<String> = fires
                .iter()
                .filter(|f| matches!(f.state, FireState::Failed | FireState::Missed))
                .take(5)
                .map(|f| format!("{} {} at {}", f.task_id, f.state.as_str(), f.fire_at))
                .collect();
            let sent = fires.iter().filter(|f| f.state == FireState::Sent).count();
            if bad.is_empty() {
                r.add(
                    "reminders",
                    Status::Ok,
                    format!("{sent} sent recently; `okbot task list` shows what is next"),
                );
            } else {
                r.add(
                    "reminders",
                    Status::Warn,
                    format!(
                        "not delivered: {} (see `okbot logs`; a reminder needs a chat where the \
                         owner wrote to the bot)",
                        bad.join("; ")
                    ),
                );
            }
        }
        Err(error) => r.add("reminders", Status::Fail, error.to_string()),
    }
}

/// Personal bots answer nobody until an account is paired.
async fn check_owners(store: &okbot_store::SqliteStore, r: &mut Report) {
    use okbot_core::ConversationStore as _;
    match store.owners().await {
        Ok(owners) if owners.is_empty() => r.add(
            "owner",
            Status::Warn,
            "personal bot without a paired account: run `okbot pair` and send the code to the bot",
        ),
        Ok(owners) => r.add(
            "owner",
            Status::Ok,
            format!(
                "personal bot, {} paired account(s): {}",
                owners.len(),
                owners
                    .iter()
                    .map(|(c, u)| format!("{c}/{u}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        Err(error) => r.add("owner", Status::Fail, error.to_string()),
    }
}

pub(crate) async fn run(args: DoctorArgs) -> anyhow::Result<()> {
    crate::logging::init("error");
    let dirs = if args.all {
        crate::registry::all()?
            .into_iter()
            .map(|(_, d)| d)
            .collect()
    } else {
        vec![crate::registry::resolve(args.name.as_deref())?]
    };
    let mut failed = 0;
    let mut out = Vec::new();
    for dir in &dirs {
        let (name, report) = check_bot(dir, &args).await;
        failed += report.0.iter().filter(|c| c.status == Status::Fail).count();
        if !args.json {
            println!("{name} ({})", dir.display());
            for c in &report.0 {
                let mark = match c.status {
                    Status::Ok => "✓",
                    Status::Warn => "!",
                    Status::Fail => "✗",
                };
                println!("  {mark} {:<22} {}", c.name, c.detail);
            }
        }
        out.push(serde_json::json!({ "bot": name, "dir": dir, "checks": report.0 }));
    }
    if args.json {
        println!("{}", serde_json::json!({ "ok": failed == 0, "bots": out }));
    }
    if failed > 0 {
        let message = format!("{failed} check(s) failed");
        return Err(if args.json {
            CliError::reported_findings(message)
        } else {
            CliError::findings(message)
        }
        .into());
    }
    Ok(())
}
