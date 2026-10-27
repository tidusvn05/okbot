//! Prompts on Slack (Block Kit): sections with the text, a header and a section per card, and
//! an actions block with a `static_select` menu and the buttons. A button with a form opens a
//! modal (`views.open`).
//!
//! Action ids carry the prompt: `okp:<prompt>:<button>` for buttons, `oks:<prompt>` for the
//! menu (the picked option's value is its id) and `okl:<prompt>:<button>` for link buttons,
//! whose presses Slack reports too and okbot ignores. Modals have the callback id `okf` and
//! carry the prompt, the button and the message in `private_metadata`.

use std::collections::BTreeMap;

use okbot_core::{
    ButtonStyle, ChannelId, ConversationKey, FieldStyle, Form, Prompt, ReplyTarget, Secret, Tone,
};
use serde_json::{Value, json};

const BUTTON: &str = "okp";
const MENU: &str = "oks";
const LINK: &str = "okl";
/// Callback id of okbot's modals.
pub(crate) const FORM: &str = "okf";
/// Action id of every text input in a modal (the block id is the field id).
const INPUT: &str = "v";
/// Slack's limit for the text of a section.
const SECTION_CHARS: usize = 3000;

/// A prompt waiting for an answer.
pub(crate) struct Open {
    /// The prompt, to show its buttons again when the text changes and to find forms.
    pub(crate) prompt: Prompt,
    /// Where it is.
    pub(crate) target: ReplyTarget,
    /// Its message.
    pub(crate) ts: String,
    /// When this process last showed it.
    pub(crate) at: std::time::Instant,
}

/// The first `max` characters of `s`, with an ellipsis when cut.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn plain(text: &str, max: usize) -> Value {
    json!({"type": "plain_text", "text": cut(text, max), "emoji": true})
}

/// Sections for `text`, split under Slack's 3000 characters (at a line break when there is one).
fn sections(text: &str, blocks: &mut Vec<Value>) {
    let mut rest = text.trim();
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .nth(SECTION_CHARS)
            .map_or(rest.len(), |(i, _)| i);
        let split = if end == rest.len() {
            end
        } else {
            rest[..end].rfind('\n').filter(|&i| i > 0).unwrap_or(end)
        };
        blocks.push(json!({"type": "section", "text": {"type": "mrkdwn", "text": &rest[..split]}}));
        rest = rest[split..].trim_start();
    }
}

/// The blocks of `prompt`: its text, its cards and, with `actions`, its menu and buttons.
pub(crate) fn blocks(prompt: &Prompt, actions: bool) -> Value {
    let mut blocks = Vec::new();
    sections(&prompt.text, &mut blocks);
    for card in &prompt.cards {
        if !blocks.is_empty() {
            blocks.push(json!({"type": "divider"}));
        }
        let emoji = card.tone.emoji();
        let title = card.title.as_deref().unwrap_or_default().trim();
        if title.is_empty() {
            let text = if card.tone == Tone::Neutral {
                card.text.clone()
            } else {
                format!("{emoji} {}", card.text)
            };
            sections(&text, &mut blocks);
        } else {
            let title = if card.tone == Tone::Neutral {
                title.to_string()
            } else {
                format!("{emoji} {title}")
            };
            blocks.push(json!({"type": "header", "text": plain(&title, 150)}));
            sections(&card.text, &mut blocks);
        }
    }
    if actions {
        let mut elements = Vec::new();
        if let Some(select) = &prompt.select {
            let options: Vec<Value> = select
                .options
                .iter()
                .map(|o| {
                    let mut option = json!({"text": plain(&o.label, 75), "value": o.id});
                    if let Some(d) = o.description.as_deref().filter(|d| !d.trim().is_empty()) {
                        option["description"] = plain(d, 75);
                    }
                    option
                })
                .collect();
            elements.push(json!({
                "type": "static_select",
                "action_id": format!("{MENU}:{}", prompt.id),
                "placeholder": plain(&select.placeholder, 150),
                "options": options,
            }));
        }
        for b in &prompt.buttons {
            let mut button = json!({"type": "button", "text": plain(&b.label, 75), "value": b.id});
            if let Some(url) = &b.url {
                button["url"] = json!(url);
                button["action_id"] = json!(format!("{LINK}:{}:{}", prompt.id, b.id));
            } else {
                button["action_id"] = json!(format!("{BUTTON}:{}:{}", prompt.id, b.id));
                // Slack has two styles besides the default.
                let style = match b.style {
                    ButtonStyle::Primary | ButtonStyle::Success => Some("primary"),
                    ButtonStyle::Danger => Some("danger"),
                    _ => None,
                };
                if let Some(style) = style {
                    button["style"] = json!(style);
                }
            }
            elements.push(button);
        }
        if !elements.is_empty() {
            blocks.push(json!({"type": "actions", "elements": elements}));
        }
    }
    if blocks.is_empty() {
        // Slack rejects an empty section; this only happens to a prompt with nothing left.
        blocks.push(json!({"type": "section", "text": {"type": "mrkdwn", "text": " "}}));
    }
    Value::Array(blocks)
}

