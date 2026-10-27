//! Prompts: a message with buttons, and optionally a form behind a button, that asks a person
//! for a decision or for input the agent must not see (ADR 0007).
//!
//! An MCP server asks the runtime for a prompt; the channel shows it natively (Discord buttons
//! and modals) and reports the answer with [`InboundSink::answer`](crate::InboundSink::answer).
//! Answers never become chat messages and never reach the agent.
//!
//! In interactive replies (ADR 0015) the agent shows a prompt too: cards, buttons, a menu of
//! options or a form. Those answers do become the clicker's next message.

use std::collections::BTreeMap;

use crate::id::{ConversationKey, UserId, UserRef};
use crate::message::SentRef;
use crate::secret::Secret;

/// What a button looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ButtonStyle {
    /// The main action.
    #[default]
    Primary,
    /// A neutral action (cancel).
    Secondary,
    /// A safe, positive action.
    Success,
    /// A destructive or risky action.
    Danger,
}

/// How much text a form field takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FieldStyle {
    /// One line.
    #[default]
    Short,
    /// Several lines (keys, certificates).
    Paragraph,
}

/// One text field of a [`Form`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FormField {
    /// Key of the value in the answer.
    pub id: String,
    /// Label shown above the field.
    pub label: String,
    /// One line or several.
    #[serde(default)]
    pub style: FieldStyle,
    /// The person must fill it in.
    #[serde(default = "yes")]
    pub required: bool,
    /// Hint shown in the empty field.
    #[serde(default)]
    pub placeholder: Option<String>,
    /// Minimum length.
    #[serde(default)]
    pub min_len: Option<u16>,
    /// Maximum length.
    #[serde(default)]
    pub max_len: Option<u16>,
    /// Filled in when the form opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

fn yes() -> bool {
    true
}

/// A small form (a Discord modal: up to five text fields).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Form {
    /// Title of the form.
    pub title: String,
    /// Fields, in order.
    pub fields: Vec<FormField>,
}

/// A button under the prompt message. With a [`Form`], pressing it opens the form and the
/// answer carries the values.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PromptButton {
    /// Key of the button in the answer.
    pub id: String,
    /// Text on the button.
    pub label: String,
    /// Look.
    #[serde(default)]
    pub style: ButtonStyle,
    /// A form to fill in before the answer is sent.
    #[serde(default)]
    pub form: Option<Form>,
    /// A link button: opens this `https` URL and sends no answer (no form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What a [`Card`] means; sets its colour (or a leading emoji) where the platform has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Tone {
    /// No particular meaning: the bot's own colour.
    #[default]
    Neutral,
    /// Information.
    Info,
    /// Something went well.
    Success,
    /// Needs attention.
    Warning,
    /// Something failed.
    Error,
}

impl Tone {
    /// An emoji for platforms without coloured cards (empty for [`Tone::Neutral`]).
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Neutral => "",
            Self::Info => "ℹ️",
            Self::Success => "✅",
            Self::Warning => "⚠️",
            Self::Error => "❌",
        }
    }

    /// An RGB colour for platforms with coloured cards.
    pub fn color(self) -> u32 {
        match self {
            Self::Neutral => 0x0058_65F2,
            Self::Info => 0x003B_82F6,
            Self::Success => 0x0022_C55E,
            Self::Warning => 0x00EA_B308,
            Self::Error => 0x00EF_4444,
        }
    }
}

/// A block of structured content in a prompt: a title and text (ADR 0015).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
pub struct Card {
    /// Plain text title.
    #[serde(default)]
    pub title: Option<String>,
    /// Meaning and colour.
    #[serde(default)]
    pub tone: Tone,
    /// Body, already rendered for the channel (like [`Prompt::text`]).
    pub text: String,
    /// A chart drawn as a picture, shown with the card (ADR 0016). Only on channels with
    /// [`UiCaps::images`](crate::UiCaps::images); at most one per prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<crate::Image>,
}

