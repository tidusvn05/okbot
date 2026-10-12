//! `okbot kb`: knowledge bundle maintenance.

use anyhow::Context;
use okbot_config::Config;

use crate::exit::CliError;

/// `okbot kb` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum KbCommand {
    /// Re-index changed files.
    Sync {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Check the bundle against the okbase standard (level L2).
    Lint {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Add default knowledge files that are missing (never overwrites your files).
    Upgrade {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
}

fn open(name: Option<&str>) -> anyhow::Result<(Config, okbase::Bundle)> {
    let dir = crate::registry::resolve(name)?;
    let config = Config::load(&dir)?;
    let bundle = okbase::Bundle::open(
        &config.kb_dir(),
        okbase::OpenOptions {
            state_dir: okbase::StateDir::Path(config.state_dir().join("index")),
            ..okbase::OpenOptions::default()
        },
    )
    .with_context(|| format!("cannot open {}", config.kb_dir().display()))?;
    Ok((config, bundle))
}

pub(crate) fn run(command: KbCommand) -> anyhow::Result<()> {
    match command {
        KbCommand::Sync { name } => {
            let (_, bundle) = open(name.as_deref())?;
            let stats = bundle.sync()?;
            println!(
                "added {}, updated {}, removed {}",
                stats.added, stats.updated, stats.removed
            );
        }
        KbCommand::Lint { name } => {
            let (_, bundle) = open(name.as_deref())?;
            bundle.sync()?;
            let report = bundle.lint(&okbase::LintConfig::level(okbase::Level::L2))?;
            let json = serde_json::to_value(&report)?;
            if let Some(items) = json["diagnostics"].as_array() {
                for d in items {
                    println!(
                        "{:<7} {:<40} {} {}",
                        d["severity"].as_str().unwrap_or(""),
                        d["path"].as_str().unwrap_or(""),
                        d["code"].as_str().unwrap_or(""),
                        d["message"].as_str().unwrap_or("")
                    );
                }
            }
            println!("{} errors, {} warnings", json["errors"], json["warnings"]);
            if json["errors"].as_u64().unwrap_or(0) > 0 {
                return Err(CliError::findings("the knowledge bundle has lint errors").into());
            }
        }
        KbCommand::Upgrade { name } => {
            let dir = crate::registry::resolve(name.as_deref())?;
            let config = Config::load(&dir)?;
            let written = okbot_knowledge::write_default_bundle(&config.kb_dir())?;
            if written.is_empty() {
                println!("nothing to add: every default file exists");
            }
            for file in written {
                println!("added {}", file.display());
            }
        }
    }
    Ok(())
}