/// The plain `text` of a message with blocks (notifications, screen readers). Slack reads it
/// as mrkdwn: texts come rendered, but a card's title is shown as plain text in the blocks and
/// is escaped here so it cannot become a mention or a link.
pub(crate) fn fallback(prompt: &Prompt) -> String {
    let mut text = prompt.text.trim().to_string();
    for card in &prompt.cards {
        let title = card.title.as_deref().map(escape).unwrap_or_default();
        for part in [title.as_str(), &card.text] {
            if !part.trim().is_empty() {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(part.trim());
            }
        }
    }
    if text.is_empty() { " ".into() } else { text }
}

/// Escapes the three characters Slack treats as control characters in mrkdwn.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// What a `block_actions` press means.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Press<'a> {
    /// An answer: a button or an option of the prompt.
    Answer {
        /// [`Prompt::id`].
        prompt: &'a str,
        /// The button or option id.
        id: &'a str,
    },
    /// A link button, or something that is not okbot's.
    Ignore,
}

/// Reads the first action of a `block_actions` payload.
pub(crate) fn press(action: &Value) -> Press<'_> {
    let Some(action_id) = action["action_id"].as_str() else {
        return Press::Ignore;
    };
    let Some((kind, rest)) = action_id.split_once(':') else {
        return Press::Ignore;
    };
    match kind {
        BUTTON => match rest.split_once(':') {
            Some((prompt, id)) if !prompt.is_empty() && !id.is_empty() => {
                Press::Answer { prompt, id }
            }
            _ => Press::Ignore,
        },
        MENU if !rest.is_empty() && !rest.contains(':') => {
            match action["selected_option"]["value"].as_str() {
                Some(id) if !id.is_empty() => Press::Answer { prompt: rest, id },
                _ => Press::Ignore,
            }
        }
        _ => Press::Ignore,
    }
}

/// Where a prompt message is, from a `block_actions` payload: its target and `ts`.
pub(crate) fn located(channel: &ChannelId, payload: &Value) -> Option<(ReplyTarget, String)> {
    let container = &payload["container"];
    let message = &payload["message"];
    let room = payload["channel"]["id"]
        .as_str()
        .or(container["channel_id"].as_str())?;
    let ts = message["ts"]
        .as_str()
        .or(container["message_ts"].as_str())?;
    let thread = message["thread_ts"]
        .as_str()
        .or(container["thread_ts"].as_str());
    let conversation = ConversationKey {
        channel: channel.clone(),
        space: payload["team"]["id"].as_str().map(str::to_string),
        room: room.to_string(),
        thread: thread.map(str::to_string),
    };
    Some((
        ReplyTarget {
            conversation,
            in_reply_to: None,
        },
        ts.to_string(),
    ))
}

