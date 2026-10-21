//! Prompts on Slack: a section with the text and an actions block of buttons. Action ids carry
//! the prompt and the button, `okp:<prompt>:<button>`.

use okbot_core::{ButtonStyle, Prompt, ReplyTarget, UserId};
use serde_json::{Value, json};

const PREFIX: &str = "okp";

/// A prompt waiting for a press.
pub(crate) struct Open {
    /// Who may answer; anyone when empty.
    pub(crate) allowed: Vec<UserId>,
    /// The prompt, to show its buttons again when the text changes.
    pub(crate) prompt: Prompt,
    /// Where it is.
    pub(crate) target: ReplyTarget,
    /// Its message.
    pub(crate) ts: String,
}

/// The blocks of a message: `text`, and the buttons of `prompt` when given.
pub(crate) fn blocks(text: &str, prompt: Option<&Prompt>) -> Value {
    let mut blocks = vec![json!({"type": "section", "text": {"type": "mrkdwn", "text": text}})];
    if let Some(prompt) = prompt {
        let buttons: Vec<Value> = prompt
            .buttons
            .iter()
            .map(|b| {
                let mut button = json!({
                    "type": "button",
                    "text": {"type": "plain_text", "text": b.label},
                    "action_id": format!("{PREFIX}:{}:{}", prompt.id, b.id),
                    "value": b.id,
                });
                // Slack has two styles besides the default.
                let style = match b.style {
                    ButtonStyle::Primary | ButtonStyle::Success => Some("primary"),
                    ButtonStyle::Danger => Some("danger"),
                    _ => None,
                };
                if let Some(style) = style {
                    button["style"] = json!(style);
                }
                button
            })
            .collect();
        blocks.push(json!({"type": "actions", "elements": buttons}));
    }
    Value::Array(blocks)
}

/// `(prompt, button)` from an action id.
pub(crate) fn parse(action_id: &str) -> Option<(&str, &str)> {
    let rest = action_id.strip_prefix(PREFIX)?.strip_prefix(':')?;
    let (prompt, button) = rest.split_once(':')?;
    (!prompt.is_empty() && !button.is_empty()).then_some((prompt, button))
}

#[cfg(test)]
mod tests {
    use okbot_core::PromptButton;

    use super::*;

    #[test]
    fn blocks_and_action_ids() {
        let prompt = Prompt {
            id: "4f2a9c".into(),
            text: "Deploy?".into(),
            buttons: vec![
                PromptButton {
                    id: "allow".into(),
                    label: "Allow".into(),
                    style: ButtonStyle::Success,
                    form: None,
                },
                PromptButton {
                    id: "later".into(),
                    label: "Later".into(),
                    style: ButtonStyle::Secondary,
                    form: None,
                },
            ],
            allowed: vec![],
        };
        let b = blocks("*Deploy?*", Some(&prompt));
        assert_eq!(b[0]["text"]["text"], "*Deploy?*");
        assert_eq!(b[1]["elements"][0]["action_id"], "okp:4f2a9c:allow");
        assert_eq!(b[1]["elements"][0]["style"], "primary");
        assert!(b[1]["elements"][1].get("style").is_none());
        assert_eq!(blocks("done", None).as_array().unwrap().len(), 1);
        assert_eq!(parse("okp:4f2a9c:allow"), Some(("4f2a9c", "allow")));
        assert_eq!(parse("okp:4f2a9c"), None);
        assert_eq!(parse("x:a:b"), None);
    }
}
