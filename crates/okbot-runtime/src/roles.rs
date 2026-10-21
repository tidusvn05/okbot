//! `/roles`, `/grant` and `/revoke` (ADR 0011): temporary roles given in chat by administrators.
//!
//! Only roles defined in `[roles]` can be granted, never `admins` or `everyone`, and always for a
//! limited time, so a chat command cannot make anyone an administrator or grant forever.

use std::fmt::Write as _;
use std::time::Duration;

use okbot_core::{InboundEvent, RoleGrant};

use crate::Shared;
use crate::command::Command;
use crate::config::{ROLE_ADMINS, ROLE_EVERYONE};

/// The longest grant.
pub const MAX_GRANT: Duration = Duration::from_secs(30 * 24 * 3600);

/// Parses `90m`, `2h`, `7d` (at most [`MAX_GRANT`]).
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let unit = text.chars().last()?;
    let number: u64 = text[..text.len() - unit.len_utf8()].parse().ok()?;
    let secs = match unit {
        'm' => number.checked_mul(60)?,
        'h' => number.checked_mul(3600)?,
        'd' => number.checked_mul(86_400)?,
        _ => return None,
    };
    let d = Duration::from_secs(secs);
    (secs > 0 && d <= MAX_GRANT).then_some(d)
}

/// Whether `account` names an account (`platform:instance/user-id`) or a person (`person:<id>`).
fn valid_member(account: &str) -> bool {
    if let Some(id) = account.strip_prefix("person:") {
        return !id.is_empty();
    }
    account.split_once('/').is_some_and(|(channel, user)| {
        !user.is_empty() && channel.parse::<okbot_core::ChannelId>().is_ok()
    })
}

/// `HH:MM dd/mm` in the bot's time zone.
fn local_time(s: &Shared, at: jiff::Timestamp) -> String {
    at.to_zoned(s.config.time_zone())
        .strftime("%H:%M %d/%m")
        .to_string()
}

pub(crate) async fn execute(s: &Shared, command: Command, event: &InboundEvent) -> String {
    let admin = s.is_admin(event).await;
    match command {
        Command::Roles => roles(s, event, admin).await,
        Command::Grant(_) | Command::Revoke(_) if !admin => {
            "Only administrators can grant or revoke roles.".to_string()
        }
        Command::Grant(rest) => grant(s, event, &rest).await,
        Command::Revoke(rest) => revoke(s, event, &rest).await,
        _ => String::new(),
    }
}

async fn roles(s: &Shared, event: &InboundEvent, admin: bool) -> String {
    let mine: Vec<String> = s
        .roles_of(event)
        .await
        .into_iter()
        .filter(|r| r != ROLE_EVERYONE)
        .collect();
    let mut out = if mine.is_empty() {
        "You have no roles here.".to_string()
    } else {
        format!("Your roles: {}.", mine.join(", "))
    };
    if !admin {
        return out;
    }
    let defined: Vec<&str> = s.config.roles.names.iter().map(String::as_str).collect();
    if !defined.is_empty() {
        let _ = write!(out, "\nRoles of this assistant: {}.", defined.join(", "));
    }
    match s.store.role_grants(s.clock.now()).await {
        Ok(grants) if grants.is_empty() => {}
        Ok(grants) => {
            out.push_str("\nTemporary grants:");
            for g in grants {
                let _ = write!(
                    out,
                    "\n- `{}` → `{}` until {} (by {})",
                    g.role,
                    g.account,
                    local_time(s, g.expires_at),
                    g.granted_by
                );
            }
        }
        Err(error) => {
            tracing::error!(%error, "cannot read role grants");
            out.push_str("\nTemporary grants are unavailable.");
        }
    }
    out
}

async fn grant(s: &Shared, event: &InboundEvent, rest: &str) -> String {
    let usage = "Use `/grant <role> <account> <duration>`, for example \
                 `/grant ops discord:main/1234 2h` (accounts as `/whoami` shows them; at most 30d).";
    let words: Vec<&str> = rest.split_whitespace().collect();
    let [role, account, duration] = words.as_slice() else {
        return usage.to_string();
    };
    if *role == ROLE_ADMINS || *role == ROLE_EVERYONE {
        return format!("`{role}` cannot be granted.");
    }
    if !s.config.roles.names.contains(*role) {
        return format!("`{role}` is not a role in [roles].");
    }
    if !valid_member(account) {
        return usage.to_string();
    }
    let Some(duration) = parse_duration(duration) else {
        return usage.to_string();
    };
    let now = s.clock.now();
    let Ok(expires_at) = jiff::SignedDuration::try_from(duration).map(|d| now + d) else {
        return usage.to_string();
    };
    let by = format!("{}/{}", event.conversation.channel, event.author.id);
    let grant = RoleGrant {
        role: (*role).to_string(),
        account: (*account).to_string(),
        granted_by: by.clone(),
        granted_at: now,
        expires_at,
    };
    match s.store.grant_role(grant).await {
        Ok(()) => {
            tracing::info!(%role, %account, %by, ?duration, "role granted");
            format!(
                "✓ `{account}` has `{role}` until {}.",
                local_time(s, expires_at)
            )
        }
        Err(error) => {
            tracing::error!(%error, "cannot grant a role");
            s.config.replies.error.clone()
        }
    }
}

async fn revoke(s: &Shared, event: &InboundEvent, rest: &str) -> String {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let [role, account] = words.as_slice() else {
        return "Use `/revoke <role> <account>`.".to_string();
    };
    match s.store.revoke_role(role, account).await {
        Ok(true) => {
            tracing::info!(%role, %account, by = %event.author.id, "role revoked");
            format!("✓ `{account}` no longer has `{role}`.")
        }
        Ok(false) => format!("`{account}` has no grant of `{role}`."),
        Err(error) => {
            tracing::error!(%error, "cannot revoke a role");
            s.config.replies.error.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("30d"), Some(MAX_GRANT));
        assert_eq!(parse_duration("31d"), None, "too long");
        assert_eq!(parse_duration("0h"), None);
        assert_eq!(parse_duration("2"), None, "a unit is required");
        assert_eq!(parse_duration("h"), None);
        assert_eq!(parse_duration("2w"), None);
        assert_eq!(parse_duration("99999999999999999d"), None);
    }

    #[test]
    fn members() {
        assert!(valid_member("discord:main/1234"));
        assert!(valid_member("gchat:corp/users/42"));
        assert!(valid_member("person:lan"));
        assert!(!valid_member("person:"));
        assert!(!valid_member("1234"));
        assert!(!valid_member("discord/1234"));
        assert!(!valid_member("discord:main/"));
    }
}
