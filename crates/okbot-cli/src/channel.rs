//! `okbot channel`: list, add and remove a bot's chat platforms.

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, bail};
use okbot_config::{CONFIG_FILE, Config, ConfigEditor, ENV_FILE, InitOptions};

use crate::init::{self, Platform, PlatformArgs, Wizard};

/// `okbot channel` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum ChannelCommand {
    /// List the bot's channels and whether their secrets are set.
    List {
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Add a platform to an existing bot (asks for its credentials, like `okbot init`).
    Add(Box<AddArgs>),
    /// Remove a channel (`discord:main`). Its secrets stay in .env unless --purge-secrets.
    Remove {
        /// Channel id, as `okbot channel list` shows it.
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Also delete the channel's variables from .env (when no other channel uses them).
        #[arg(long)]
        purge_secrets: bool,
    },
}

/// `okbot channel add`.
#[derive(Debug, clap::Args)]
pub(crate) struct AddArgs {
    /// Platform to add.
    #[arg(value_enum)]
    platform: Platform,
    /// Instance name, the part after `:` in the channel id (default: from the bot's name).
    #[arg(long)]
    instance: Option<String>,
    /// Bot name; default: the bot in the current directory.
    #[arg(long)]
    bot: Option<String>,
    #[command(flatten)]
    args: PlatformArgs,
    /// Never ask; fail with exit code 3 when an answer is missing.
    #[arg(long, short = 'y')]
    yes: bool,
}

pub(crate) async fn run(command: ChannelCommand) -> anyhow::Result<()> {
    match command {
        ChannelCommand::List { bot } => list(&crate::registry::resolve(bot.as_deref())?),
        ChannelCommand::Add(a) => {
            let dir = crate::registry::resolve(a.bot.as_deref())?;
            add(&dir, a.platform, a.instance.clone(), &a.args, a.yes).await
        }
        ChannelCommand::Remove {
            id,
            bot,
            purge_secrets,
        } => remove(
            &crate::registry::resolve(bot.as_deref())?,
            &id,
            purge_secrets,
        ),
    }
}

fn load(dir: &Path) -> anyhow::Result<Config> {
    Config::load(dir).with_context(|| format!("cannot load {}", dir.join(CONFIG_FILE).display()))
}

fn list(dir: &Path) -> anyhow::Result<()> {
    let config = load(dir)?;
    let channels = config.channel_secrets();
    if channels.is_empty() {
        println!("no channels; `okbot channel add <platform>` adds one");
        return Ok(());
    }
    for (id, kind, secrets) in channels {
        let missing: Vec<&str> = secrets
            .iter()
            .filter(|name| config.secret(name).is_err())
            .map(String::as_str)
            .collect();
        let status = if missing.is_empty() {
            "ready".to_string()
        } else {
            format!("missing {}", missing.join(", "))
        };
        let detail = detail(&config, &id);
        println!("{id:<24} {kind:<8} {status:<24} {detail}");
    }
    Ok(())
}

/// Delivery mode and the like, for `list`.
fn detail(config: &Config, id: &str) -> String {
    let Some((platform, instance)) = id.split_once(':') else {
        return String::new();
    };
    let c = &config.channels;
    match platform {
        "zalo" => c
            .zalo
            .get(instance)
            .map(|z| format!("{:?} delivery", z.delivery).to_lowercase())
            .unwrap_or_default(),
        "gchat" => c
            .gchat
            .get(instance)
            .map(|g| format!("{:?} delivery", g.delivery).to_lowercase())
            .unwrap_or_default(),
        "discord" | "slack" => String::new(),
        _ => c
            .bridge
            .get(instance)
            .map(|b| {
                format!(
                    "adapter {}",
                    b.command
                        .clone()
                        .unwrap_or_else(|| format!("okbot-channel-{}", b.platform))
                )
            })
            .unwrap_or_default(),
    }
}

fn platform_name(platform: Platform) -> anyhow::Result<&'static str> {
    Ok(match platform {
        Platform::Zalo => "zalo",
        Platform::Gchat => "gchat",
        Platform::Discord => "discord",
        Platform::Slack => "slack",
        Platform::None => bail!("choose a platform: zalo, gchat, discord or slack"),
    })
}

async fn add(
    dir: &Path,
    platform: Platform,
    instance: Option<String>,
    args: &PlatformArgs,
    yes: bool,
) -> anyhow::Result<()> {
    let config = load(dir)?;
    let name = platform_name(platform)?;
    let instance = instance.unwrap_or_else(|| init::instance_of(&config.bot.name));
    let id = okbot_core::ChannelId::new(name, &instance)?.to_string();
    if config.channel_secrets().iter().any(|(c, _, _)| *c == id) {
        bail!("{id} already exists; pick another --instance, or `okbot channel remove {id}` first");
    }

    let w = Wizard {
        args,
        agents: &[],
        interactive: !yes && std::io::stdin().is_terminal(),
        adding: true,
    };
    let mut options = InitOptions::default();
    init::setup_channel(&w, dir, &instance, platform, &mut options).await?;

    let mut editor = ConfigEditor::open(dir)?;
    let mut text = String::new();
    if let Some(listen) = &options.http_listen
        && !editor.has_http_listen()
    {
        text.push_str(&okbot_config::render_http(listen));
    }
    text.push_str(&okbot_config::render_channels(&options));
    editor.append(&text)?;
    editor
        .save()
        .with_context(|| format!("{id} was not added: the result does not validate"))?;

    eprintln!("✓ Added {id} to {}", dir.join(CONFIG_FILE).display());
    next_steps(dir);
    Ok(())
}

fn remove(dir: &Path, id: &str, purge_secrets: bool) -> anyhow::Result<()> {
    let config = load(dir)?;
    let channels = config.channel_secrets();
    let Some((_, kind, secrets)) = channels.iter().find(|(c, _, _)| c == id) else {
        bail!("no channel {id}; `okbot channel list` shows them");
    };
    let instance = id.split_once(':').map_or(id, |(_, i)| i);
    let mut editor = ConfigEditor::open(dir)?;
    editor.remove_channel(kind, instance);
    let config = editor.save()?;
    eprintln!("✓ Removed {id}");

    let prefix = format!("{id}/");
    let stale: Vec<&String> = config
        .bot
        .admins
        .iter()
        .chain(config.people.values().flat_map(|p| p.accounts.iter()))
        .filter(|a| a.starts_with(&prefix))
        .collect();
    if !stale.is_empty() {
        eprintln!(
            "Note: these accounts in [bot] admins or [people] belong to {id} and no longer match: {}",
            stale
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if purge_secrets {
        let still_used: Vec<String> = config
            .channel_secrets()
            .into_iter()
            .flat_map(|(_, _, s)| s)
            .collect();
        for name in secrets.iter().filter(|n| !still_used.contains(n)) {
            if okbot_config::remove_env(&dir.join(ENV_FILE), name)? {
                eprintln!("✓ Deleted {name} from {ENV_FILE}");
            }
        }
    } else if !secrets.is_empty() {
        eprintln!(
            "Its variables ({}) stay in {ENV_FILE}; --purge-secrets deletes them.",
            secrets.join(", ")
        );
    }
    next_steps(dir);
    Ok(())
}

fn next_steps(dir: &Path) {
    let name = crate::registry::name_of(dir);
    eprintln!(
        "Next:\n  okbot doctor {name}\n  okbot restart {name}   # if it runs as a service; or restart `okbot run`"
    );
}
