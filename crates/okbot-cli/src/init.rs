//! `okbot init` and `okbot new`: the setup wizard (design §6.6).
//!
//! Every question has a flag. With a terminal the wizard asks for what is missing; without one
//! (or with `--yes` / `--json`) a missing answer ends with exit code 3 and names the flag, so a
//! coding agent can relay the question to its user.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use okbot_agent::detect::{self, DetectedCli, Login};
use okbot_config::{
    CONFIG_FILE, Delivery, ENV_FILE, GChatDelivery, GChatInit, InitOptions, ZaloInit, upsert_env,
};

use crate::exit::CliError;

/// Messaging platform for the new bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Platform {
    /// Zalo Bot Platform.
    Zalo,
    /// Google Chat app.
    Gchat,
    /// Discord bot.
    Discord,
    /// Slack app (Socket Mode).
    Slack,
    /// No platform yet (use `okbot chat` to try the bot).
    None,
}

/// How Zalo delivers messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DeliveryArg {
    /// Long polling: no public URL needed.
    Polling,
    /// Zalo calls a public HTTPS URL (recommended for production).
    Webhook,
}

/// How Google Chat delivers events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum GChatDeliveryArg {
    /// Cloud Pub/Sub pull subscription: no public URL needed.
    Pubsub,
    /// Google calls a public HTTPS URL.
    Http,
}

/// Arguments shared by `okbot init` and `okbot new`.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct InitArgs {
    /// Bot name (default: the directory name).
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Agent CLIs in fallback order (`claude,codex`). Default: ask, or the only logged-in one.
    #[arg(long, value_delimiter = ',')]
    agent: Vec<String>,
    /// Messaging platform.
    #[arg(long, value_enum)]
    platform: Option<Platform>,
    #[command(flatten)]
    pub(crate) channel: PlatformArgs,
    /// IANA timezone shown to the agent (`Asia/Ho_Chi_Minh`).
    #[arg(long)]
    timezone: Option<String>,
    /// Overwrite an existing okbot.toml.
    #[arg(long)]
    force: bool,
    /// Never ask; fail with exit code 3 when an answer is missing.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Print the result as JSON (implies --yes).
    #[arg(long)]
    pub(crate) json: bool,
}

/// A platform's credentials and delivery (`okbot init` and `okbot channel add`).
#[derive(Debug, Clone, Default, clap::Args)]
pub(crate) struct PlatformArgs {
    /// Environment variable that already holds the Zalo bot token (otherwise you are asked for
    /// the token and it is saved to .env).
    #[arg(long)]
    zalo_token_env: Option<String>,
    /// How Zalo delivers messages.
    #[arg(long, value_enum)]
    delivery: Option<DeliveryArg>,
    /// Public HTTPS URL for webhook delivery (`https://bot.example.com/zalo/<instance>`).
    #[arg(long)]
    public_url: Option<String>,
    /// Google Chat: the app's service-account key file (copied into the bot directory).
    #[arg(long)]
    gchat_credentials: Option<PathBuf>,
    /// Google Chat: how events arrive.
    #[arg(long, value_enum)]
    gchat_delivery: Option<GChatDeliveryArg>,
    /// Google Chat: Pub/Sub subscription (`projects/<p>/subscriptions/<s>`).
    #[arg(long)]
    gchat_subscription: Option<String>,
    /// Discord: environment variable that already holds the bot token.
    #[arg(long)]
    discord_token_env: Option<String>,
    /// Slack: environment variable that already holds the app-level token (`xapp-…`).
    #[arg(long)]
    slack_app_token_env: Option<String>,
    /// Slack: environment variable that already holds the bot token (`xoxb-…`).
    #[arg(long)]
    slack_bot_token_env: Option<String>,
    /// HTTP listen address for webhooks (default: the first free port from 127.0.0.1:8787).
    #[arg(long)]
    listen: Option<String>,
    /// Skip checking credentials with the platform's API.
    #[arg(long)]
    no_verify: bool,
}

pub(crate) struct Wizard<'a> {
    pub(crate) args: &'a PlatformArgs,
    pub(crate) agents: &'a [String],
    pub(crate) interactive: bool,
    /// Adding a channel to an existing bot: never reuse a secret name already in `.env`.
    pub(crate) adding: bool,
}

