//! `okbot people`: link a person's accounts on several platforms so they share one profile and
//! personal memory.

use std::path::Path;

use anyhow::{Context, bail};
use okbot_config::{CONFIG_FILE, Config, ConfigEditor};
use serde_json::json;

/// `okbot people` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum PeopleCommand {
    /// List people and their linked accounts.
    List {
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print JSON (for scripts and MCP servers; the text output may change).
        #[arg(long)]
        json: bool,
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
        /// Print JSON (for scripts and MCP servers; the text output may change).
        #[arg(long)]
        json: bool,
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
        /// Print JSON (for scripts and MCP servers; the text output may change).
        #[arg(long)]
        json: bool,
    },
    /// Remove a person and all their links. Notes in `kb/memory/people/<id>/` are kept.
    Remove {
        /// Person id.
        person: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print JSON (for scripts and MCP servers; the text output may change).
        #[arg(long)]
        json: bool,
    },
}

impl PeopleCommand {
    /// Whether errors should be reported as JSON.
    pub(crate) fn json(&self) -> bool {
        match self {
            Self::List { json, .. }
            | Self::Link { json, .. }
            | Self::Unlink { json, .. }
            | Self::Remove { json, .. } => *json,
        }
    }
}

/// A person as JSON: `{"id", "accounts"}` (empty accounts once removed).
fn person_json(config: &Config, id: &str) -> serde_json::Value {
    let accounts = config
        .people
        .get(id)
        .map(|p| p.accounts.clone())
        .unwrap_or_default();
    json!({ "id": id, "accounts": accounts })
}

pub(crate) fn run(command: PeopleCommand) -> anyhow::Result<()> {
    match command {
        PeopleCommand::List { bot, json } => list(&crate::registry::resolve(bot.as_deref())?, json),
        PeopleCommand::Link {
            person,
            accounts,
            bot,
            json,
        } => link(
            &crate::registry::resolve(bot.as_deref())?,
            &person,
            &accounts,
            json,
        ),
        PeopleCommand::Unlink {
            person,
            accounts,
            bot,
            json,
        } => {
            let dir = crate::registry::resolve(bot.as_deref())?;
            let mut editor = ConfigEditor::open(&dir)?;
            let removed = editor.unlink_accounts(&person, &accounts);
            if removed.is_empty() {
                bail!("none of these accounts is linked to `{person}`");
            }
            let config = editor.save()?;
            if json {
                let mut out = person_json(&config, &person);
                out["unlinked"] = json!(removed);
                println!("{out}");
            } else {
                eprintln!("✓ Unlinked {} from `{person}`", removed.join(", "));
                applied_hint();
            }
            Ok(())
        }
        PeopleCommand::Remove { person, bot, json } => {
            let dir = crate::registry::resolve(bot.as_deref())?;
            let mut editor = ConfigEditor::open(&dir)?;
            if !editor.remove_person(&person) {
                bail!("no person `{person}`; `okbot people list` shows them");
            }
            editor.save()?;
            if json {
                println!("{}", json!({ "removed": person }));
            } else {
                eprintln!("✓ Removed `{person}`; notes in kb/memory/people/{person}/ are kept");
                applied_hint();
            }
            Ok(())
        }
    }
}

fn list(dir: &Path, json: bool) -> anyhow::Result<()> {
    let config = Config::load(dir)
        .with_context(|| format!("cannot load {}", dir.join(CONFIG_FILE).display()))?;
    if json {
        let people: Vec<_> = config
            .people
            .keys()
            .map(|id| person_json(&config, id))
            .collect();
        println!("{}", serde_json::Value::Array(people));
        return Ok(());
    }
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

fn link(dir: &Path, person: &str, accounts: &[String], json: bool) -> anyhow::Result<()> {
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
        if json {
            let mut out = person_json(&config, person);
            out["linked"] = json!([]);
            println!("{out}");
        } else {
            eprintln!("Nothing to do: already linked to `{person}`");
        }
        return Ok(());
    }
    let config = editor.save()?;
    if json {
        let mut out = person_json(&config, person);
        out["linked"] = json!(added);
        println!("{out}");
        return Ok(());
    }
    eprintln!(
        "✓ `{person}` now has {} account(s): {}",
        config.people[person].accounts.len(),
        config.people[person].accounts.join(", ")
    );
    eprintln!(
        "They share one profile and personal memory (kb/memory/people/{person}/); notes saved \
         before the link stay visible to all of them."
    );
    applied_hint();
    Ok(())
}

fn applied_hint() {
    eprintln!("A running bot applies this within seconds (no restart needed).");
}
