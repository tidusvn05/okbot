//! Prompts on Telegram: an inline keyboard under the message. Callback data carries the prompt
//! and the button or menu option, `okp:<prompt>:<id>` (at most 64 bytes, which prompt, button and
//! option ids keep to). A menu is one row per option, above the buttons; link buttons open their
//! URL and send no callback. Telegram has no forms, so buttons with one are not shown here, and no
//! card widget: cards are appended to the text, a bold title line and their body. A card's chart
//! (ADR 0016) is sent as a photo with the text as its caption when it fits ([`fits_caption`]).

use okbot_core::{Card, Image, Prompt, ReplyTarget, UserId};
use serde_json::{Value, json};

const PREFIX: &str = "okp";

/// Buttons per keyboard row.
const PER_ROW: usize = 5;

/// An option's description goes on its button when label and description fit in this many
/// characters; Telegram cuts longer button text off.
const OPTION_TEXT: usize = 48;

/// A prompt waiting for a press.
pub(crate) struct Open {
    /// Who may answer; anyone when empty.
    pub(crate) allowed: Vec<UserId>,
    /// The button and option ids that answer.
    pub(crate) answers: Vec<String>,
    /// Its keyboard, to keep it when the text changes.
    pub(crate) markup: Value,
    /// Where it is.
    pub(crate) target: ReplyTarget,
    /// Its message.
    pub(crate) message_id: i64,
}

impl Open {
    /// The entry for `prompt`, shown as `message_id` at `target`.
    pub(crate) fn new(prompt: &Prompt, target: ReplyTarget, message_id: i64) -> Self {
        let answers = prompt
            .select
            .iter()
            .flat_map(|s| s.options.iter().map(|o| o.id.clone()))
            .chain(
                prompt
                    .buttons
                    .iter()
                    .filter(|b| b.url.is_none() && b.form.is_none())
                    .map(|b| b.id.clone()),
            )
            .collect();
        Self {
            allowed: prompt.allowed.clone(),
            answers,
            markup: markup(prompt),
            target,
            message_id,
        }
    }
}

fn callback(prompt: &Prompt, id: &str) -> String {
    format!("{PREFIX}:{}:{id}", prompt.id)
}

/// The inline keyboard: a row per menu option, then the buttons, at most [`PER_ROW`] a row.
/// Buttons with a form are left out (Telegram cannot show forms).
pub(crate) fn markup(prompt: &Prompt) -> Value {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    if let Some(select) = &prompt.select {
        for o in &select.options {
            let text = match o.description.as_deref().map(str::trim) {
                Some(d)
                    if !d.is_empty()
                        && o.label.chars().count() + d.chars().count() + 3 <= OPTION_TEXT =>
                {
                    format!("{} — {d}", o.label)
                }
                _ => o.label.clone(),
            };
            rows.push(vec![
                json!({"text": text, "callback_data": callback(prompt, &o.id)}),
            ]);
        }
    }
    let buttons: Vec<Value> = prompt
        .buttons
        .iter()
        .filter(|b| b.form.is_none())
        .map(|b| match &b.url {
            Some(url) => json!({"text": b.label, "url": url}),
            None => json!({"text": b.label, "callback_data": callback(prompt, &b.id)}),
        })
        .collect();
    rows.extend(buttons.chunks(PER_ROW).map(<[Value]>::to_vec));
    json!({ "inline_keyboard": rows })
}

/// A keyboard without buttons (removes them).
pub(crate) fn no_buttons() -> Value {
    json!({ "inline_keyboard": [] })
}

/// The most a photo caption holds, counted by Telegram after entity parsing.
const CAPTION_LIMIT: usize = 1024;

/// Whether `html` fits in a photo caption. Measured on the HTML itself (tags and escapes
/// included), in UTF-16 units, which is never less than what Telegram counts.
pub(crate) fn fits_caption(html: &str) -> bool {
    html.encode_utf16().count() <= CAPTION_LIMIT
}

/// The card with a chart picture, and the picture. `png` is empty in a prompt read back from the
/// store, where the message keeps the picture it was sent with.
pub(crate) fn image(prompt: &Prompt) -> Option<(&Card, &Image)> {
    prompt
        .cards
        .iter()
        .find_map(|c| c.image.as_ref().map(|i| (c, i)))
}

/// A card's title line in bold, the tone's emoji first; `None` when it has neither.
pub(crate) fn heading(card: &Card) -> Option<String> {
    let title = card.title.as_deref().map(str::trim).unwrap_or_default();
    let emoji = card.tone.emoji();
    if title.is_empty() && emoji.is_empty() {
        return None;
    }
    let head = [emoji, &escape(title)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("<b>{head}</b>"))
}

/// The message text: the prompt's text, then any cards (title in bold, tone as an emoji), blank
/// lines between blocks.
pub(crate) fn text(prompt: &Prompt) -> String {
    let mut out = prompt.text.clone();
    for card in &prompt.cards {
        let mut part = heading(card).unwrap_or_default();
        if !card.text.trim().is_empty() {
            if !part.is_empty() {
                part.push('\n');
            }
            part.push_str(&card.text);
        }
        if !part.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&part);
        }
    }
    out
}

