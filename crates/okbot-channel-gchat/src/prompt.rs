//! Prompts on Google Chat: the message text with `cardsV2`. Each [`Card`] of a prompt is a card
//! of its own; the menu and the buttons share a last card. Every button and the menu invoke the
//! action [`FUNCTION`] with the prompt id (and the button id) as parameters; Chat reports the click
//! as a `CARD_CLICKED` event (or an add-on `buttonClickedPayload`).
//!
//! A button with a form opens a dialog. Chat opens dialogs only as the synchronous answer to the
//! click, so forms need HTTP delivery (ADR 0007, ADR 0015).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use okbot_core::{ButtonStyle, Card, FieldStyle, Form, Prompt, PromptButton, ReplyTarget, Secret};
use serde_json::{Value, json};

/// The action name of okbot's buttons.
pub(crate) const FUNCTION: &str = "okp";

/// The widget name of a prompt's menu (its value arrives in the event's form inputs).
pub(crate) const SELECT_INPUT: &str = "okp_select";

/// A prompt waiting for a click.
#[derive(Clone)]
pub(crate) struct Open {
    /// The prompt (who may answer, its buttons and forms).
    pub(crate) prompt: Prompt,
    /// Its cards, to keep them when the text changes.
    pub(crate) cards: Value,
    /// Where it is.
    pub(crate) target: ReplyTarget,
    /// Its message name.
    pub(crate) name: String,
}

impl Open {
    pub(crate) fn new(prompt: &Prompt, target: ReplyTarget, name: String) -> Self {
        Self {
            prompt: prompt.clone(),
            cards: cards(prompt),
            target,
            name,
        }
    }
}

fn param(key: &str, value: &str) -> Value {
    json!({"key": key, "value": value})
}

/// A Chat `Color` from an RGB value.
fn color(rgb: u32) -> Value {
    let part = |shift: u32| (f64::from((rgb >> shift) & 0xFF) / 2.55).round() / 100.0;
    json!({"red": part(16), "green": part(8), "blue": part(0), "alpha": 1})
}

fn button(prompt: &Prompt, b: &PromptButton) -> Value {
    let on_click = match &b.url {
        Some(url) => json!({"openLink": {"url": url}}),
        None => {
            let mut action = json!({
                "function": FUNCTION,
                "parameters": [param("prompt", &prompt.id), param("button", &b.id)],
            });
            if b.form.is_some() {
                action["interaction"] = json!("OPEN_DIALOG");
            }
            json!({"action": action})
        }
    };
    let mut out = json!({"text": b.label, "onClick": on_click});
    match b.style {
        ButtonStyle::Secondary => out["type"] = json!("OUTLINED"),
        ButtonStyle::Success => out["color"] = color(okbot_core::Tone::Success.color()),
        ButtonStyle::Danger => out["color"] = color(okbot_core::Tone::Error.color()),
        _ => {}
    }
    out
}

fn card(prompt: &Prompt, index: usize, card: &Card) -> Value {
    let emoji = card.tone.emoji();
    let title = card
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let text = card.text.trim();
    let with_emoji = |s: &str| {
        if emoji.is_empty() {
            s.to_string()
        } else {
            format!("{emoji} {s}")
        }
    };
    let body = match (title, text.is_empty()) {
        (Some(title), false) => json!({
            "header": {"title": with_emoji(title)},
            "sections": [{"widgets": [{"textParagraph": {"text": html(text)}}]}],
        }),
        // A header needs a section under it: a lone title is bold text instead.
        (Some(title), true) => json!({
            "sections": [{"widgets": [{"textParagraph": {
                "text": format!("<b>{}</b>", escape(&with_emoji(title))),
            }}]}],
        }),
        (None, _) => {
            let text = if emoji.is_empty() {
                html(text)
            } else {
                format!("{emoji} {}", html(text))
            };
            json!({"sections": [{"widgets": [{"textParagraph": {"text": text}}]}]})
        }
    };
    json!({"cardId": format!("okp-{}-c{}", prompt.id, index + 1), "card": body})
}

