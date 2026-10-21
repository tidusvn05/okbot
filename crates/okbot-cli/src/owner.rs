//! `okbot profile` and `okbot pair`: a personal assistant and its owner's accounts.

use std::time::Duration;

use anyhow::{Context, bail};
use okbot_config::{BotProfile, Config, ConfigEditor};
use okbot_core::ConversationStore;
use okbot_store::SqliteStore;

use crate::init::ProfileArg;

/// How long a pairing code works.
const CODE_TTL: Duration = Duration::from_secs(600);

/// `okbot profile`.
#[derive(Debug, clap::Args)]
pub(crate) struct ProfileArgs {
    /// New profile; without it the current one is shown.
    #[arg(value_enum)]
    profile: Option<ProfileArg>,
    /// Bot name; default: the bot in the current directory.
    #[arg(long)]
    bot: Option<String>,
}

/// `okbot pair`.
#[derive(Debug, clap::Args)]
pub(crate) struct PairArgs {
    /// Bot name; default: the bot in the current directory.
    bot: Option<String>,
    /// List the paired accounts instead of creating a code.
    #[arg(long, conflicts_with = "remove")]
    list: bool,
    /// Unpair an account (`zalo:shop/6ede…`).
    #[arg(long, value_name = "ACCOUNT")]
    remove: Option<String>,
}

pub(crate) fn profile(args: &ProfileArgs) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(args.bot.as_deref())?;
    let name = crate::registry::name_of(&dir);
    let current = Config::load(&dir)?.bot.profile;
    let Some(wanted) = args.profile else {
        println!("{}", describe(current));
        return Ok(());
    };
    let wanted = match wanted {
        ProfileArg::Personal => BotProfile::Personal,
        ProfileArg::Team => BotProfile::Team,
    };
    if wanted == current {
        println!("{}", describe(current));
        return Ok(());
    }
    let mut editor = ConfigEditor::open(&dir)?;
    editor.set_profile(wanted);
    editor.save()?;
    eprintln!("✓ {}", describe(wanted));
    match wanted {
        BotProfile::Personal => eprintln!(
            "Next:\n  okbot restart {name}   # or restart `okbot run`\n  okbot pair {name}      # then send `/pair <code>` to the bot on each platform\nUntil an account is paired the bot answers nobody."
        ),
        BotProfile::Team => eprintln!("Next:\n  okbot restart {name}   # or restart `okbot run`"),
    }
    Ok(())
}

fn describe(profile: BotProfile) -> &'static str {
    match profile {
        BotProfile::Personal => {
            "personal: answers only your paired accounts, one memory on every platform"
        }
        BotProfile::Team => {
            "team: everyone allowed can talk to it; each person has their own memory"
        }
    }
}

pub(crate) async fn store(config: &Config) -> anyhow::Result<SqliteStore> {
    let state = config.state_dir();
    std::fs::create_dir_all(&state)
        .with_context(|| format!("cannot create {}", state.display()))?;
    let path = state.join("okbot.db");
    tokio::task::spawn_blocking(move || SqliteStore::open(&path))
        .await?
        .context("cannot open the bot's database")
}

pub(crate) async fn pair(args: &PairArgs) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(args.bot.as_deref())?;
    let name = crate::registry::name_of(&dir);
    let config = Config::load(&dir)?;
    if config.bot.profile != BotProfile::Personal {
        bail!(
            "`{name}` is a team bot; pairing is for personal assistants (`okbot profile personal --bot {name}`), \
             or link accounts with `okbot people link`"
        );
    }
    let store = store(&config).await?;
    if args.list {
        let owners = store.owners().await?;
        if owners.is_empty() {
            println!("no paired accounts; `okbot pair {name}` creates a code");
        }
        for (channel, user) in owners {
            println!("{channel}/{user}");
        }
        return Ok(());
    }
    if let Some(account) = &args.remove {
        let (channel, user) = okbot_config::parse_account(account).map_err(anyhow::Error::msg)?;
        if !store.remove_owner(&channel, &user).await? {
            bail!("{account} is not paired; `okbot pair {name} --list` shows the paired accounts");
        }
        eprintln!("✓ Unpaired {account}");
        return Ok(());
    }
    let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
    store.create_pairing_code(&code, CODE_TTL).await?;
    println!(
        "Send this to your bot in a direct message, on any platform, within 10 minutes:\n\n  /pair {code}\n"
    );
    eprintln!(
        "The code works once. The bot must be running (`okbot run` or `okbot start {name}`)."
    );
    Ok(())
}
