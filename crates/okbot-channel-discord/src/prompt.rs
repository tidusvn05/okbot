//! Prompts on Discord: buttons and a menu under a message, cards as Components V2 containers,
//! and a modal for buttons with a form.
//!
//! Custom ids carry the prompt and button: `okp:<prompt>:<button>` on buttons, `oks:<prompt>` on
//! the menu (the picked option is the answer's button) and `okf:<prompt>:<button>` on the modal,
//! so a click needs no other lookup than the prompt itself.

use okbot_core::{ButtonStyle, FieldStyle, Form, Prompt, Secret};
use std::collections::BTreeMap;
use twilight_model::application::interaction::modal::{
    ModalInteractionComponent, ModalInteractionData,
};
use twilight_model::channel::message::Component;
use twilight_model::channel::message::component::{
    ActionRow, Button, ButtonStyle as DiscordStyle, Container, Label, MediaGallery,
    MediaGalleryItem, SelectMenu, SelectMenuOption, SelectMenuType, Separator, TextDisplay,
    TextInput, TextInputStyle, UnfurledMediaItem,
};
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};

const BUTTON: &str = "okp";
const MENU: &str = "oks";
const MODAL: &str = "okf";

/// The custom id of a button.
pub(crate) fn button_id(prompt: &str, button: &str) -> String {
    format!("{BUTTON}:{prompt}:{button}")
}

/// The custom id of a prompt's menu.
pub(crate) fn menu_id(prompt: &str) -> String {
    format!("{MENU}:{prompt}")
}

/// `(prompt, button)` from a button's (`modal == false`) or a modal's custom id.
pub(crate) fn parse_id(custom_id: &str, modal: bool) -> Option<(&str, &str)> {
    let rest = custom_id.strip_prefix(if modal { MODAL } else { BUTTON })?;
    let (prompt, button) = rest.strip_prefix(':')?.split_once(':')?;
    (!prompt.is_empty() && !button.is_empty()).then_some((prompt, button))
}

/// The prompt id from a menu's custom id.
pub(crate) fn parse_menu_id(custom_id: &str) -> Option<&str> {
    let prompt = custom_id.strip_prefix(MENU)?.strip_prefix(':')?;
    (!prompt.is_empty() && !prompt.contains(':')).then_some(prompt)
}

fn style(s: ButtonStyle) -> DiscordStyle {
    match s {
        ButtonStyle::Secondary => DiscordStyle::Secondary,
        ButtonStyle::Success => DiscordStyle::Success,
        ButtonStyle::Danger => DiscordStyle::Danger,
        _ => DiscordStyle::Primary,
    }
}

/// The action rows of a prompt: its menu (if any), then its buttons (if any). Link buttons
/// open their URL and carry no custom id.
pub(crate) fn components(prompt: &Prompt) -> Vec<Component> {
    let mut rows = Vec::new();
    if let Some(select) = &prompt.select {
        rows.push(Component::ActionRow(ActionRow {
            id: None,
            components: vec![Component::SelectMenu(SelectMenu {
                id: None,
                channel_types: None,
                custom_id: menu_id(&prompt.id),
                default_values: None,
                disabled: false,
                kind: SelectMenuType::Text,
                max_values: Some(1),
                min_values: Some(1),
                options: Some(
                    select
                        .options
                        .iter()
                        .map(|o| SelectMenuOption {
                            default: false,
                            description: o.description.clone(),
                            emoji: None,
                            label: o.label.clone(),
                            value: o.id.clone(),
                        })
                        .collect(),
                ),
                placeholder: (!select.placeholder.trim().is_empty())
                    .then(|| select.placeholder.clone()),
                required: None,
            })],
        }));
    }
    if !prompt.buttons.is_empty() {
        let buttons = prompt
            .buttons
            .iter()
            .map(|b| {
                Component::Button(match &b.url {
                    Some(url) => Button {
                        id: None,
                        custom_id: None,
                        disabled: false,
                        emoji: None,
                        label: Some(b.label.clone()),
                        style: DiscordStyle::Link,
                        url: Some(url.clone()),
                        sku_id: None,
                    },
                    None => Button {
                        id: None,
                        custom_id: Some(button_id(&prompt.id, &b.id)),
                        disabled: false,
                        emoji: None,
                        label: Some(b.label.clone()),
                        style: style(b.style),
                        url: None,
                        sku_id: None,
                    },
                })
            })
            .collect();
        rows.push(Component::ActionRow(ActionRow {
            id: None,
            components: buttons,
        }));
    }
    rows
}

