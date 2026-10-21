//! Prompts on Telegram: an inline keyboard under the message. Callback data carries the prompt
//! and the button, `okp:<prompt>:<button>` (at most 64 bytes, which prompt and button ids keep
//! to). Telegram has no forms, so buttons with one are not shown here.

use okbot_core::{Prompt, ReplyTarget, UserId};
use serde_json::{Value, json};

const PREFIX: &str = "okp";

/// A prompt waiting for a press.
pub(crate) struct Open {
    /// Who may answer; anyone when empty.
    pub(crate) allowed: Vec<UserId>,
    /// Its keyboard, to keep it when the text changes.
    pub(crate) markup: Value,
    /// Where it is.
    pub(crate) target: ReplyTarget,
    /// Its message.
    pub(crate) message_id: i64,
}

/// The inline keyboard: one row of buttons.
pub(crate) fn markup(prompt: &Prompt) -> Value {
    let row: Vec<Value> = prompt
        .buttons
        .iter()
        .map(|b| json!({"text": b.label, "callback_data": format!("{PREFIX}:{}:{}", prompt.id, b.id)}))
        .collect();
    json!({ "inline_keyboard": [row] })
}

/// A keyboard without buttons (removes them).
pub(crate) fn no_buttons() -> Value {
    json!({ "inline_keyboard": [] })
}

/// `(prompt, button)` from callback data.
pub(crate) fn parse(data: &str) -> Option<(&str, &str)> {
    let rest = data.strip_prefix(PREFIX)?.strip_prefix(':')?;
    let (prompt, button) = rest.split_once(':')?;
    (!prompt.is_empty() && !button.is_empty()).then_some((prompt, button))
}

#[cfg(test)]
mod tests {
    use okbot_core::{ButtonStyle, PromptButton};

    use super::*;

    #[test]
    fn keyboard_and_callback_data() {
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
                    id: "deny".into(),
                    label: "Deny".into(),
                    style: ButtonStyle::Danger,
                    form: None,
                },
            ],
            allowed: vec![],
        };
        let m = markup(&prompt);
        assert_eq!(m["inline_keyboard"][0][1]["text"], "Deny");
        let data = m["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap();
        assert_eq!(data, "okp:4f2a9c:allow");
        assert!(data.len() <= 64);
        assert_eq!(parse(data), Some(("4f2a9c", "allow")));
        assert_eq!(parse("okp:x"), None);
        assert_eq!(parse("other:a:b"), None);
        assert_eq!(parse("okp::b"), None);
    }
}