impl Wizard<'_> {
    fn ask_or_fail<T>(
        &self,
        flag: &'static str,
        question: &str,
        ask: impl FnOnce() -> inquire::error::InquireResult<T>,
    ) -> anyhow::Result<T> {
        if !self.interactive {
            return Err(CliError::needs(flag, format!("{question} (pass {flag})")).into());
        }
        ask().map_err(|e| anyhow::anyhow!("{question}: {e}"))
    }
}

fn describe(cli: &DetectedCli) -> String {
    let login = match cli.login {
        Login::LoggedIn => "logged in",
        Login::LoggedOut => "NOT logged in",
        Login::Unknown => "login unknown",
    };
    format!(
        "{} {} ({login}) — {}",
        okbot_agent::provider_name(cli.provider),
        cli.version,
        cli.path.display()
    )
}

/// Step 2: agent CLIs.
async fn choose_agents(w: &Wizard<'_>) -> anyhow::Result<Vec<(String, Option<PathBuf>)>> {
    let found = detect::detect(&detect::SearchPaths::from_env()).await;
    let path_of = |name: &str| {
        found
            .iter()
            .find(|c| okbot_agent::provider_name(c.provider) == name)
            .map(|c| c.path.clone())
    };
    if !w.agents.is_empty() {
        for name in w.agents {
            if okbot_agent::parse_provider(name).is_none() {
                bail!("unknown agent `{name}` (use claude, codex, opencode or devin)");
            }
        }
        return Ok(w.agents.iter().map(|n| (n.clone(), path_of(n))).collect());
    }
    // Only CLIs that can restrict their tools are offered; Devin/OpenCode need --agent.
    let usable: Vec<&DetectedCli> = found
        .iter()
        .filter(|c| matches!(okbot_agent::provider_name(c.provider), "claude" | "codex"))
        .collect();
    match usable.as_slice() {
        [] => Err(CliError::needs(
            "--agent",
            "no Claude Code or Codex CLI found. Install and log in to one \
             (claude: `npm i -g @anthropic-ai/claude-code && claude`; codex: \
             `npm i -g @openai/codex && codex login`), then run init again",
        )
        .into()),
        [only] => Ok(vec![(
            okbot_agent::provider_name(only.provider).to_string(),
            Some(only.path.clone()),
        )]),
        several => {
            let labels: Vec<String> = several.iter().map(|c| describe(c)).collect();
            let primary = w.ask_or_fail("--agent", "Which agent CLI should answer?", || {
                inquire::Select::new("Which agent CLI should answer?", labels.clone()).raw_prompt()
            })?;
            let first = several[primary.index];
            let mut chain = vec![(
                okbot_agent::provider_name(first.provider).to_string(),
                Some(first.path.clone()),
            )];
            let rest: Vec<&&DetectedCli> = several
                .iter()
                .filter(|c| c.provider != first.provider)
                .collect();
            let fallback = inquire::Confirm::new(&format!(
                "Use {} as a fallback when {} fails?",
                okbot_agent::provider_name(rest[0].provider),
                chain[0].0
            ))
            .with_default(true)
            .prompt()
            .unwrap_or(false);
            if fallback {
                chain.push((
                    okbot_agent::provider_name(rest[0].provider).to_string(),
                    Some(rest[0].path.clone()),
                ));
            }
            Ok(chain)
        }
    }
}

/// The first free TCP port from 8787 on loopback.
fn free_listen() -> String {
    (8787..8887)
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .map_or_else(|| "127.0.0.1:8787".into(), |p| format!("127.0.0.1:{p}"))
}

