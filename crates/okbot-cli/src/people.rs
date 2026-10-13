//! `okbot people`: link a person's accounts on several platforms so they share one profile and
//! personal memory.

use std::path::Path;

use anyhow::{Context, bail};
use okbot_config::{CONFIG_FILE, Config, ConfigEditor};

/// `okbot people` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum PeopleCommand {
    /// List people and their linked accounts.
    List {
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Link accounts to a person (created if new). Ask each account to send `/whoami` for its id.
    Link {
        /// Person id: lowercase letters, digits, `-`, `_` (`luan`).
        person: String,
        /// Accounts as `platform:instance/user-id` (`zalo:shop/6ede9afa…`).
        #[arg(required = true)]
        accounts: Vec<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Unlink accounts; the person is removed when no account is left. Notes are kept.
    Unlink {
        /// Person id.
        person: String,
        /// Accounts to unlink.
        #[arg(required = true)]
        accounts: Vec<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Remove a person and all their links. Notes in `kb/memory/people/<id>/` are kept.
    Remove {
        /// Person id.
        person: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
}

pub(crate) fn run(command: PeopleCommand) -> anyhow::Result<()> {
    match command {
        PeopleCommand::List { bot } => list(&crate::registry::resolve(bot.as_deref())?),
        PeopleCommand::Link {
            person,
            accounts,
            bot,
        } => link(
            &crate::registry::resolve(bot.as_deref())?,
            &person,
            &accounts,
        ),
        PeopleCommand::Unlink {
            person,
            accounts,
            bot,
        } => {
            let dir = crate::registry::resolve(bot.as_deref())?;
            let mut editor = ConfigEditor::open(&dir)?;
            let removed = editor.unlink_accounts(&person, &accounts);
            if removed.is_empty() {
                bail!("none of these accounts is linked to `{person}`");
            }
            editor.save()?;
            eprintln!("✓ Unlinked {} from `{person}`", removed.join(", "));
            restart_hint(&dir);
            Ok(())
        }
        PeopleCommand::Remove { person, bot } => {
            let dir = crate::registry::resolve(bot.as_deref())?;
            let mut editor = ConfigEditor::open(&dir)?;
            if !editor.remove_person(&person) {
                bail!("no person `{person}`; `okbot people list` shows them");
            }
            editor.save()?;
            eprintln!("✓ Removed `{person}`; notes in kb/memory/people/{person}/ are kept");
            restart_hint(&dir);
            Ok(())
        }
    }
}

fn list(dir: &Path) -> anyhow::Result<()> {
    let config = Config::load(dir)
        .with_context(|| format!("cannot load {}", dir.join(CONFIG_FILE).display()))?;
    if config.people.is_empty() {
        println!(
            "no people linked; `okbot people link <person> <account>…` shares memory across accounts"
        );
    }
    for (id, person) in &config.people {
        println!("{id}");
        for account in &person.accounts {
            println!("  {account}");
        }
    }
    Ok(())
}

fn link(dir: &Path, person: &str, accounts: &[String]) -> anyhow::Result<()> {
    let config = Config::load(dir)?;
    let configured: Vec<String> = config
        .channel_secrets()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    for account in accounts {
        let (channel, _) = okbot_config::parse_account(account).map_err(anyhow::Error::msg)?;
        if !configured.contains(&channel.to_string()) {
            eprintln!("Warning: {channel} is not a channel of this bot yet (`okbot channel list`)");
        }
    }
    let mut editor = ConfigEditor::open(dir)?;
    let added = editor.link_accounts(person, accounts);
    if added.is_empty() {
        eprintln!("Nothing to do: already linked to `{person}`");
        return Ok(());
    }
    let config = editor.save()?;
    eprintln!(
        "✓ `{person}` now has {} account(s): {}",
        config.people[person].accounts.len(),
        config.people[person].accounts.join(", ")
    );
    eprintln!(
        "They share one profile and personal memory (kb/memory/people/{person}/); notes saved \
         before the link stay visible to all of them."
    );
    restart_hint(dir);
    Ok(())
}

fn restart_hint(dir: &Path) {
    let name = crate::registry::name_of(dir);
    eprintln!("Restart the bot to apply: okbot restart {name}   (or restart `okbot run`)");
}