/// One entry of a [`Select`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SelectOption {
    /// Key of the option in the answer ([`PromptAnswer::button`]).
    pub id: String,
    /// Text shown.
    pub label: String,
    /// A second line, where the platform shows one.
    #[serde(default)]
    pub description: Option<String>,
}

/// A menu to pick one option from; the answer carries the option's id as its button.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Select {
    /// Shown while nothing is picked.
    pub placeholder: String,
    /// Two to [`MAX_OPTIONS`] options.
    pub options: Vec<SelectOption>,
}

/// A message with buttons, shown in one conversation. Interactive replies (ADR 0015) add cards
/// and a menu; a prompt with cards may have no text and nothing to answer.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
pub struct Prompt {
    /// Unique id, chosen by the runtime.
    pub id: String,
    /// The message, already rendered for the channel.
    pub text: String,
    /// Up to five buttons.
    pub buttons: Vec<PromptButton>,
    /// Who may answer; anyone in the conversation when empty. Others are told it is not theirs.
    pub allowed: Vec<UserId>,
    /// Cards shown after the text.
    #[serde(default)]
    pub cards: Vec<Card>,
    /// A menu shown before the buttons.
    #[serde(default)]
    pub select: Option<Select>,
}

/// Limits every channel can honour (Discord's are the tightest).
pub const MAX_BUTTONS: usize = 5;
/// Fields per form.
pub const MAX_FIELDS: usize = 5;
/// Cards per prompt.
pub const MAX_CARDS: usize = 5;
/// Options per menu.
pub const MAX_OPTIONS: usize = 25;
/// Characters of all the text of a prompt (Discord counts 4000 over all its text).
pub const MAX_PROMPT_CHARS: usize = 3800;

