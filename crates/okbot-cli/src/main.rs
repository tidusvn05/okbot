//! The `okbot` command line: creates, runs and manages bots.

mod chat;
mod logging;

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
    /// Chat with the bot in this terminal.
    Chat(chat::ChatArgs),
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Chat(args) => chat::run(args).await,
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
