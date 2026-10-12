//! `okbot memory`: review and undo what bots remember.

use std::sync::Arc;

use anyhow::Context;
use okbot_config::Config;
use okbot_knowledge::{Knowledge, KnowledgeOptions};

/// `okbot memory` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum MemoryCommand {
    /// List every note (personal, chat and team).
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Show recent memory changes (git history of kb/memory).
    Log {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
        /// Number of changes.
        #[arg(long, short = 'n', default_value_t = 20)]
        count: u32,
    },
    /// Remove a note by id (operators may remove any note).
    Forget {
        /// Note id, as `okbot memory list` shows it.
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
}

async fn open(name: Option<&str>) -> anyhow::Result<(Config, Arc<Knowledge>)> {
    let dir = crate::registry::resolve(name)?;
    let config = Config::load(&dir)?;
    let mut options = KnowledgeOptions::new(config.kb_dir());
    options.index_dir = Some(config.state_dir().join("index"));
    let knowledge = tokio::task::spawn_blocking(move || Knowledge::open(options))
        .await?
        .with_context(|| format!("cannot open {}", config.kb_dir().display()))?;
    Ok((config, Arc::new(knowledge)))
}

pub(crate) async fn run(command: MemoryCommand) -> anyhow::Result<()> {
    match command {
        MemoryCommand::List { name } => {
            let (_, k) = open(name.as_deref()).await?;
            let notes = k.all_notes().await;
            if notes.is_empty() {
                println!("no notes");
            }
            for n in notes {
                println!(
                    "{}  [{:?}, {}, {}]\n    {}",
                    n.id,
                    n.scope,
                    n.updated,
                    n.author.as_deref().unwrap_or("-"),
                    n.text.replace('\n', " ")
                );
            }
        }
        MemoryCommand::Log { name, count } => {
            let (config, _) = open(name.as_deref()).await?;
            let kb = config.kb_dir();
            if !kb.join(".git").exists() {
                anyhow::bail!(
                    "{} is not a git repository, so there is no history; `okbot memory list` shows the notes",
                    kb.display()
                );
            }
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&kb)
                .args([
                    "log",
                    &format!("-{count}"),
                    "--format=%h %ad %s",
                    "--date=short",
                    "--",
                    "memory",
                ])
                .status()?;
            anyhow::ensure!(status.success(), "git log failed");
            println!("undo a change: git -C {} revert <hash>", kb.display());
        }
        MemoryCommand::Forget { id, bot } => {
            let (_, k) = open(bot.as_deref()).await?;
            k.remove_note(&id).await?;
            println!("removed {id}");
        }
    }
    Ok(())
}
