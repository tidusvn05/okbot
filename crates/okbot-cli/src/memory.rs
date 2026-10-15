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
    /// Open a note, task or journal file in $VISUAL or $EDITOR (okbot re-reads it by itself).
    Edit {
        /// Id, as `okbot memory list` or `okbot task list` shows it.
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
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

pub(crate) async fn open(name: Option<&str>) -> anyhow::Result<(Config, Arc<Knowledge>)> {
    let dir = crate::registry::resolve(name)?;
    let config = Config::load(&dir)?;
    let mut options = KnowledgeOptions::new(config.kb_dir());
    options.index_dir = Some(config.state_dir().join("index"));
    options.timezone.clone_from(&config.bot.timezone);
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
        MemoryCommand::Edit { id, bot } => {
            let dir = crate::registry::resolve(bot.as_deref())?;
            let config = Config::load(&dir)?;
            let id = id.trim().trim_start_matches('/').trim_end_matches(".md");
            let writable = ["memory/", "tasks/", "notes/", "knowledge/"]
                .iter()
                .any(|area| id.starts_with(area));
            anyhow::ensure!(
                writable && !id.split('/').any(|s| s.is_empty() || s == ".."),
                "`{id}` is not a memory, task, note or knowledge id"
            );
            let path = config.kb_dir().join(format!("{id}.md"));
            anyhow::ensure!(path.is_file(), "no file {}", path.display());
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into());
            let status = std::process::Command::new(&editor)
                .arg(&path)
                .status()
                .with_context(|| format!("cannot start the editor `{editor}`"))?;
            anyhow::ensure!(status.success(), "the editor exited with {status}");
        }
        MemoryCommand::Forget { id, bot } => {
            let (_, k) = open(bot.as_deref()).await?;
            k.remove_note(&id).await?;
            println!("removed {id}");
        }
    }
    Ok(())
}
