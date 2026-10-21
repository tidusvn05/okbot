//! `okbot role` and `okbot audit` (ADR 0011): temporary roles and the tool call record.

use std::time::Duration;

use anyhow::{Context, bail};
use okbot_config::Config;
use okbot_core::{ConversationStore, RoleGrant};

/// `okbot role` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum RoleCommand {
    /// The roles of `[roles]` with their members, and the temporary grants.
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Give a role from `[roles]` to an account for a while.
    Grant {
        /// Role name.
        role: String,
        /// Account (`discord:main/1234`, as `/whoami` shows it) or `person:<id>`.
        account: String,
        /// How long: `90m`, `2h`, `7d` (at most 30d).
        #[arg(long = "for", value_name = "DURATION")]
        duration: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// End a temporary grant.
    Revoke {
        /// Role name.
        role: String,
        /// Account or `person:<id>`.
        account: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
}

/// `okbot audit`.
#[derive(Debug, clap::Args)]
pub(crate) struct AuditArgs {
    /// Bot name; default: the bot in the current directory.
    name: Option<String>,
    /// How far back: `90m`, `24h`, `7d`.
    #[arg(long, default_value = "24h")]
    since: String,
    /// At most this many calls, newest first.
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Only calls for this person (`person:<id>` or an account).
    #[arg(long)]
    person: Option<String>,
    /// Print JSON lines.
    #[arg(long)]
    json: bool,
}

fn load(name: Option<&str>) -> anyhow::Result<Config> {
    let dir = crate::registry::resolve(name)?;
    Ok(Config::load(&dir)?)
}

/// A duration like `24h`, any length (for looking back).
fn lookback(text: &str) -> anyhow::Result<Duration> {
    let text = text.trim();
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    let n: u64 = number
        .parse()
        .with_context(|| format!("`{text}` is not a duration such as 90m, 24h or 7d"))?;
    let secs = match unit {
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("`{text}` is not a duration such as 90m, 24h or 7d"),
    };
    Ok(Duration::from_secs(secs))
}

fn local(config: &Config, at: jiff::Timestamp) -> String {
    let tz = config
        .bot
        .timezone
        .as_deref()
        .and_then(|z| jiff::tz::TimeZone::get(z).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    at.to_zoned(tz).strftime("%Y-%m-%d %H:%M").to_string()
}

pub(crate) async fn role(command: RoleCommand) -> anyhow::Result<()> {
    match command {
        RoleCommand::List { name } => {
            let config = load(name.as_deref())?;
            if config.roles.is_empty() {
                println!("no roles; define them in [roles] of okbot.toml");
            }
            for (role, members) in &config.roles {
                println!("{role}: {}", members.join(", "));
            }
            let grants = crate::owner::store(&config)
                .await?
                .role_grants(jiff::Timestamp::now())
                .await?;
            if !grants.is_empty() {
                println!("\ntemporary grants:");
            }
            for g in grants {
                println!(
                    "{}: {} until {} (by {})",
                    g.role,
                    g.account,
                    local(&config, g.expires_at),
                    g.granted_by
                );
            }
        }
        RoleCommand::Grant {
            role,
            account,
            duration,
            bot,
        } => {
            let config = load(bot.as_deref())?;
            if !config.roles.contains_key(&role) {
                bail!(
                    "`{role}` is not in [roles] of okbot.toml (admins and everyone cannot be granted)"
                );
            }
            if account.strip_prefix("person:").map_or_else(
                || okbot_config::parse_account(&account).is_err(),
                |id| !config.people.contains_key(id),
            ) {
                bail!(
                    "`{account}` must be an account such as `discord:main/1234` or `person:<id>` from [people]"
                );
            }
            let length = okbot_runtime::parse_duration(&duration).with_context(|| {
                format!("`{duration}` is not a duration from 1m to 30d (90m, 2h, 7d)")
            })?;
            let now = jiff::Timestamp::now();
            let expires_at = now + jiff::SignedDuration::try_from(length)?;
            crate::owner::store(&config)
                .await?
                .grant_role(RoleGrant {
                    role: role.clone(),
                    account: account.clone(),
                    granted_by: "operator".into(),
                    granted_at: now,
                    expires_at,
                })
                .await?;
            eprintln!(
                "✓ {account} has `{role}` until {} (the running bot sees it at once)",
                local(&config, expires_at)
            );
        }
        RoleCommand::Revoke { role, account, bot } => {
            let config = load(bot.as_deref())?;
            if !crate::owner::store(&config)
                .await?
                .revoke_role(&role, &account)
                .await?
            {
                bail!("{account} has no grant of `{role}`; `okbot role list` shows them");
            }
            eprintln!("✓ {account} no longer has `{role}`");
        }
    }
    Ok(())
}

pub(crate) async fn audit(args: AuditArgs) -> anyhow::Result<()> {
    let config = load(args.name.as_deref())?;
    let since = jiff::Timestamp::now() - jiff::SignedDuration::try_from(lookback(&args.since)?)?;
    let calls = crate::owner::store(&config)
        .await?
        .tool_calls(since, args.limit)
        .await?;
    let calls = calls
        .into_iter()
        .filter(|c| args.person.as_ref().is_none_or(|p| *p == c.person));
    let mut any = false;
    for c in calls {
        any = true;
        if args.json {
            let line = serde_json::json!({
                "at": c.at.to_string(),
                "person": c.person,
                "conversation": c.conversation,
                "server": c.server,
                "tool": c.tool,
                "decision": c.decision.as_str(),
                "error": c.error,
                "duration_ms": u64::try_from(c.duration.as_millis()).unwrap_or(u64::MAX),
                "run": c.run_id.as_ref().map(okbot_core::RunId::as_str),
                "command": c.command,
            });
            println!("{line}");
        } else {
            let via = c
                .command
                .as_ref()
                .map_or_else(|| "agent".to_string(), |n| format!("/{n}"));
            let outcome = match (&c.decision, &c.error) {
                (okbot_core::ToolDecision::Allowed, None) => "ok".to_string(),
                (okbot_core::ToolDecision::Allowed, Some(e)) => format!("failed: {e}"),
                (d, _) => d.as_str().to_string(),
            };
            println!(
                "{}  {:<24} {}.{:<20} {:<8} {} ({} ms)",
                local(&config, c.at),
                c.person,
                c.server,
                c.tool,
                via,
                outcome,
                c.duration.as_millis()
            );
        }
    }
    if !any && !args.json {
        eprintln!("no tool calls since {}", local(&config, since));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookbacks() {
        assert_eq!(lookback("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(lookback("90m").unwrap(), Duration::from_secs(5400));
        assert_eq!(lookback("60d").unwrap(), Duration::from_secs(60 * 86_400));
        assert!(lookback("2w").is_err());
        assert!(lookback("").is_err());
        assert!(lookback("h").is_err());
    }
}
