//! Prompts on Google Chat: the message text with a card of buttons. Each button invokes the
//! action [`FUNCTION`] with the prompt and button ids as parameters; Chat reports the click as a
//! `CARD_CLICKED` event (or an add-on `buttonClickedPayload`).

use okbot_core::{Prompt, ReplyTarget, UserId};
use serde_json::{Value, json};

/// The action name of okbot's buttons.
pub(crate) const FUNCTION: &str = "okp";

/// A prompt waiting for a click.
pub(crate) struct Open {
    /// Who may answer; anyone when empty.
    pub(crate) allowed: Vec<UserId>,
    /// Its cards, to keep them when the text changes.
    pub(crate) cards: Value,
    /// Where it is.
    pub(crate) target: ReplyTarget,
    /// Its message name.
    pub(crate) name: String,
}

/// The `cardsV2` list with the prompt's buttons.
pub(crate) fn cards(prompt: &Prompt) -> Value {
    let buttons: Vec<Value> = prompt
        .buttons
        .iter()
        .map(|b| {
            json!({
                "text": b.label,
                "onClick": {"action": {
                    "function": FUNCTION,
                    "parameters": [
                        {"key": "prompt", "value": prompt.id},
                        {"key": "button", "value": b.id},
                    ],
                }},
            })
        })
        .collect();
    json!([{
        "cardId": format!("okp-{}", prompt.id),
        "card": {"sections": [{"widgets": [{"buttonList": {"buttons": buttons}}]}]},
    }])
}