fn text(content: &str) -> Component {
    Component::TextDisplay(TextDisplay {
        id: None,
        content: content.to_string(),
    })
}

/// Discord's limit on a media gallery item's description (alt text).
const ALT_MAX: usize = 1024;

/// The file name a card's picture is uploaded under: the image's own name when it is a plain
/// file name, else `chart.png`. `attachment://` references must match it exactly.
pub(crate) fn file_name(image: &okbot_core::Image) -> String {
    let name = image.name.trim();
    let plain = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if plain {
        name.to_string()
    } else {
        "chart.png".to_string()
    }
}

/// `text` cut to at most `max` bytes (so also characters) on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A gallery showing the uploaded file `file` with `alt` as its description.
fn gallery(file: &str, alt: &str) -> Component {
    let alt = cut(alt.trim(), ALT_MAX);
    Component::MediaGallery(MediaGallery {
        id: None,
        items: vec![MediaGalleryItem {
            media: UnfurledMediaItem {
                url: format!("attachment://{file}"),
                proxy_url: None,
                height: None,
                width: None,
                content_type: None,
            },
            description: (!alt.is_empty()).then(|| alt.to_string()),
            spoiler: None,
        }],
    })
}

/// A Components V2 message (sent with `IS_COMPONENTS_V2`, without content): the text, one
/// container per card with the card's tone as its accent colour, then the action rows. At most
/// 1 + 5 × 4 + 1 + 2 + 6 = 30 of Discord's 40 components.
///
/// `picture` is the file name of the message's attachment that shows the card image: the card
/// that has an image gets a media gallery of it after its text. `None` leaves the picture out.
pub(crate) fn components_v2(prompt: &Prompt, picture: Option<&str>) -> Vec<Component> {
    let mut out = Vec::new();
    if !prompt.text.trim().is_empty() {
        out.push(text(&prompt.text));
    }
    let mut shown = false;
    for card in &prompt.cards {
        let mut inner = Vec::new();
        let title = card.title.as_deref().unwrap_or_default().trim();
        let image = match (picture, &card.image) {
            // At most one picture per message.
            (Some(file), Some(image)) if !shown => {
                shown = true;
                Some(gallery(file, &image.alt))
            }
            _ => None,
        };
        let body = !card.text.trim().is_empty();
        if !title.is_empty() {
            inner.push(text(&format!("### {title}")));
            if body || image.is_some() {
                inner.push(Component::Separator(Separator {
                    id: None,
                    divider: Some(true),
                    spacing: None,
                }));
            }
        }
        if body {
            inner.push(text(&card.text));
        }
        inner.extend(image);
        if inner.is_empty() {
            continue;
        }
        out.push(Component::Container(Container {
            id: None,
            accent_color: Some(Some(card.tone.color())),
            spoiler: None,
            components: inner,
        }));
    }
    out.extend(components(prompt));
    out
}

