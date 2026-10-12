//! `okbot init` and `okbot new`: the setup wizard (design §6.6).
//!
//! Every question has a flag. With a terminal the wizard asks for what is missing; without one
//! (or with `--yes` / `--json`) a missing answer ends with exit code 3 and names the flag, so a
//! coding agent can relay the question to its user.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use okbot_agent::detect::{self, DetectedCli, Login};
use okbot_config::{CONFIG_FILE, Delivery, ENV_FILE, InitOptions, ZaloInit, upsert_env};

use crate::exit::CliError;

/// Messaging platform for the new bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Platform {
    /// Zalo Bot Platform.
    Zalo,
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
    /// HTTP listen address for webhooks (default: the first free port from 127.0.0.1:8787).
    #[arg(long)]
    listen: Option<String>,
    /// IANA timezone shown to the agent (`Asia/Ho_Chi_Minh`).
    #[arg(long)]
    timezone: Option<String>,
    /// Skip checking the Zalo token with the Zalo API.
    #[arg(long)]
    no_verify: bool,
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

struct Wizard<'a> {
    args: &'a InitArgs,
    interactive: bool,
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
    if !w.args.agent.is_empty() {
        for name in &w.args.agent {
            if okbot_agent::parse_provider(name).is_none() {
                bail!("unknown agent `{name}` (use claude, codex, opencode or devin)");
            }
        }
        return Ok(w
            .args
            .agent
            .iter()
            .map(|n| (n.clone(), path_of(n)))
            .collect());
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
            upsert_env(&env_file, "ZALO_BOT_TOKEN", &token)?;
            "ZALO_BOT_TOKEN".into()
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
    upsert_env(&env_file, "ZALO_WEBHOOK_SECRET", &secret)?;
    let listen = w.args.listen.clone().unwrap_or_else(free_listen);
    Ok((
        ZaloInit {
            instance: instance.into(),
            token_env,
            delivery,
            public_url: Some(public_url),
            secret_env: Some("ZALO_WEBHOOK_SECRET".into()),
        },
        Some(listen),
    ))
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
    let api = okbot_channel_zalo::ZaloApi::new(&base, okbot_core::Secret::new(token));
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
    let w = Wizard { args, interactive };
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| crate::registry::name_of(dir));
    let instance: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let instance = if instance.is_empty() {
        "bot".into()
    } else {
        instance
    };

    let chain = choose_agents(&w).await?;
    let platform = match args.platform {
        Some(p) => p,
        None => {
            let choice = w.ask_or_fail(
                "--platform",
                "Which messaging platform (zalo or none)",
                || {
                    inquire::Select::new(
                        "Which messaging platform?",
                        vec!["Zalo Bot", "none for now (try it with `okbot chat`)"],
                    )
                    .raw_prompt()
                },
            )?;
            if choice.index == 0 {
                Platform::Zalo
            } else {
                Platform::None
            }
        }
    };
    let (zalo, http_listen) = match platform {
        Platform::Zalo => {
            let (z, listen) = zalo(&w, dir, &instance).await?;
            eprintln!(
                "Note: under the Zalo Bot terms you are responsible for what the bot says \
                 (https://bot.zapps.me/docs/terms/)."
            );
            (Some(z), listen)
        }
        Platform::None => (None, None),
    };

    let options = InitOptions {
        name: name.clone(),
        chain,
        zalo,
        http_listen,
        timezone: args.timezone.clone(),
    };
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
    let written = okbot_knowledge::write_default_bundle(&dir.join("kb"))?;
    let mut ignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
    for line in ["/state", ".env"] {
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
