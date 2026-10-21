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

/// Longest argument text shown.
const MAX_ARGS: usize = 800;

/// The arguments as indented JSON, shortened.
fn show_arguments(arguments: &Value) -> String {
    let text = match arguments {
        Value::Null => return String::new(),
        Value::Object(map) if map.is_empty() => return String::new(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    };
    let mut out: String = text.chars().take(MAX_ARGS).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    // The text sits in a code block: it cannot close it.
    format!("\n```\n{}\n```", out.replace("```", "'''"))
}

fn button(id: &str, label: &str, style: ButtonStyle) -> PromptButton {
    PromptButton {
        id: id.into(),
        label: label.into(),
        style,
        form: None,
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

/// Asks for approval of `tool` with `arguments` and waits for the answer. `false` when declined,
/// unanswered before the turn ends, or impossible to ask.
pub(crate) async fn ask(
    shared: &Shared,
    grant: &Grant,
    server: &ExtraMcp,
    tool: &str,
    arguments: &Value,
) -> bool {
    let allowed = approvers(shared, grant, server.rules.confirm_by);
    if allowed.is_empty() {
        tracing::warn!(mcp = %server.server.name, %tool, "no one on this channel can approve the call");
        return false;
    }
    let who = match server.rules.confirm_by {
        ConfirmBy::Requester => grant.event.author.display_name.clone(),
        ConfirmBy::Admins => "an administrator".to_string(),
    };
    let markdown = format!(
        "🔐 The assistant wants to use **{tool}** ({}) for {}:{}\nAllow it? ({who} decides)",
        server.server.name,
        grant.event.author.display_name,
        show_arguments(arguments),
    );
    let Some(channel) = shared.channels.get(&grant.event.conversation.channel) else {
        return false;
    };
    let Some(text) = shared
        .renderer
        .render(&markdown, channel.caps())
        .into_iter()
        .next()
    else {
        return false;
    };
    let prompt = Prompt {
        id: interact::short_id(),
        text: text.text,
        buttons: vec![
            button("allow", "Allow", ButtonStyle::Success),
            button("deny", "Deny", ButtonStyle::Danger),
        ],
        allowed: allowed.clone(),
    };
    let target = ReplyTarget {
        conversation: grant.event.conversation.clone(),
        in_reply_to: grant.event.message_id.clone(),
    };
    let (sent, text) = match interact::show(shared, &target, &prompt, &markdown).await {
        Ok(shown) => shown,
        Err(interact::Shown::NoForms | interact::Shown::Failed(_)) => {
            tracing::warn!(mcp = %server.server.name, %tool, "cannot ask for approval");
            return false;
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
        return allow;
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
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn arguments_are_shown_short_and_fenced() {
        assert_eq!(show_arguments(&Value::Null), "");
        assert_eq!(show_arguments(&json!({})), "");
        assert_eq!(
            show_arguments(&json!({"env": "prod"})),
            "\n```\n{\n  \"env\": \"prod\"\n}\n```"
        );
        let long = show_arguments(&json!({"x": "a".repeat(2000)}));
        assert!(long.chars().count() < MAX_ARGS + 20, "{}", long.len());
        assert!(long.contains('…'));
        let fence = show_arguments(&json!({"x": "```\nboom"}));
        assert_eq!(fence.matches("```").count(), 2, "{fence}");
    }
}