/// Escapes plain text for `parse_mode = HTML`.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `(prompt, button)` from callback data.
pub(crate) fn parse(data: &str) -> Option<(&str, &str)> {
    let rest = data.strip_prefix(PREFIX)?.strip_prefix(':')?;
    let (prompt, button) = rest.split_once(':')?;
    (!prompt.is_empty() && !button.is_empty()).then_some((prompt, button))
}

#[cfg(test)]
mod tests {
    use okbot_core::{ButtonStyle, Card, PromptButton, Select, SelectOption, Tone};

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
                    url: None,
                },
                PromptButton {
                    id: "deny".into(),
                    label: "Deny".into(),
                    style: ButtonStyle::Danger,
                    form: None,
                    url: None,
                },
            ],
            allowed: vec![],
            cards: Vec::new(),
            select: None,
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

    #[test]
    fn menus_links_rows_and_cards() {
        let button = |id: &str, url: Option<&str>| PromptButton {
            id: id.into(),
            label: id.to_uppercase(),
            style: ButtonStyle::Primary,
            form: None,
            url: url.map(Into::into),
        };
        let option = |id: &str, label: &str, description: Option<&str>| SelectOption {
            id: id.into(),
            label: label.into(),
            description: description.map(Into::into),
        };
        let prompt = Prompt {
            id: "v1a2b3c4d".into(),
            text: "Pick a size".into(),
            buttons: vec![
                button("b1", None),
                button("b2", None),
                button("b3", None),
                button("b4", None),
                button("b5", None),
                button("docs", Some("https://example.com/sizes")),
            ],
            cards: vec![Card {
                title: Some("Stock <today>".into()),
                tone: Tone::Warning,
                text: "<i>low</i>".into(),
                image: None,
            }],
            select: Some(Select {
                placeholder: "Size".into(),
                options: vec![
                    option("o1", "Small", Some("fits 1")),
                    option("o2", "Large", Some(&"x".repeat(60))),
                ],
            }),
            ..Prompt::default()
        };
        let m = markup(&prompt);
        let rows = m["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows.len(), 4, "two options, then 5 + 1 buttons");
        assert_eq!(rows[0][0]["text"], "Small — fits 1");
        assert_eq!(rows[0][0]["callback_data"], "okp:v1a2b3c4d:o1");
        assert_eq!(
            rows[1][0]["text"], "Large",
            "long descriptions are left out"
        );
        assert_eq!(rows[2].as_array().unwrap().len(), 5);
        assert_eq!(
            rows[3][0],
            json!({"text": "DOCS", "url": "https://example.com/sizes"}),
            "a link has no callback data"
        );
        for row in rows {
            for b in row.as_array().unwrap() {
                assert!(b["callback_data"].as_str().is_none_or(|d| d.len() <= 64));
            }
        }
        assert_eq!(
            text(&prompt),
            "Pick a size\n\n<b>⚠️ Stock &lt;today&gt;</b>\n<i>low</i>"
        );

        assert!(image(&prompt).is_none());

        let target = ReplyTarget {
            conversation: okbot_core::ConversationKey::room("telegram:shop".parse().unwrap(), "7"),
            in_reply_to: None,
        };
        let open = Open::new(&prompt, target, 77);
        assert_eq!(open.answers, ["o1", "o2", "b1", "b2", "b3", "b4", "b5"]);

        let closed = Prompt {
            text: "Picked Small".into(),
            ..Prompt::default()
        };
        assert_eq!(markup(&closed), no_buttons());
    }

    #[test]
    fn captions_and_images() {
        assert!(fits_caption(&"a".repeat(1024)));
        assert!(!fits_caption(&"a".repeat(1025)));
        // Counted in UTF-16 units: an emoji takes two.
        assert!(!fits_caption(&"😀".repeat(513)));
        let card = |title: Option<&str>, tone, image| Card {
            title: title.map(Into::into),
            tone,
            text: "body".into(),
            image,
        };
        let chart = Image {
            name: "chart.png".into(),
            alt: "Bar chart".into(),
            width: 1200,
            height: 800,
            png: vec![1, 2, 3],
        };
        let prompt = Prompt {
            text: "Sales".into(),
            cards: vec![
                card(Some("Notes"), Tone::Neutral, None),
                card(Some("Week & day"), Tone::Success, Some(chart.clone())),
            ],
            ..Prompt::default()
        };
        let (with, found) = image(&prompt).unwrap();
        assert_eq!(found, &chart);
        assert_eq!(heading(with).as_deref(), Some("<b>✅ Week &amp; day</b>"));
        assert_eq!(heading(&card(None, Tone::Neutral, None)), None);
        assert_eq!(
            text(&prompt),
            "Sales\n\n<b>Notes</b>\nbody\n\n<b>✅ Week &amp; day</b>\nbody"
        );
    }
}
