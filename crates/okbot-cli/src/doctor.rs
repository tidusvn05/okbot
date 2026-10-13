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
            Some(path) => {
                let dirs = okbot_agent::detect::SearchPaths {
                    dirs: path
                        .parent()
                        .map(|p| vec![p.to_path_buf()])
                        .unwrap_or_default(),
                };
                okbot_agent::detect::detect_one_with_env(provider, &dirs, &env).await
            }
            None => okbot_agent::detect::detect_one_with_env(provider, &paths, &env).await,
        };
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
    };
    let result = async {
        let grant = knowledge
            .grant(&who, &okbot_core::RunInfo { tools: okbot_core::ToolPolicy::Web, ttl: Duration::from_secs(30), attachments: &[] })
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
            Ok((_, name)) => r.add(&label, Status::Ok, format!("bot `{name}`")),
            Err(error) => r.add(&label, Status::Fail, error.to_string()),
        }
    }
}

#[cfg(not(feature = "discord"))]
async fn check_discord(_: &Config, _: &mut Report) {}

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
    if config.channels.is_empty() {
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
    check_bridge(&config, &mut r);
    let state = config.state_dir();
    match std::fs::create_dir_all(&state).and_then(|()| {
        okbot_store::SqliteStore::open(&state.join("okbot.db"))
            .map(drop)
            .map_err(std::io::Error::other)
    }) {
        Ok(()) => r.add("state", Status::Ok, state.display().to_string()),
        Err(error) => r.add("state", Status::Fail, error.to_string()),
    }
    (config.bot.name.clone(), r)
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
