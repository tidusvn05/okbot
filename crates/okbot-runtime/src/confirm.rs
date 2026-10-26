//! Approval of an agent's tool call by a person (ADR 0011): `[[mcp]] confirm`.
//!
//! The gateway holds the call while the conversation shows the tool and its arguments with Allow
//! and Deny (or `/approve <code>` and `/deny <code>` where the platform has no buttons). No answer
//! before the turn ends is a refusal. The agent cannot answer: answers come from the platform as
//! a person's button press or command, never from the agent's output.

use std::time::Instant;

use okbot_core::{ButtonStyle, Prompt, PromptButton, ReplyTarget, UserId};
use serde_json::Value;

use crate::Shared;
use crate::config::{ConfirmBy, ExtraMcp};
use crate::gateway::Grant;
use crate::interact::{self, Kind, Pending};

/// Longest argument text shown. Longer arguments are not asked about: a person approves only what
/// they can read in full.
const MAX_ARGS: usize = 1500;

/// What became of a call that needs approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Approval {
    /// The person allowed it.
    Allowed,
    /// Declined, unanswered, or impossible to ask.
    Declined,
    /// The request is too long to show in full, so nobody was asked.
    Unreviewable,
}

/// The arguments as indented JSON, in full; `None` when longer than [`MAX_ARGS`].
fn show_arguments(arguments: &Value) -> Option<String> {
    let text = match arguments {
        Value::Null => return Some(String::new()),
        Value::Object(map) if map.is_empty() => return Some(String::new()),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    };
    if text.chars().count() > MAX_ARGS {
        return None;
    }
    // The text sits in a code block: it cannot close it.
    Some(format!("\n```\n{}\n```", text.replace("```", "'''")))
}

fn button(id: &str, label: &str, style: ButtonStyle) -> PromptButton {
    PromptButton {
        id: id.into(),
        label: label.into(),
        style,
        form: None,
        url: None,
    }
}

/// Who may approve: the requester, or the `[bot] admins` of the conversation's channel.
fn approvers(shared: &Shared, grant: &Grant, by: ConfirmBy) -> Vec<UserId> {
    match by {
        ConfirmBy::Requester => vec![grant.event.author.id.clone()],
        ConfirmBy::Admins => shared
            .config
            .access
            .admins
            .iter()
            .filter(|(c, _)| *c == grant.event.conversation.channel)
            .map(|(_, u)| u.clone())
            .collect(),
    }
}

/// Asks for approval of `tool` with `arguments` and waits for the answer.
pub(crate) async fn ask(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    tool: &str,
    arguments: &Value,
) -> Approval {
    let Some(shown) = show_arguments(arguments) else {
        tracing::warn!(mcp = %server.server.name, %tool, "arguments too long to review");
        return Approval::Unreviewable;
    };
    match ask_shown(shared, grant, server, tool, &shown).await {
        Some(true) => Approval::Allowed,
        Some(false) => Approval::Declined,
        None => Approval::Unreviewable,
    }
}

/// Shows the request with `shown` arguments; `None` when it does not fit one message.
async fn ask_shown(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    tool: &str,
    shown: &str,
) -> Option<bool> {
    // In an automation's turn the person it runs as decides (ADR 0013).
    let by = if grant.restricted {
        ConfirmBy::Requester
    } else {
        server.rules.confirm_by
    };
    let allowed = approvers(shared, grant, by);
    if allowed.is_empty() {
        tracing::warn!(mcp = %server.server.name, %tool, "no one on this channel can approve the call");
        return Some(false);
    }
    let who = match by {
        ConfirmBy::Requester => grant.event.author.display_name.clone(),
        ConfirmBy::Admins => "an administrator".to_string(),
    };
    let markdown = format!(
        "🔐 The assistant wants to use **{tool}** ({}) for {}:{}\nAllow it? ({who} decides)",
        server.server.name, grant.event.author.display_name, shown,
    );
    let Some(channel) = shared.channels.get(&grant.event.conversation.channel) else {
        return Some(false);
    };
    // The whole request must be in the one message the buttons are under.
    let mut parts = shared
        .renderer
        .render(&markdown, channel.caps())
        .into_iter();
    let (Some(text), None) = (parts.next(), parts.next()) else {
        tracing::warn!(mcp = %server.server.name, %tool, "the approval request does not fit one message");
        return None;
    };
    let prompt = Prompt {
        id: interact::short_id(),
        text: text.text,
        buttons: vec![
            button("allow", "Allow", ButtonStyle::Success),
            button("deny", "Deny", ButtonStyle::Danger),
        ],
        allowed: allowed.clone(),
        cards: Vec::new(),
        select: None,
    };
    let target = ReplyTarget {
        conversation: grant.event.conversation.clone(),
        in_reply_to: grant.event.message_id.clone(),
    };
    let (sent, text) = match interact::show(shared, &target, &prompt, &markdown).await {
        Ok(shown) => shown,
        Err(interact::Shown::NoForms | interact::Shown::Failed(_)) => {
            tracing::warn!(mcp = %server.server.name, %tool, "cannot ask for approval");
            return Some(false);
        }
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    shared.prompts.lock().insert(
        prompt.id.clone(),
        Pending {
            kind: Kind::Confirm(Some(tx)),
            markdown: markdown.clone(),
            sent: sent.clone(),
            expires: grant.expires,
            busy: false,
            allowed,
            buttons: vec!["allow".into(), "deny".into()],
            forms: Vec::new(),
            text,
        },
    );
    tracing::info!(prompt = %prompt.id, mcp = %server.server.name, %tool, "approval asked");
    let deadline = tokio::time::Instant::from_std(grant.expires.max(Instant::now()));
    let answer = tokio::select! {
        answer = rx => answer.ok(),
        () = tokio::time::sleep_until(deadline) => None,
        () = grant.done.cancelled() => None,
    };
    if let Some(allow) = answer {
        return Some(allow);
    }
    // Not answered: the prompt closes as expired, unless an answer just took it.
    let pending = shared.prompts.lock().remove(&prompt.id);
    if let Some(p) = pending {
        interact::update(
            shared,
            &p.sent,
            p.text,
            &p.markdown,
            "⌛ Not approved in time.",
            false,
        )
        .await;
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn arguments_are_shown_in_full_or_not_at_all() {
        assert_eq!(show_arguments(&Value::Null).unwrap(), "");
        assert_eq!(show_arguments(&json!({})).unwrap(), "");
        assert_eq!(
            show_arguments(&json!({"env": "prod"})).unwrap(),
            "\n```\n{\n  \"env\": \"prod\"\n}\n```"
        );
        // Never a shortened view: a hidden key could change what the call does.
        assert_eq!(
            show_arguments(&json!({"body": "a".repeat(2000), "to": "x@evil"})),
            None
        );
        let fence = show_arguments(&json!({"x": "```\nboom"})).unwrap();
        assert_eq!(fence.matches("```").count(), 2, "{fence}");
    }
}
