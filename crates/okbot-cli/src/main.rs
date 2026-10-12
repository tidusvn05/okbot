//! The `okbot` command line: creates, runs and manages bots.

mod bot;
mod chat;
mod doctor;
mod exit;
mod init;
mod kb;
mod logging;
mod registry;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Chat assistants backed by your agent CLI and an okbase knowledge bundle.
#[derive(Debug, Parser)]
#[command(name = "okbot", version, about, propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a bot in a new directory and run the setup wizard.
    New {
        /// Bot name (also the registry name).
        #[arg(value_name = "NAME")]
        bot: String,
        /// Directory (default: ./NAME).
        dir: Option<PathBuf>,
        #[command(flatten)]
        args: init::InitArgs,
    },
    /// Run the setup wizard in the current directory.
    Init(init::InitArgs),
    /// Run a bot in the foreground.
    Run(run::RunArgs),
    /// Chat with the bot in this terminal.
    Chat(chat::ChatArgs),
    /// List the bots on this machine.
    List,
    /// Check a bot's configuration, agents, knowledge and channels.
    Doctor(doctor::DoctorArgs),
    /// Maintain the knowledge bundle.
    #[command(subcommand)]
    Kb(kb::KbCommand),
    /// Forget a bot (its directory is kept).
    Remove {
        /// Bot name.
        name: String,
    },
    /// Print the contract for coding agents.
    #[command(name = "help-agent", alias = "agent-help")]
    HelpAgent,
}

fn list() -> anyhow::Result<()> {
    let bots = registry::all()?;
    if bots.is_empty() {
        println!("no bots yet: `okbot new <name>` creates one");
    }
    for (name, dir) in bots {
        let detail = match okbot_config::Config::load(&dir) {
            Ok(c) => {
                let channels: Vec<String> = c
                    .channels
                    .zalo
                    .keys()
                    .map(|i| format!("zalo:{i}"))
                    .collect();
                format!(
                    "agents {}; channels {}",
                    c.agent.chain.join(","),
                    if channels.is_empty() {
                        "none".into()
                    } else {
                        channels.join(",")
                    }
                )
            }
            Err(e) => format!("invalid: {e}"),
        };
        println!("{name:<16} {}  ({detail})", dir.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = match &cli.command {
        Command::New { args, .. } | Command::Init(args) => args.json,
        Command::Doctor(args) => args.json,
        _ => false,
    };
    let result = match cli.command {
        Command::New { bot, dir, mut args } => {
            let dir = dir.unwrap_or_else(|| PathBuf::from(&bot));
            args.name = Some(bot);
            init::init(&dir, &args).await.map(drop)
        }
        Command::Init(args) => match std::env::current_dir() {
            Ok(dir) => init::init(&dir, &args).await.map(drop),
            Err(e) => Err(e.into()),
        },
        Command::Run(args) => run::run(args).await,
        Command::Chat(args) => chat::run(args).await,
        Command::List => list(),
        Command::Doctor(args) => doctor::run(args).await,
        Command::Kb(command) => kb::run(command),
        Command::Remove { name } => registry::unregister(&name).map(|removed| {
            if removed {
                println!("forgot `{name}` (its directory is kept)");
            } else {
                println!("no bot named `{name}`");
            }
        }),
        Command::HelpAgent => {
            print!("{}", include_str!("agent_contract.md"));
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => exit::report(&error, json),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_definition_is_valid() {
        <super::Cli as clap::CommandFactory>::command().debug_assert();
    }
}