/// The `cardsV2` list of a prompt: its cards, then its menu and buttons.
pub(crate) fn cards(prompt: &Prompt) -> Value {
    let mut out: Vec<Value> = prompt
        .cards
        .iter()
        .enumerate()
        .map(|(i, c)| card(prompt, i, c))
        .collect();
    let mut widgets = Vec::new();
    if let Some(select) = &prompt.select {
        // A dropdown always shows an item: the placeholder is an empty first item, so that
        // picking any real option is a change.
        let mut items = vec![json!({"text": select.placeholder, "value": "", "selected": true})];
        items.extend(select.options.iter().map(|o| {
            let text = match o.description.as_deref().filter(|d| !d.trim().is_empty()) {
                Some(d) => format!("{} — {d}", o.label),
                None => o.label.clone(),
            };
            json!({"text": text, "value": o.id, "selected": false})
        }));
        widgets.push(json!({"selectionInput": {
            "name": SELECT_INPUT,
            "type": "DROPDOWN",
            "items": items,
            "onChangeAction": {
                "function": FUNCTION,
                "parameters": [param("prompt", &prompt.id), param("select", "1")],
            },
        }}));
    }
    if !prompt.buttons.is_empty() {
        let buttons: Vec<Value> = prompt.buttons.iter().map(|b| button(prompt, b)).collect();
        widgets.push(json!({"buttonList": {"buttons": buttons}}));
    }
    if !widgets.is_empty() {
        out.push(json!({
            "cardId": format!("okp-{}", prompt.id),
            "card": {"sections": [{"widgets": widgets}]},
        }));
    }
    Value::Array(out)
}

/// The dialog card of `button`'s form: one text input per field and a submit button.
pub(crate) fn dialog(prompt: &Prompt, button: &PromptButton, form: &Form) -> Value {
    let mut widgets: Vec<Value> = form
        .fields
        .iter()
        .map(|f| {
            let mut input = json!({
                "name": f.id,
                "label": f.label,
                "type": match f.style {
                    FieldStyle::Paragraph => "MULTIPLE_LINE",
                    _ => "SINGLE_LINE",
                },
            });
            if let Some(value) = &f.value {
                input["value"] = json!(value);
            }
            if let Some(hint) = &f.placeholder {
                input["hintText"] = json!(hint);
            }
            if let Some(max) = f.max_len {
                input["validation"] = json!({"characterLimit": max});
            }
            json!({"textInput": input})
        })
        .collect();
    let required: Vec<&str> = form
        .fields
        .iter()
        .filter(|f| f.required)
        .map(|f| f.id.as_str())
        .collect();
    let mut action = json!({
        "function": FUNCTION,
        "parameters": [
            param("prompt", &prompt.id),
            param("button", &button.id),
            param("submit", "1"),
        ],
    });
    if !required.is_empty() {
        action["requiredWidgets"] = json!(required);
    }
    widgets.push(json!({"buttonList": {"buttons": [{
        "text": button.label,
        "onClick": {"action": action},
    }]}}));
    json!({"header": {"title": form.title}, "sections": [{"widgets": widgets}]})
}

/// Checks submitted values against `form` and keeps only its fields (empty when not sent).
pub(crate) fn form_values(
    form: &Form,
    inputs: &BTreeMap<String, Secret<String>>,
) -> Result<BTreeMap<String, Secret<String>>, String> {
    let mut values = BTreeMap::new();
    for field in &form.fields {
        let value = inputs
            .get(&field.id)
            .map(|v| v.expose().clone())
            .unwrap_or_default();
        let len = value.trim().chars().count();
        if field.required && len == 0 {
            return Err(format!("{} is required.", field.label));
        }
        if len > 0 {
            if field.min_len.is_some_and(|min| len < usize::from(min)) {
                return Err(format!(
                    "{}: at least {} characters.",
                    field.label,
                    field.min_len.unwrap_or_default()
                ));
            }
            if field.max_len.is_some_and(|max| len > usize::from(max)) {
                return Err(format!(
                    "{}: at most {} characters.",
                    field.label,
                    field.max_len.unwrap_or_default()
                ));
            }
        }
        values.insert(field.id.clone(), Secret::new(value));
    }
    Ok(values)
}

/// The synchronous answer that opens a dialog.
pub(crate) fn open_dialog(add_on: bool, card: &Value) -> Value {
    if add_on {
        json!({"action": {"navigations": [{"pushCard": card}]}})
    } else {
        json!({"actionResponse": {"type": "DIALOG", "dialogAction": {"dialog": {"body": card}}}})
    }
}

/// The synchronous answer that closes a dialog.
pub(crate) fn close_dialog(add_on: bool) -> Value {
    if add_on {
        json!({"action": {"navigations": [{"endNavigation": {"action": "CLOSE_DIALOG"}}]}})
    } else {
        json!({"actionResponse": {"type": "DIALOG",
               "dialogAction": {"actionStatus": {"statusCode": "OK"}}}})
    }
}