/// Step 3: Zalo.
async fn zalo(
    w: &Wizard<'_>,
    dir: &Path,
    instance: &str,
) -> anyhow::Result<(ZaloInit, Option<String>)> {
    let env_file = dir.join(ENV_FILE);
    let token_env = match &w.args.zalo_token_env {
        Some(name) => name.clone(),
        None => {
            let token = w.ask_or_fail(
                "--zalo-token-env",
                "Paste the Zalo bot token (Zalo Bot Creator sent it to you)",
                || {
                    inquire::Password::new("Zalo bot token:")
                        .without_confirmation()
                        .with_help_message("from Zalo Bot Creator; saved to .env, never printed")
                        .prompt()
                },
            )?;
            let token = token.trim().to_string();
            if !token.contains(':') {
                bail!("that does not look like a Zalo bot token (expected `12345:abc…`)");
            }
            let name = fresh_env_name(w, dir, "ZALO_BOT_TOKEN", instance);
            upsert_env(&env_file, &name, &token)?;
            name
        }
    };
    if !w.args.no_verify {
        verify_zalo_token(dir, &token_env).await?;
    }
    let delivery = match w.args.delivery {
        Some(DeliveryArg::Polling) => Delivery::Polling,
        Some(DeliveryArg::Webhook) => Delivery::Webhook,
        None => {
            let choice = w.ask_or_fail(
                "--delivery",
                "How should Zalo deliver messages: polling (no public URL) or webhook (public \
                 HTTPS URL, recommended for production)",
                || {
                    inquire::Select::new(
                        "How should Zalo deliver messages?",
                        vec![
                            "polling — works without a public URL (good to start)",
                            "webhook — Zalo calls your HTTPS URL (recommended for production)",
                        ],
                    )
                    .raw_prompt()
                },
            )?;
            if choice.index == 0 {
                Delivery::Polling
            } else {
                Delivery::Webhook
            }
        }
    };
    if delivery == Delivery::Polling {
        return Ok((
            ZaloInit {
                instance: instance.into(),
                token_env,
                delivery,
                public_url: None,
                secret_env: None,
            },
            None,
        ));
    }
    let public_url = match &w.args.public_url {
        Some(url) => url.clone(),
        None => w.ask_or_fail(
            "--public-url",
            &format!("Public HTTPS URL that reaches this bot (for example https://bot.example.com/zalo/{instance})"),
            || inquire::Text::new("Public HTTPS URL for Zalo:").prompt(),
        )?,
    };
    if !public_url.starts_with("https://") {
        bail!("the webhook URL must start with https://");
    }
    let secret = hex::encode(rand::random::<[u8; 24]>());
    let secret_env = fresh_env_name(w, dir, "ZALO_WEBHOOK_SECRET", instance);
    upsert_env(&env_file, &secret_env, &secret)?;
    let listen = w.args.listen.clone().unwrap_or_else(free_listen);
    Ok((
        ZaloInit {
            instance: instance.into(),
            token_env,
            delivery,
            public_url: Some(public_url),
            secret_env: Some(secret_env),
        },
        Some(listen),
    ))
}

/// `base`, or `base_<INSTANCE>` when adding a channel and `.env` already holds `base` (a second
/// channel of the same platform must not overwrite the first one's secret).
fn fresh_env_name(w: &Wizard<'_>, dir: &Path, base: &str, instance: &str) -> String {
    let taken = w.adding
        && std::fs::read_to_string(dir.join(ENV_FILE))
            .is_ok_and(|text| text.lines().any(|l| l.starts_with(&format!("{base}="))));
    if taken {
        format!(
            "{base}_{}",
            instance.to_ascii_uppercase().replace(['-', '.'], "_")
        )
    } else {
        base.to_string()
    }
}

/// Adds `name` to the bot directory's `.gitignore`.
pub(crate) fn ensure_ignored(dir: &Path, name: &str) -> anyhow::Result<()> {
    let path = dir.join(".gitignore");
    let mut ignore = std::fs::read_to_string(&path).unwrap_or_default();
    if !ignore.lines().any(|l| l == name) {
        if !ignore.is_empty() && !ignore.ends_with('\n') {
            ignore.push('\n');
        }
        ignore.push_str(name);
        ignore.push('\n');
        std::fs::write(path, ignore)?;
    }
    Ok(())
}