/// What a modal remembers: the prompt, the button and where the prompt message is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Meta {
    pub(crate) prompt: String,
    pub(crate) button: String,
    pub(crate) target: ReplyTarget,
    pub(crate) ts: String,
}

impl Meta {
    fn encode(&self) -> String {
        let c = &self.target.conversation;
        json!({
            "p": self.prompt, "b": self.button, "r": c.room, "t": c.thread, "s": c.space,
            "m": self.ts,
        })
        .to_string()
    }

    /// Reads `private_metadata` back.
    pub(crate) fn decode(channel: &ChannelId, metadata: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(metadata).ok()?;
        let text = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        Some(Self {
            prompt: text("p")?,
            button: text("b")?,
            target: ReplyTarget {
                conversation: ConversationKey {
                    channel: channel.clone(),
                    space: text("s"),
                    room: text("r")?,
                    thread: text("t"),
                },
                in_reply_to: None,
            },
            ts: text("m")?,
        })
    }
}

/// The modal of `form`, opened by a press of `meta.button`.
pub(crate) fn modal(form: &Form, meta: &Meta) -> Value {
    let inputs: Vec<Value> = form
        .fields
        .iter()
        .map(|f| {
            let mut element = json!({
                "type": "plain_text_input",
                "action_id": INPUT,
                "multiline": f.style == FieldStyle::Paragraph,
            });
            if let Some(p) = f.placeholder.as_deref().filter(|p| !p.is_empty()) {
                element["placeholder"] = plain(p, 150);
            }
            if let Some(v) = f.value.as_deref().filter(|v| !v.is_empty()) {
                element["initial_value"] = json!(v);
            }
            if let Some(min) = f.min_len {
                element["min_length"] = json!(min.min(3000));
            }
            if let Some(max) = f.max_len {
                element["max_length"] = json!(max.min(3000));
            }
            json!({
                "type": "input",
                "block_id": f.id,
                "label": plain(&f.label, 2000),
                "optional": !f.required,
                "element": element,
            })
        })
        .collect();
    json!({
        "type": "modal",
        "callback_id": FORM,
        "title": plain(&form.title, 24),
        "submit": plain("Send", 24),
        "close": plain("Cancel", 24),
        "private_metadata": meta.encode(),
        "blocks": inputs,
    })
}

