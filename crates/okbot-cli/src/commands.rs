//! `okbot commands`: the chat commands of a bot, and the built-in names `[[command]]` cannot use.

use anyhow::Context;
use okbot_config::{BUILTIN_COMMANDS, CONFIG_FILE, Config};
use serde_json::json;

/// `okbot commands` arguments.
#[derive(Debug, clap::Args)]
pub(crate) struct CommandsArgs {
    /// Only the built-in command names, which `[[command]]` cannot reuse (needs no bot).
    #[arg(long)]
    builtin: bool,
    /// Bot name; default: the bot in the current directory.
    #[arg(long)]
    bot: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

pub(crate) fn run(args: &CommandsArgs) -> anyhow::Result<()> {
    if args.builtin {
        if args.json {
            println!("{}", json!({ "builtin": BUILTIN_COMMANDS }));
        } else {
            for name in BUILTIN_COMMANDS {
                println!("{name}");
            }
        }
        return Ok(());
    }
    let dir = crate::registry::resolve(args.bot.as_deref())?;
    let config = Config::load(&dir)
        .with_context(|| format!("cannot load {}", dir.join(CONFIG_FILE).display()))?;
    if args.json {
        let own: Vec<_> = config
            .commands
            .iter()
            .map(|c| {
                json!({
                    "name": c.name,
                    "description": c.description,
                    "mcp": c.mcp,
                    "tool": c.tool,
                    "args": c.args,
                    "roles": c.roles,
                })
            })
            .collect();
        println!(
            "{}",
            json!({ "commands": own, "builtin": BUILTIN_COMMANDS })
        );
        return Ok(());
    }
    if config.commands.is_empty() {
        println!("no [[command]] in {}", dir.join(CONFIG_FILE).display());
    }
    for c in &config.commands {
        let args: Vec<String> = c.args.iter().map(|a| format!("<{a}>")).collect();
        println!("/{} {}  → {} {}", c.name, args.join(" "), c.mcp, c.tool);
        if !c.description.is_empty() {
            println!("    {}", c.description);
        }
    }
    println!(
        "\nbuilt in (a [[command]] cannot use these names):\n  {}",
        BUILTIN_COMMANDS.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_list_needs_no_bot() {
        let args = super::CommandsArgs {
            builtin: true,
            bot: Some("no-such-bot-anywhere".into()),
            json: true,
        };
        super::run(&args).unwrap();
    }
}