/// A token from `--<flag>` (an environment variable name) or typed by the user and saved to
/// `.env` as `default_env`. `prefix` checks the token's shape.
fn token(
    w: &Wizard<'_>,
    dir: &Path,
    given: Option<&String>,
    flag: &'static str,
    question: &str,
    default_env: &str,
    prefix: &str,
) -> anyhow::Result<String> {
    if let Some(name) = given {
        return Ok(name.clone());
    }
    let value = w.ask_or_fail(flag, question, || {
        inquire::Password::new(&format!("{question}:"))
            .without_confirmation()
            .with_help_message("saved to .env, never printed")
            .prompt()
    })?;
    let value = value.trim();
    if !value.starts_with(prefix) {
        bail!("that does not look right (expected a token starting with `{prefix}`)");
    }
    upsert_env(&dir.join(ENV_FILE), default_env, value)?;
    Ok(default_env.to_string())
}

/// File name of the copied Google service-account key.
const GCHAT_KEY_FILE: &str = "gchat-service-account.json";

/// Step 3: Google Chat.
async fn gchat(
    w: &Wizard<'_>,
    dir: &Path,
    instance: &str,
) -> anyhow::Result<(GChatInit, Option<String>)> {
    let source = match &w.args.gchat_credentials {
        Some(path) => path.clone(),
        None => PathBuf::from(w.ask_or_fail(
            "--gchat-credentials",
            "Path to the Chat app's service-account key file (JSON)",
            || inquire::Text::new("Service-account key file (JSON):").prompt(),
        )?),
    };
    let text = std::fs::read_to_string(&source)
        .with_context(|| format!("cannot read {}", source.display()))?;
    if !w.args.no_verify {
        verify_gchat(&text).await?;
    }
    // A second Google Chat app in the same bot gets its own key file.
    let key_file = if dir.join(GCHAT_KEY_FILE).exists() {
        format!("gchat-{instance}-service-account.json")
    } else {
        GCHAT_KEY_FILE.to_string()
    };
    write_private(&dir.join(&key_file), &text)?;
    ensure_ignored(dir, &key_file)?;
    let delivery = match w.args.gchat_delivery {
        Some(GChatDeliveryArg::Pubsub) => GChatDelivery::Pubsub,
        Some(GChatDeliveryArg::Http) => GChatDelivery::Http,
        None => {
            let choice = w.ask_or_fail(
                "--gchat-delivery",
                "How should Google Chat deliver events: pubsub (no public URL) or http (public HTTPS URL)",
                || {
                    inquire::Select::new(
                        "How should Google Chat deliver events?",
                        vec![
                            "Pub/Sub — no public URL needed",
                            "HTTP endpoint — Google calls your HTTPS URL",
                        ],
                    )
                    .raw_prompt()
                },
            )?;
            if choice.index == 0 {
                GChatDelivery::Pubsub
            } else {
                GChatDelivery::Http
            }
        }
    };
    let mut init = GChatInit {
        instance: instance.into(),
        credentials: key_file.into(),
        delivery,
        subscription: None,
        audience_url: None,
    };
    match delivery {
        GChatDelivery::Pubsub => {
            let sub = match &w.args.gchat_subscription {
                Some(s) => s.clone(),
                None => w.ask_or_fail(
                    "--gchat-subscription",
                    "Pub/Sub subscription (projects/<project>/subscriptions/<name>)",
                    || inquire::Text::new("Pub/Sub subscription:").prompt(),
                )?,
            };
            if !sub.starts_with("projects/") {
                bail!("the subscription must look like projects/<project>/subscriptions/<name>");
            }
            init.subscription = Some(sub);
            Ok((init, None))
        }
        GChatDelivery::Http => {
            let url = match &w.args.public_url {
                Some(url) => url.clone(),
                None => w.ask_or_fail(
                    "--public-url",
                    &format!("Public HTTPS URL of the app's endpoint (for example https://bot.example.com/gchat/{instance})"),
                    || inquire::Text::new("Public HTTPS URL for Google Chat:").prompt(),
                )?,
            };
            if !url.starts_with("https://") {
                bail!("the endpoint URL must start with https://");
            }
            init.audience_url = Some(url);
            Ok((
                init,
                Some(w.args.listen.clone().unwrap_or_else(free_listen)),
            ))
        }
    }
}