/// The synchronous answer that shows `message` and keeps a dialog open (or opens none).
pub(crate) fn dialog_error(add_on: bool, message: &str) -> Value {
    if add_on {
        json!({"action": {"notification": {"text": message}}})
    } else {
        json!({"actionResponse": {"type": "DIALOG", "dialogAction": {"actionStatus": {
            "statusCode": "INVALID_ARGUMENT", "userFacingMessage": message}}}})
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>"),
            c => out.push(c),
        }
    }
    out
}

/// Google Chat message formatting (`*bold*`, `_italic_`, `~strike~`, `<url|text>`) → the HTML
/// subset of card text widgets. Everything else is escaped; code stays as typed.
pub(crate) fn html(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let rest = &chars[i..];
        // Code blocks and inline code: kept verbatim (escaped).
        if rest.starts_with(&['`', '`', '`']) {
            let end = find(&chars, i + 3, &['`', '`', '`']).map_or(chars.len(), |e| e + 3);
            out.push_str(&escape(&chars[i..end].iter().collect::<String>()));
            i = end;
            continue;
        }
        if c == '`'
            && let Some(end) = find_on_line(&chars, i + 1, '`')
        {
            out.push_str(&escape(&chars[i..=end].iter().collect::<String>()));
            i = end + 1;
            continue;
        }
        // `<url|text>` and `<url>` links.
        if c == '<'
            && let Some(end) = find_on_line(&chars, i + 1, '>')
        {
            let inner: String = chars[i + 1..end].iter().collect();
            let (url, label) = inner.split_once('|').unwrap_or((&inner, &inner));
            if (url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("mailto:"))
                && !url.contains(char::is_whitespace)
            {
                let _ = write!(out, "<a href=\"{}\">{}</a>", escape(url), escape(label));
                i = end + 1;
                continue;
            }
        }
        if let Some(tag) = match c {
            '*' => Some("b"),
            '_' => Some("i"),
            '~' => Some("s"),
            _ => None,
        } {
            let opens = i == 0 || !chars[i - 1].is_alphanumeric();
            let next_ok = chars
                .get(i + 1)
                .is_some_and(|n| !n.is_whitespace() && *n != c);
            if opens
                && next_ok
                && let Some(end) = closing(&chars, i + 1, c)
            {
                let inner: String = chars[i + 1..end].iter().collect();
                let _ = write!(out, "<{tag}>{}</{tag}>", html(&inner));
                i = end + 1;
                continue;
            }
        }
        out.push_str(&escape(&c.to_string()));
        i += 1;
    }
    out
}

fn find(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j..].starts_with(needle))
}

fn find_on_line(chars: &[char], from: usize, c: char) -> Option<usize> {
    chars
        .iter()
        .enumerate()
        .skip(from)
        .take_while(|(_, x)| **x != '\n')
        .find(|(_, x)| **x == c)
        .map(|(j, _)| j)
}