impl Prompt {
    /// Checks the limits and ids; the message says what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.buttons.len() > MAX_BUTTONS {
            return Err(format!("a prompt has at most {MAX_BUTTONS} buttons"));
        }
        if self.buttons.is_empty() && self.select.is_none() && self.cards.is_empty() {
            return Err(format!("a prompt needs 1 to {MAX_BUTTONS} buttons"));
        }
        if self.text.trim().is_empty() && self.cards.is_empty() {
            return Err("a prompt needs text".into());
        }
        self.validate_cards()?;
        let mut seen = std::collections::HashSet::new();
        self.validate_select(&mut seen)?;
        for b in &self.buttons {
            if !id_ok(&b.id, 32) || !seen.insert(b.id.as_str()) {
                return Err(format!(
                    "button id {:?}: unique, 1-32 of a-z A-Z 0-9 - _",
                    b.id
                ));
            }
            if b.label.trim().is_empty() || b.label.chars().count() > 80 {
                return Err(format!("button {:?}: label of 1-80 characters", b.id));
            }
            if let Some(url) = &b.url {
                if b.form.is_some() {
                    return Err(format!("button {:?}: a link has no form", b.id));
                }
                if !url.starts_with("https://")
                    || url.len() > 512
                    || url.contains(char::is_whitespace)
                {
                    return Err(format!("button {:?}: links must be https URLs", b.id));
                }
            }
            if let Some(form) = &b.form {
                if form.fields.is_empty() || form.fields.len() > MAX_FIELDS {
                    return Err(format!("form of {:?}: 1 to {MAX_FIELDS} fields", b.id));
                }
                if form.title.trim().is_empty() || form.title.chars().count() > 45 {
                    return Err(format!("form of {:?}: title of 1-45 characters", b.id));
                }
                let mut fields = std::collections::HashSet::new();
                for f in &form.fields {
                    if !id_ok(&f.id, 100) || !fields.insert(f.id.as_str()) {
                        return Err(format!(
                            "field id {:?}: unique, 1-100 of a-z A-Z 0-9 - _",
                            f.id
                        ));
                    }
                    if f.label.trim().is_empty() || f.label.chars().count() > 45 {
                        return Err(format!("field {:?}: label of 1-45 characters", f.id));
                    }
                    let max = f.max_len.map_or(4000, |m| m.min(4000));
                    if f.value
                        .as_ref()
                        .is_some_and(|v| v.chars().count() > usize::from(max))
                    {
                        return Err(format!(
                            "field {:?}: value of at most {max} characters",
                            f.id
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_cards(&self) -> Result<(), String> {
        if self.cards.len() > MAX_CARDS {
            return Err(format!("a prompt has at most {MAX_CARDS} cards"));
        }
        let mut chars = self.text.chars().count();
        for c in &self.cards {
            let title = c.title.as_deref().unwrap_or_default();
            if title.chars().count() > 100 {
                return Err("card title: at most 100 characters".into());
            }
            if c.text.trim().is_empty() && title.trim().is_empty() {
                return Err("a card needs a title or text".into());
            }
            // Discord counts bytes per text block.
            if c.text.len() > 1900 {
                return Err("card text: at most about 1900 bytes".into());
            }
            chars += title.chars().count() + c.text.chars().count();
        }
        if chars > MAX_PROMPT_CHARS {
            return Err(format!(
                "a prompt has at most {MAX_PROMPT_CHARS} characters of text"
            ));
        }
        Ok(())
    }

    fn validate_select<'a>(
        &'a self,
        seen: &mut std::collections::HashSet<&'a str>,
    ) -> Result<(), String> {
        if let Some(select) = &self.select {
            if select.options.len() < 2 || select.options.len() > MAX_OPTIONS {
                return Err(format!("a menu needs 2 to {MAX_OPTIONS} options"));
            }
            if select.placeholder.chars().count() > 150 {
                return Err("menu placeholder: at most 150 characters".into());
            }
            for o in &select.options {
                if !id_ok(&o.id, 32) || !seen.insert(o.id.as_str()) {
                    return Err(format!(
                        "option id {:?}: unique, 1-32 of a-z A-Z 0-9 - _",
                        o.id
                    ));
                }
                if o.label.trim().is_empty() || o.label.chars().count() > 100 {
                    return Err(format!("option {:?}: label of 1-100 characters", o.id));
                }
                if o.description
                    .as_ref()
                    .is_some_and(|d| d.chars().count() > 100)
                {
                    return Err(format!(
                        "option {:?}: description of at most 100 characters",
                        o.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// The button `id`.
    pub fn button(&self, id: &str) -> Option<&PromptButton> {
        self.buttons.iter().find(|b| b.id == id)
    }

    /// The menu option `id`.
    pub fn option(&self, id: &str) -> Option<&SelectOption> {
        self.select.as_ref()?.options.iter().find(|o| o.id == id)
    }

    /// Whether `id` names something that answers: a button that is not a link, or an option.
    pub fn answers_with(&self, id: &str) -> bool {
        self.button(id).is_some_and(|b| b.url.is_none()) || self.option(id).is_some()
    }

    /// Whether anything in it can be answered.
    pub fn is_answerable(&self) -> bool {
        self.select.is_some() || self.buttons.iter().any(|b| b.url.is_none())
    }

    /// Whether `user` may answer.
    pub fn may_answer(&self, user: &UserId) -> bool {
        self.allowed.is_empty() || self.allowed.contains(user)
    }
}

/// A valid id: 1 to `max` of `a-z A-Z 0-9 - _`. Button and option ids end up in a Discord custom
/// id with the prompt id (100 characters in all); field ids are custom ids of their own, so they
/// may be as long as Discord allows.
fn id_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Someone pressed a button (and filled in its form).
#[derive(Debug, Clone)]
pub struct PromptAnswer {
    /// [`Prompt::id`].
    pub prompt: String,
    /// [`PromptButton::id`].
    pub button: String,
    /// Form values by field id; never logged, stored or shown to the agent.
    pub values: BTreeMap<String, Secret<String>>,
    /// Who answered (the channel already checked [`Prompt::may_answer`]).
    pub user: UserRef,
    /// Where the prompt is.
    pub conversation: ConversationKey,
    /// The prompt message, to update with the outcome.
    pub sent: SentRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(buttons: Vec<PromptButton>) -> Prompt {
        Prompt {
            id: "p1".into(),
            text: "Deploy?".into(),
            buttons,
            ..Prompt::default()
        }
    }

    fn button(id: &str, form: Option<Form>) -> PromptButton {
        PromptButton {
            id: id.into(),
            label: "OK".into(),
            style: ButtonStyle::Primary,
            form,
            url: None,
        }
    }

    #[test]
    fn validates_limits_and_ids() {
        assert!(prompt(vec![button("ok", None)]).validate().is_ok());
        assert!(prompt(vec![]).validate().is_err());
        assert!(
            prompt(vec![button("ok", None), button("ok", None)])
                .validate()
                .is_err()
        );
        assert!(prompt(vec![button("a b", None)]).validate().is_err());
        let field = |id: &str| FormField {
            id: id.into(),
            label: "Code".into(),
            style: FieldStyle::Short,
            required: true,
            placeholder: None,
            min_len: None,
            max_len: None,
            value: None,
        };
        let form = |fields| Form {
            title: "Confirm".into(),
            fields,
        };
        assert!(
            prompt(vec![button("ok", Some(form(vec![field("otp")])))])
                .validate()
                .is_ok()
        );
        assert!(
            prompt(vec![button("ok", Some(form(vec![])))])
                .validate()
                .is_err()
        );
        assert!(
            prompt(vec![button("ok", Some(form(vec![field("a"), field("a")])))])
                .validate()
                .is_err()
        );
        let long = "LINE_ASSISTANT_CHANNEL_ACCESS_TOKEN";
        assert!(
            prompt(vec![button("ok", Some(form(vec![field(long)])))])
                .validate()
                .is_ok(),
            "variable names make natural field ids"
        );
        let filled = |value: &str, max_len| FormField {
            value: Some(value.into()),
            max_len,
            ..field("note")
        };
        let ok = |f: FormField| prompt(vec![button("ok", Some(form(vec![f])))]).validate();
        assert!(ok(filled("abc", Some(3))).is_ok());
        assert!(
            ok(filled("abcd", Some(3))).is_err(),
            "longer than the field"
        );
        assert!(
            ok(filled(&"x".repeat(4001), None)).is_err(),
            "Discord's limit"
        );
    }

    #[test]
    fn cards_menus_and_links() {
        let mut p = prompt(vec![]);
        assert!(p.validate().is_err(), "nothing to show but text");
        p.cards.push(Card {
            title: Some("Stock".into()),
            tone: Tone::Info,
            text: "• **A** · 3".into(),
            image: None,
        });
        assert!(p.validate().is_ok(), "cards alone are enough");
        assert!(!p.is_answerable());
        let option = |id: &str| SelectOption {
            id: id.into(),
            label: id.into(),
            description: None,
        };
        p.select = Some(Select {
            placeholder: "Pick".into(),
            options: vec![option("o1")],
        });
        assert!(p.validate().is_err(), "a menu of one");
        p.select = Some(Select {
            placeholder: "Pick".into(),
            options: vec![option("o1"), option("o2")],
        });
        assert!(p.validate().is_ok());
        assert!(p.answers_with("o2") && !p.answers_with("o3"));
        let mut link = button("docs", None);
        link.url = Some("http://example.com".into());
        p.buttons = vec![link.clone()];
        assert!(p.validate().is_err(), "only https");
        link.url = Some("https://example.com".into());
        p.buttons = vec![link];
        assert!(p.validate().is_ok());
        assert!(!p.answers_with("docs"), "links send no answer");
        p.buttons.push(button("o1", None));
        assert!(
            p.validate().is_err(),
            "ids are unique over options and buttons"
        );
    }

    #[test]
    fn who_may_answer() {
        let mut p = prompt(vec![button("ok", None)]);
        assert!(p.may_answer(&UserId::new("1")));
        p.allowed = vec![UserId::new("2")];
        assert!(!p.may_answer(&UserId::new("1")));
        assert!(p.may_answer(&UserId::new("2")));
    }
}