#[cfg(feature = "gchat")]
async fn verify_gchat(key: &str) -> anyhow::Result<()> {
    let mut account = okbot_channel_gchat::ServiceAccount::from_json(key)?;
    if let Ok(base) = std::env::var("OKBOT_GCHAT_API_BASE") {
        account = account.with_token_uri(&format!("{base}/token"));
    }
    let email = account.client_email.clone();
    okbot_channel_gchat::TokenSource::new(account, reqwest::Client::new())
        .token()
        .await
        .context(
            "Google did not accept the service account (use --no-verify to skip this check)",
        )?;
    eprintln!("✓ Google service account {email}");
    Ok(())
}

#[cfg(not(feature = "gchat"))]
async fn verify_gchat(_: &str) -> anyhow::Result<()> {
    bail!("this okbot build has no Google Chat support")
}

/// Writes a secret file readable by its owner only.
fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(feature = "zalo")]
async fn verify_zalo_token(dir: &Path, token_env: &str) -> anyhow::Result<()> {
    let token = std::env::var(token_env).ok().or_else(|| {
        std::fs::read_to_string(dir.join(ENV_FILE))
            .ok()
            .and_then(|text| {
                text.lines().find_map(|l| {
                    l.strip_prefix(&format!("{token_env}="))
                        .map(|v| v.trim_matches(['"', '\'']).to_string())
                })
            })
    });
    let token = token.with_context(|| format!("{token_env} is not set"))?;
    let base = std::env::var("OKBOT_ZALO_API_BASE")
        .unwrap_or_else(|_| okbot_channel_zalo::DEFAULT_API_BASE.to_string());
    let api = okbot_channel_zalo::client(&base, &okbot_core::Secret::new(token));
    let me = api
        .get_me()
        .await
        .context("Zalo did not accept the token (use --no-verify to skip this check)")?;
    eprintln!("✓ Zalo bot `{}` ({})", me.account_name, me.account_type);
    Ok(())
}

#[cfg(not(feature = "zalo"))]
async fn verify_zalo_token(_: &Path, _: &str) -> anyhow::Result<()> {
    bail!("this okbot build has no Zalo support")
}

/// Step 3a: which platform.
fn choose_platform(w: &Wizard<'_>, given: Option<Platform>) -> anyhow::Result<Platform> {
    if let Some(p) = given {
        return Ok(p);
    }
    let options = vec![
        "Zalo Bot",
        "Google Chat",
        "Discord",
        "Slack",
        "none for now (try it with `okbot chat`)",
    ];
    let choice = w.ask_or_fail(
        "--platform",
        "Which messaging platform (zalo, gchat, discord, slack or none)",
        || inquire::Select::new("Which messaging platform?", options.clone()).raw_prompt(),
    )?;
    Ok(match choice.index {
        0 => Platform::Zalo,
        1 => Platform::Gchat,
        2 => Platform::Discord,
        3 => Platform::Slack,
        _ => Platform::None,
    })
}

/// Step 3b: the platform's credentials and delivery, written into `options`.
pub(crate) async fn setup_channel(
    w: &Wizard<'_>,
    dir: &Path,
    instance: &str,
    platform: Platform,
    options: &mut InitOptions,
) -> anyhow::Result<()> {
    let args = w.args;
    match platform {
        Platform::Zalo => {
            let (z, listen) = zalo(w, dir, instance).await?;
            eprintln!(
                "Note: under the Zalo Bot terms you are responsible for what the bot says \
                 (https://bot.zapps.me/docs/terms/)."
            );
            options.zalo = Some(z);
            options.http_listen = listen;
        }
        Platform::Gchat => {
            let (g, listen) = gchat(w, dir, instance).await?;
            options.gchat = Some(g);
            options.http_listen = listen;
        }
        Platform::Discord => {
            let env = token(
                w,
                dir,
                args.discord_token_env.as_ref(),
                "--discord-token-env",
                "Discord bot token (Developer Portal → Bot → Reset Token)",
                &fresh_env_name(w, dir, "DISCORD_TOKEN", instance),
                "",
            )?;
            options.discord = Some((instance.to_string(), env));
        }
        Platform::Slack => {
            let app = token(
                w,
                dir,
                args.slack_app_token_env.as_ref(),
                "--slack-app-token-env",
                "Slack app-level token with connections:write",
                &fresh_env_name(w, dir, "SLACK_APP_TOKEN", instance),
                "xapp-",
            )?;
            let bot = token(
                w,
                dir,
                args.slack_bot_token_env.as_ref(),
                "--slack-bot-token-env",
                "Slack bot token",
                &fresh_env_name(w, dir, "SLACK_BOT_TOKEN", instance),
                "xoxb-",
            )?;
            options.slack = Some((instance.to_string(), app, bot));
        }
        Platform::None => {}
    }
    Ok(())
}