/// The closing `marker` on the same line: after a non-space, not followed by a letter or digit.
fn closing(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from + 1..chars.len())
        .take_while(|&j| chars[j] != '\n')
        .find(|&j| {
            chars[j] == marker
                && !chars[j - 1].is_whitespace()
                && chars.get(j + 1).is_none_or(|n| !n.is_alphanumeric())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{FormField, Select, SelectOption, Tone};

    fn link(id: &str, url: &str) -> PromptButton {
        PromptButton {
            id: id.into(),
            label: "Docs".into(),
            style: ButtonStyle::Primary,
            form: None,
            url: Some(url.into()),
        }
    }

    fn button(id: &str, style: ButtonStyle, form: Option<Form>) -> PromptButton {
        PromptButton {
            id: id.into(),
            label: id.to_uppercase(),
            style,
            form,
            url: None,
        }
    }

    fn form() -> Form {
        Form {
            title: "Your details".into(),
            fields: vec![
                FormField {
                    id: "f1".into(),
                    label: "Name".into(),
                    style: FieldStyle::Short,
                    required: true,
                    placeholder: Some("Full name".into()),
                    min_len: Some(2),
                    max_len: Some(40),
                    value: Some("An".into()),
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
        }
    }

    fn interactive() -> Prompt {
        Prompt {
            id: "v1a2b3c4d".into(),
            text: "Here is your order:".into(),
            cards: vec![
                Card {
                    title: Some("Order #12".into()),
                    tone: Tone::Success,
                    text: "• *Tea* × 2\n• <https://shop.example/t|Details> & more".into(),
                    image: None,
                },
                Card {
                    title: None,
                    tone: Tone::Neutral,
                    text: "Delivery _tomorrow_".into(),
                    image: None,
                },
            ],
            select: Some(Select {
                placeholder: "Pick a size".into(),
                options: vec![
                    SelectOption {
                        id: "o1".into(),
                        label: "Small".into(),
                        description: None,
                    },
                    SelectOption {
                        id: "o2".into(),
                        label: "Large".into(),
                        description: Some("+10k".into()),
                    },
                ],
            }),
            buttons: vec![
                button("b1", ButtonStyle::Success, None),
                button("b2", ButtonStyle::Secondary, Some(form())),
                link("b3", "https://shop.example/help"),
            ],
            allowed: Vec::new(),
        }
    }

    /// `value` with its keys sorted: the order of a JSON object depends on whether another crate
    /// of the build turns on serde_json's `preserve_order`.
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let map: std::collections::BTreeMap<String, Value> =
                    map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
                Value::Object(map.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            other => other,
        }
    }

    #[test]
    fn interactive_prompt_cards() {
        insta::assert_json_snapshot!(sorted(cards(&interactive())));
    }

    #[test]
    fn form_dialog() {
        let prompt = interactive();
        let b = &prompt.buttons[1];
        let card = dialog(&prompt, b, b.form.as_ref().expect("a form button"));
        insta::assert_json_snapshot!(sorted(open_dialog(false, &card)));
        assert_eq!(
            open_dialog(true, &card)["action"]["navigations"][0]["pushCard"],
            card
        );
        assert_eq!(
            close_dialog(false)["actionResponse"]["dialogAction"]["actionStatus"]["statusCode"],
            "OK"
        );
        assert_eq!(
            close_dialog(true)["action"]["navigations"][0]["endNavigation"]["action"],
            "CLOSE_DIALOG"
        );
    }

    #[test]
    fn form_values_are_checked() {
        let form = form();
        let inputs = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Secret::new(v.to_string())))
                .collect::<BTreeMap<_, _>>()
        };
        let values = form_values(&form, &inputs(&[("f1", "Lan"), ("x", "y")])).unwrap();
        assert_eq!(values.len(), 2, "only the form's fields");
        assert_eq!(values["f1"].expose(), "Lan");
        assert_eq!(values["f2"].expose(), "");
        assert!(form_values(&form, &inputs(&[("f2", "x")])).is_err());
        assert!(form_values(&form, &inputs(&[("f1", "L")])).is_err());
        assert!(form_values(&form, &inputs(&[("f1", &"x".repeat(41))])).is_err());
    }

    #[test]
    fn plain_prompts_keep_their_shape() {
        let prompt = Prompt {
            id: "abc".into(),
            text: "Deploy?".into(),
            buttons: vec![button("allow", ButtonStyle::Primary, None)],
            ..Prompt::default()
        };
        let cards = cards(&prompt);
        assert_eq!(cards.as_array().map(Vec::len), Some(1));
        assert_eq!(
            cards[0]["card"]["sections"][0]["widgets"][0]["buttonList"]["buttons"][0],
            json!({"text": "ALLOW", "onClick": {"action": {"function": "okp", "parameters": [
                {"key": "prompt", "value": "abc"}, {"key": "button", "value": "allow"}]}}})
        );
    }

    #[test]
    fn chat_formatting_to_card_html() {
        assert_eq!(
            html("*bold* and _it_ ~x~"),
            "<b>bold</b> and <i>it</i> <s>x</s>"
        );
        assert_eq!(html("a < b & c"), "a &lt; b &amp; c");
        assert_eq!(
            html("<https://e.com/?a=1&b=2|site>"),
            "<a href=\"https://e.com/?a=1&amp;b=2\">site</a>"
        );
        assert_eq!(html("snake_case_name 2*3*4"), "snake_case_name 2*3*4");
        assert_eq!(html("`*not bold*`"), "`*not bold*`");
        assert_eq!(html("one\ntwo"), "one<br>two");
        assert_eq!(html("*_both_*"), "<b><i>both</i></b>");
        assert_eq!(html("<script>"), "&lt;script&gt;");
    }
}