/// The values of a `view_submission`, by field id (fields left empty are absent).
pub(crate) fn values(form: &Form, view: &Value) -> BTreeMap<String, Secret<String>> {
    form.fields
        .iter()
        .filter_map(|f| {
            let value = view["state"]["values"][&f.id][INPUT]["value"].as_str()?;
            Some((f.id.clone(), Secret::new(value.to_string())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use okbot_core::{Card, FormField, PromptButton, Select, SelectOption};

    use super::*;

    fn button(id: &str, style: ButtonStyle) -> PromptButton {
        PromptButton {
            id: id.into(),
            label: id.into(),
            style,
            form: None,
            url: None,
        }
    }

    #[test]
    fn blocks_and_action_ids() {
        let prompt = Prompt {
            id: "4f2a9c".into(),
            text: "*Deploy?*".into(),
            buttons: vec![
                button("allow", ButtonStyle::Success),
                button("later", ButtonStyle::Secondary),
            ],
            ..Prompt::default()
        };
        let b = blocks(&prompt, true);
        assert_eq!(b[0]["text"]["text"], "*Deploy?*");
        assert_eq!(b[1]["elements"][0]["action_id"], "okp:4f2a9c:allow");
        assert_eq!(b[1]["elements"][0]["style"], "primary");
        assert!(b[1]["elements"][1].get("style").is_none());
        assert_eq!(blocks(&prompt, false).as_array().unwrap().len(), 1);
        let press_of = |id: &str| json!({"action_id": id});
        assert_eq!(
            press(&press_of("okp:4f2a9c:allow")),
            Press::Answer {
                prompt: "4f2a9c",
                id: "allow"
            }
        );
        assert_eq!(press(&press_of("okp:4f2a9c")), Press::Ignore);
        assert_eq!(press(&press_of("x:a:b")), Press::Ignore);
        assert_eq!(press(&press_of("okl:4f2a9c:docs")), Press::Ignore);
    }

    fn interactive() -> Prompt {
        let mut docs = button("b2", ButtonStyle::Primary);
        docs.url = Some("https://example.com/docs".into());
        Prompt {
            id: "v1a2b3c4d".into(),
            text: "Here is the stock:".into(),
            cards: vec![
                Card {
                    title: Some("Warehouse A".into()),
                    tone: Tone::Success,
                    text: "• *Rice* · 30".into(),
                    image: None,
                },
                Card {
                    title: None,
                    tone: Tone::Warning,
                    text: "Low on salt".into(),
                    image: None,
                },
            ],
            select: Some(Select {
                placeholder: "Pick a product".into(),
                options: vec![
                    SelectOption {
                        id: "o1".into(),
                        label: "Rice".into(),
                        description: Some("30 bags".into()),
                    },
                    SelectOption {
                        id: "o2".into(),
                        label: "x".repeat(90),
                        description: None,
                    },
                ],
            }),
            buttons: vec![button("b1", ButtonStyle::Danger), docs],
            allowed: vec![],
        }
    }

    #[test]
    fn cards_menu_and_links() {
        let prompt = interactive();
        let b = blocks(&prompt, true);
        let kinds: Vec<&str> = b
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "section", "divider", "header", "section", "divider", "section", "actions"
            ]
        );
        assert_eq!(b[2]["text"]["text"], "✅ Warehouse A");
        assert_eq!(b[2]["text"]["type"], "plain_text");
        assert_eq!(b[5]["text"]["text"], "⚠️ Low on salt");
        let menu = &b[6]["elements"][0];
        assert_eq!(menu["type"], "static_select");
        assert_eq!(menu["action_id"], "oks:v1a2b3c4d");
        assert_eq!(menu["options"][0]["value"], "o1");
        assert_eq!(menu["options"][0]["description"]["text"], "30 bags");
        assert_eq!(
            menu["options"][1]["text"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            75,
            "Slack's limit for option text"
        );
        assert_eq!(b[6]["elements"][1]["style"], "danger");
        let link = &b[6]["elements"][2];
        assert_eq!(link["url"], "https://example.com/docs");
        assert_eq!(link["action_id"], "okl:v1a2b3c4d:b2");
        assert!(link.get("style").is_none());
        // Closed: no actions block.
        let closed = blocks(&prompt, false);
        assert!(
            closed
                .as_array()
                .unwrap()
                .iter()
                .all(|b| b["type"] != "actions")
        );
        assert_eq!(
            fallback(&prompt),
            "Here is the stock:\n\nWarehouse A\n\n• *Rice* · 30\n\nLow on salt"
        );
        let mut loud = prompt.clone();
        loud.cards[0].title = Some("<!channel> <https://x.test|Invoice> & co".into());
        assert!(
            fallback(&loud).contains("&lt;!channel&gt; &lt;https://x.test|Invoice&gt; &amp; co"),
            "{}",
            fallback(&loud)
        );

        let pick = json!({"action_id": "oks:v1a2b3c4d", "selected_option": {"value": "o2"}});
        assert_eq!(
            press(&pick),
            Press::Answer {
                prompt: "v1a2b3c4d",
                id: "o2"
            }
        );
        assert_eq!(press(&json!({"action_id": "oks:v1a2b3c4d"})), Press::Ignore);
    }

    #[test]
    fn cards_without_text_and_long_text() {
        let prompt = Prompt {
            id: "p".into(),
            cards: vec![Card {
                title: Some("Only".into()),
                tone: Tone::Neutral,
                image: None,
                text: String::new(),
            }],
            ..Prompt::default()
        };
        let b = blocks(&prompt, true);
        assert_eq!(
            b.as_array().unwrap().len(),
            1,
            "no empty section, no divider"
        );
        assert_eq!(b[0]["text"]["text"], "Only");
        let long = format!("{}\n{}", "a".repeat(2000), "b".repeat(1500));
        let mut out = Vec::new();
        sections(&long, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["text"]["text"], "a".repeat(2000));
        let mut out = Vec::new();
        sections(&"é".repeat(3500), &mut out);
        assert_eq!(out.len(), 2, "split by characters, not bytes");
    }

    #[test]
    fn modals_and_values() {
        let channel: ChannelId = "slack:acme".parse().unwrap();
        let form = Form {
            title: "A rather long form title here".into(),
            fields: vec![
                FormField {
                    id: "f1".into(),
                    label: "Name".into(),
                    style: FieldStyle::Short,
                    required: true,
                    placeholder: Some("Lan".into()),
                    min_len: Some(2),
                    max_len: Some(40),
                    value: Some("Lan".into()),
                },
                FormField {
                    id: "f2".into(),
                    label: "Note".into(),
                    style: FieldStyle::Paragraph,
                    required: false,
                    placeholder: None,
                    min_len: None,
                    max_len: None,
                    value: None,
                },
            ],
        };
        let mut conversation = ConversationKey::room(channel.clone(), "C1");
        conversation.thread = Some("1.0".into());
        conversation.space = Some("T1".into());
        let meta = Meta {
            prompt: "v1a2b3c4d".into(),
            button: "b1".into(),
            target: ReplyTarget {
                conversation,
                in_reply_to: None,
            },
            ts: "2.0".into(),
        };
        let view = modal(&form, &meta);
        assert_eq!(view["callback_id"], "okf");
        assert_eq!(view["title"]["text"].as_str().unwrap().chars().count(), 24);
        let name = &view["blocks"][0];
        assert_eq!(name["block_id"], "f1");
        assert_eq!(name["optional"], false);
        assert_eq!(name["element"]["initial_value"], "Lan");
        assert_eq!(name["element"]["min_length"], 2);
        assert_eq!(name["element"]["max_length"], 40);
        assert_eq!(name["element"]["multiline"], false);
        let note = &view["blocks"][1];
        assert_eq!(note["optional"], true);
        assert_eq!(note["element"]["multiline"], true);
        assert!(note["element"].get("initial_value").is_none());
        let back = Meta::decode(&channel, view["private_metadata"].as_str().unwrap()).unwrap();
        assert_eq!(back, meta);
        assert!(Meta::decode(&channel, "{}").is_none());

        let submitted = json!({"state": {"values": {
            "f1": {"v": {"type": "plain_text_input", "value": "Mai"}},
            "f2": {"v": {"type": "plain_text_input", "value": null}},
        }}});
        let values = values(&form, &submitted);
        assert_eq!(values.len(), 1);
        assert_eq!(values["f1"].expose(), "Mai");
    }

    #[test]
    fn locates_the_message() {
        let channel: ChannelId = "slack:acme".parse().unwrap();
        let payload = json!({
            "team": {"id": "T1"}, "channel": {"id": "C1"},
            "message": {"ts": "2.0", "thread_ts": "1.0"},
        });
        let (target, ts) = located(&channel, &payload).unwrap();
        assert_eq!(ts, "2.0");
        assert_eq!(target.conversation.thread.as_deref(), Some("1.0"));
        assert_eq!(target.conversation.space.as_deref(), Some("T1"));
        let container = json!({"container": {"channel_id": "D1", "message_ts": "3.0"}});
        let (target, ts) = located(&channel, &container).unwrap();
        assert_eq!(
            (target.conversation.room.as_str(), ts.as_str()),
            ("D1", "3.0")
        );
        assert!(target.conversation.thread.is_none());
        assert!(located(&channel, &json!({})).is_none());
    }
}