/// A V2 message showing only `content` (a closed prompt that had cards).
pub(crate) fn text_v2(content: &str) -> Vec<Component> {
    if content.trim().is_empty() {
        // A V2 message cannot be empty.
        vec![text("\u{200b}")]
    } else {
        vec![text(content)]
    }
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
                    value: f.value.clone(),
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
                            value: None,
                        }],
                    }),
                    url: None,
                },
                PromptButton {
                    id: "no".into(),
                    label: "Cancel".into(),
                    style: ButtonStyle::Secondary,
                    form: None,
                    url: None,
                },
            ],
            allowed: vec![],
            cards: Vec::new(),
            select: None,
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

    fn interactive() -> Prompt {
        use okbot_core::{Card, Select, SelectOption, Tone};
        Prompt {
            id: "v1a2b3c4d".into(),
            text: "Your orders".into(),
            buttons: vec![
                PromptButton {
                    id: "b1".into(),
                    label: "Refresh".into(),
                    style: ButtonStyle::Secondary,
                    form: None,
                    url: None,
                },
                PromptButton {
                    id: "b2".into(),
                    label: "Docs".into(),
                    style: ButtonStyle::Primary,
                    form: None,
                    url: Some("https://example.com/docs".into()),
                },
            ],
            allowed: vec![],
            cards: vec![
                Card {
                    title: Some("Order 17".into()),
                    tone: Tone::Success,
                    text: "**Paid** · 3 items".into(),
                    image: None,
                },
                Card {
                    title: None,
                    tone: Tone::Neutral,
                    text: "No title".into(),
                    image: None,
                },
            ],
            select: Some(Select {
                placeholder: "Pick an order".into(),
                options: vec![
                    SelectOption {
                        id: "o1".into(),
                        label: "Order 17".into(),
                        description: Some("Paid".into()),
                    },
                    SelectOption {
                        id: "o2".into(),
                        label: "Order 18".into(),
                        description: None,
                    },
                ],
            }),
        }
    }

    #[test]
    fn menu_ids_round_trip() {
        assert_eq!(menu_id("v1a2b3c4d"), "oks:v1a2b3c4d");
        assert_eq!(parse_menu_id("oks:v1a2b3c4d"), Some("v1a2b3c4d"));
        assert_eq!(parse_menu_id("oks:"), None);
        assert_eq!(parse_menu_id("okp:v1:b1"), None);
        assert_eq!(parse_menu_id("oks:a:b"), None);
        assert_eq!(parse_id("oks:v1a2b3c4d", false), None);
    }

    #[test]
    fn cards_serialize_as_components_v2() {
        let v2 = serde_json::to_value(components_v2(&interactive(), None)).unwrap();
        assert_eq!(
            v2,
            serde_json::json!([
                {"type": 10, "content": "Your orders"},
                {"type": 17, "accent_color": 0x22_C55E, "components": [
                    {"type": 10, "content": "### Order 17"},
                    {"type": 14, "divider": true},
                    {"type": 10, "content": "**Paid** · 3 items"}
                ]},
                {"type": 17, "accent_color": 0x58_65F2, "components": [
                    {"type": 10, "content": "No title"}
                ]},
                {"type": 1, "components": [{
                    "type": 3, "custom_id": "oks:v1a2b3c4d", "disabled": false,
                    "max_values": 1, "min_values": 1, "placeholder": "Pick an order",
                    "options": [
                        {"default": false, "description": "Paid", "label": "Order 17", "value": "o1"},
                        {"default": false, "label": "Order 18", "value": "o2"}
                    ]
                }]},
                {"type": 1, "components": [
                    {"type": 2, "custom_id": "okp:v1a2b3c4d:b1", "label": "Refresh", "style": 2},
                    {"type": 2, "label": "Docs", "style": 5, "url": "https://example.com/docs"}
                ]}
            ])
        );
        let count: usize = components_v2(&interactive(), None)
            .iter()
            .map(Component::component_count)
            .sum();
        assert!(count <= 40, "{count}");
    }

    #[test]
    fn a_closed_prompt_keeps_its_cards_without_rows() {
        let mut p = interactive();
        p.buttons.clear();
        p.select = None;
        let v2 = serde_json::to_value(components_v2(&p, None)).unwrap();
        assert_eq!(v2.as_array().unwrap().len(), 3);
        assert!(v2.as_array().unwrap().iter().all(|c| c["type"] != 1));
        assert!(components(&p).is_empty(), "no empty action rows");
        let text = serde_json::to_value(text_v2("")).unwrap();
        assert_eq!(text[0]["type"], 10);
        assert_ne!(text[0]["content"], "");
    }

    #[test]
    fn form_values_prefill_the_modal() {
        let mut form = sample().buttons[0].form.clone().unwrap();
        form.fields[0].value = Some("123".into());
        let modal = serde_json::to_value(modal("abc", "ok", &form)).unwrap();
        assert_eq!(modal["data"]["components"][0]["component"]["value"], "123");
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

    #[test]
    fn the_gallery_alt_text_is_cut_on_a_char_boundary() {
        let mut p = interactive();
        p.cards[0].image = Some(okbot_core::Image {
            name: "../evil name.png".into(),
            alt: "ở".repeat(600),
            width: 1,
            height: 1,
            png: Vec::new(),
        });
        let file = file_name(p.cards[0].image.as_ref().unwrap());
        assert_eq!(file, "chart.png", "not a plain file name");
        let v2 = serde_json::to_value(components_v2(&p, Some(&file))).unwrap();
        let item = &v2[1]["components"][3]["items"][0];
        assert_eq!(item["media"]["url"], "attachment://chart.png");
        let alt = item["description"].as_str().unwrap();
        assert!(alt.len() <= 1024 && alt.chars().all(|c| c == 'ở'));
        // Without a picture, the card is drawn as before.
        let none = serde_json::to_value(components_v2(&p, None)).unwrap();
        assert_eq!(none[1]["components"].as_array().unwrap().len(), 3);
    }
}
