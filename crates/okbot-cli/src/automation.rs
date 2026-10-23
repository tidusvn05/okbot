//! `okbot automation` (ADR 0013): what each `[[automation]]` does and what it did lately.

use anyhow::bail;
use okbot_config::Config;
use okbot_core::ConversationStore;

/// `okbot automation` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum AutomationCommand {
    /// Each `[[automation]]` with its trigger, action and latest runs.
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
        /// Runs shown per automation, newest first.
        #[arg(long, default_value_t = 3)]
        runs: usize,
    },
}

fn describe(config: &Config, a: &okbot_config::AutomationSection) -> String {
    let trigger = match (&a.every, &a.schedule) {
        (Some(every), _) => format!("every {every}"),
        (None, Some(rule)) => rule.clone(),
        (None, None) => format!("POST /automation/{}", a.name),
    };
    let source = a
        .source
        .as_ref()
        .map(|s| format!(" · {} {}", s.mcp, s.tool))
        .unwrap_or_default();
    let action = if a.prompt.is_some() { "prompt" } else { "text" };
    let run_as = config
        .automation_run_as(a)
        .map(|r| format!(" · as {r}"))
        .unwrap_or_default();
    let paused = if a.paused { " · paused" } else { "" };
    format!("{trigger}{source} → {action}{run_as}{paused}")
}

pub(crate) async fn run(command: AutomationCommand) -> anyhow::Result<()> {
    let AutomationCommand::List { name, runs } = command;
    let dir = crate::registry::resolve(name.as_deref())?;
    let config = Config::load(&dir)?;
    if config.automations.is_empty() {
        bail!("no [[automation]] in okbot.toml; docs/automations.md shows examples");
    }
    let store = crate::owner::store(&config).await?;
    let tz = config
        .bot
        .timezone
        .as_deref()
        .and_then(|z| jiff::tz::TimeZone::get(z).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    for a in &config.automations {
        println!("{:<20} {}", a.name, describe(&config, a));
        let recent = store
            .automation_runs(Some(&a.name), jiff::Timestamp::UNIX_EPOCH, runs)
            .await?;
        if recent.is_empty() {
            println!("{:<20} no runs yet", "");
        }
        for r in recent {
            let when = r.at.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M");
            let error = r.error.map(|e| format!(" — {e}")).unwrap_or_default();
            println!(
                "{:<20} {when} {} ({} item(s)){error}",
                "", r.action, r.items
            );
        }
    }
    Ok(())
}