/// The instance part of channel ids, from the bot name.
pub(crate) fn instance_of(name: &str) -> String {
    let instance: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if instance.is_empty() {
        "bot".into()
    } else {
        instance
    }
}

/// Runs the wizard in `dir` and returns the bot name.
pub(crate) async fn init(dir: &Path, args: &InitArgs) -> anyhow::Result<String> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let config_path = dir.join(CONFIG_FILE);
    if config_path.exists() && !args.force {
        bail!(
            "{} already exists; use --force to replace it",
            config_path.display()
        );
    }
    let interactive = !args.yes && !args.json && std::io::stdin().is_terminal();
    let w = Wizard {
        args: &args.channel,
        agents: &args.agent,
        interactive,
        adding: false,
    };
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| crate::registry::name_of(dir));
    let instance = instance_of(&name);
    let mut options = InitOptions {
        name: name.clone(),
        chain: choose_agents(&w).await?,
        timezone: args.timezone.clone(),
        ..InitOptions::default()
    };
    let platform = choose_platform(&w, args.platform)?;
    setup_channel(&w, dir, &instance, platform, &mut options).await?;
    write_bot(dir, &options, platform, args.json)?;
    Ok(name)
}

/// Writes the configuration, default knowledge and `.gitignore`, registers the bot and reports.
fn write_bot(
    dir: &Path,
    options: &InitOptions,
    platform: Platform,
    json: bool,
) -> anyhow::Result<()> {
    let name = &options.name;
    std::fs::write(dir.join(CONFIG_FILE), okbot_config::render_initial(options))?;
    let kb = dir.join("kb");
    let fresh = !kb.exists();
    let written = okbot_knowledge::write_default_bundle(&kb)?;
    if fresh {
        git_init(&kb);
    }
    let mut ignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
    for line in ["/state", ".env", GCHAT_KEY_FILE] {
        if !ignore.lines().any(|l| l == line) {
            ignore.push_str(line);
            ignore.push('\n');
        }
    }
    std::fs::write(dir.join(".gitignore"), ignore)?;
    okbot_config::Config::load(dir).context("the generated configuration does not load")?;
    crate::registry::register(name, dir)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "bot": name,
                "dir": std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()),
                "agents": options.chain.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "platform": format!("{platform:?}").to_lowercase(),
                "knowledge_files_written": written.len(),
                "next": ["okbot doctor", "okbot run"],
            })
        );
    } else {
        eprintln!(
            "✓ Created {CONFIG_FILE} and kb/ ({} files) in {}",
            written.len(),
            dir.display()
        );
        eprintln!(
            "Next:\n  okbot doctor {name}\n  okbot run {name}      # or `okbot chat` in {} to try it",
            dir.display()
        );
    }
    Ok(())
}

/// Makes a new `kb/` a git repository, so memory changes have history (`okbot memory log`).
/// Skipped silently when git is missing or `kb/` is already inside a repository.
fn git_init(kb: &Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(kb)
            .args(["-c", "user.name=okbot", "-c", "user.email=okbot@localhost"])
            .args(args)
            .output()
    };
    let inside = git(&["rev-parse", "--is-inside-work-tree"]).is_ok_and(|o| o.status.success());
    if inside {
        return;
    }
    let ok = git(&["init", "-q"]).is_ok_and(|o| o.status.success())
        && git(&["add", "-A"]).is_ok_and(|o| o.status.success())
        && git(&["commit", "-qm", "default knowledge"]).is_ok_and(|o| o.status.success());
    if !ok {
        tracing::debug!("git is not available; memory changes will have no history");
    }
}
