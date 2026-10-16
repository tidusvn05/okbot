//! Prompts on Discord: buttons under a message, and a modal for buttons with a form.
//!
//! Custom ids carry the prompt and button: `okp:<prompt>:<button>` on buttons and
//! `okf:<prompt>:<button>` on the modal, so a click needs no other lookup than the prompt itself.

use okbot_core::{ButtonStyle, FieldStyle, Form, Prompt, Secret};
use std::collections::BTreeMap;
use twilight_model::application::interaction::modal::{
    ModalInteractionComponent, ModalInteractionData,
};
use twilight_model::channel::message::Component;
use twilight_model::channel::message::component::{
    ActionRow, Button, ButtonStyle as DiscordStyle, Label, TextInput, TextInputStyle,
};
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};

const BUTTON: &str = "okp";
const MODAL: &str = "okf";

/// The custom id of a button.
pub(crate) fn button_id(prompt: &str, button: &str) -> String {
    format!("{BUTTON}:{prompt}:{button}")
}

/// `(prompt, button)` from a button's (`modal == false`) or a modal's custom id.
pub(crate) fn parse_id(custom_id: &str, modal: bool) -> Option<(&str, &str)> {
    let rest = custom_id.strip_prefix(if modal { MODAL } else { BUTTON })?;
    let (prompt, button) = rest.strip_prefix(':')?.split_once(':')?;
    (!prompt.is_empty() && !button.is_empty()).then_some((prompt, button))
}

fn style(s: ButtonStyle) -> DiscordStyle {
    match s {
        ButtonStyle::Secondary => DiscordStyle::Secondary,
        ButtonStyle::Success => DiscordStyle::Success,
        ButtonStyle::Danger => DiscordStyle::Danger,
        _ => DiscordStyle::Primary,
    }
}

/// One action row with the prompt's buttons.
pub(crate) fn components(prompt: &Prompt) -> Vec<Component> {
    let buttons = prompt
        .buttons
        .iter()
        .map(|b| {
            Component::Button(Button {
                id: None,
                custom_id: Some(button_id(&prompt.id, &b.id)),
                disabled: false,
                emoji: None,
                label: Some(b.label.clone()),
                style: style(b.style),
                url: None,
                sku_id: None,
            })
        })
        .collect();
    vec![Component::ActionRow(ActionRow {
        id: None,
        components: buttons,
    })]
}

/// The response that opens `form` for the button `button` of `prompt`.
#[allow(
    deprecated,
    reason = "TextInput::label must be None inside a Label, but the struct literal still names it"
)]
pub(crate) fn modal(prompt: &str, button: &str, form: &Form) -> InteractionResponse {
    let fields = form
        .fields
        .iter()
        .map(|f| {
            Component::Label(Label {
                id: None,
                label: f.label.clone(),
                description: None,
                component: Box::new(Component::TextInput(TextInput {
                    id: None,
                    custom_id: f.id.clone(),
                    label: None,
                    max_length: f.max_len,
                    min_length: f.min_len,
                    placeholder: f.placeholder.clone(),
                    required: Some(f.required),
                    style: match f.style {
                        FieldStyle::Paragraph => TextInputStyle::Paragraph,
                        _ => TextInputStyle::Short,
                    },
                    value: None,
                })),
            })
        })
        .collect();
    InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(InteractionResponseData {
            custom_id: Some(format!("{MODAL}:{prompt}:{button}")),
            title: Some(form.title.clone()),
            components: Some(fields),
            ..InteractionResponseData::default()
        }),
    }
}

/// Acknowledges a click or a submitted modal without changing the message yet.
pub(crate) fn deferred() -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::DeferredUpdateMessage,
        data: None,
    }
}

/// A short reply only the clicker sees.
pub(crate) fn private_note(text: &str) -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(text.to_string()),
            flags: Some(twilight_model::channel::message::MessageFlags::EPHEMERAL),
            ..InteractionResponseData::default()
        }),
    }
}

/// The text values of a submitted modal, by field id.
pub(crate) fn values(data: &ModalInteractionData) -> BTreeMap<String, Secret<String>> {
    fn walk(c: &ModalInteractionComponent, out: &mut BTreeMap<String, Secret<String>>) {
        match c {
            ModalInteractionComponent::TextInput(t) => {
                out.insert(t.custom_id.clone(), Secret::new(t.value.clone()));
            }
            ModalInteractionComponent::Label(l) => walk(&l.component, out),
            ModalInteractionComponent::ActionRow(r) => {
                for c in &r.components {
                    walk(c, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    for c in &data.components {
        walk(c, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{FormField, PromptButton};

    fn sample() -> Prompt {
        Prompt {
            id: "abc".into(),
            text: "Deploy?".into(),
            buttons: vec![
                PromptButton {
                    id: "ok".into(),
                    label: "Confirm".into(),
                    style: ButtonStyle::Danger,
                    form: Some(Form {
                        title: "Confirm".into(),
                        fields: vec![FormField {
                            id: "otp".into(),
                            label: "Code".into(),
                            style: FieldStyle::Short,
                            required: true,
                            placeholder: None,
                            min_len: Some(6),
                            max_len: Some(6),
                        }],
                    }),
                },
                PromptButton {
                    id: "no".into(),
                    label: "Cancel".into(),
                    style: ButtonStyle::Secondary,
                    form: None,
                },
            ],
            allowed: vec![],
        }
    }

    #[test]
    fn custom_ids_round_trip() {
        assert_eq!(
            parse_id(&button_id("abc", "ok"), false),
            Some(("abc", "ok"))
        );
        assert_eq!(parse_id("okf:abc:ok", true), Some(("abc", "ok")));
        assert_eq!(parse_id("okf:abc:ok", false), None);
        assert_eq!(parse_id("okp::ok", false), None);
        assert_eq!(parse_id("other", false), None);
    }

    #[test]
    fn buttons_and_modal_serialize_as_discord_expects() {
        let rows = serde_json::to_value(components(&sample())).unwrap();
        assert_eq!(rows[0]["type"], 1);
        assert_eq!(rows[0]["components"][0]["custom_id"], "okp:abc:ok");
        assert_eq!(rows[0]["components"][0]["style"], 4);
        assert_eq!(rows[0]["components"][1]["style"], 2);
        let form = sample().buttons[0].form.clone().unwrap();
        let modal = serde_json::to_value(modal("abc", "ok", &form)).unwrap();
        assert_eq!(modal["type"], 9);
        assert_eq!(modal["data"]["custom_id"], "okf:abc:ok");
        let label = &modal["data"]["components"][0];
        assert_eq!(label["type"], 18);
        assert_eq!(label["component"]["custom_id"], "otp");
        assert_eq!(label["component"]["min_length"], 6);
    }

    #[test]
    fn modal_values_are_collected() {
        let data: ModalInteractionData = serde_json::from_value(serde_json::json!({
            "custom_id": "okf:abc:ok",
            "components": [
                {"type": 18, "id": 1, "component": {"type": 4, "id": 2, "custom_id": "otp", "value": "123456"}},
                {"type": 1, "id": 3, "components": [{"type": 4, "id": 4, "custom_id": "note", "value": "x"}]}
            ]
        }))
        .unwrap();
        let v = values(&data);
        assert_eq!(v["otp"].expose(), "123456");
        assert_eq!(v["note"].expose(), "x");
    }
}
