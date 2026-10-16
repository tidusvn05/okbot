//! Prompts: a message with buttons, and optionally a form behind a button, that asks a person
//! for a decision or for input the agent must not see (ADR 0007).
//!
//! An MCP server asks the runtime for a prompt; the channel shows it natively (Discord buttons
//! and modals) and reports the answer with [`InboundSink::answer`](crate::InboundSink::answer).
//! Answers never become chat messages and never reach the agent.

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
}

/// A message with buttons, shown in one conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Unique id, chosen by the runtime.
    pub id: String,
    /// The message, already rendered for the channel.
    pub text: String,
    /// One to five buttons.
    pub buttons: Vec<PromptButton>,
    /// Who may answer; anyone in the conversation when empty. Others are told it is not theirs.
    pub allowed: Vec<UserId>,
}

/// Limits every channel can honour (Discord's are the tightest).
pub const MAX_BUTTONS: usize = 5;
/// Fields per form.
pub const MAX_FIELDS: usize = 5;

impl Prompt {
    /// Checks the limits and ids; the message says what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        let id_ok = |s: &str| {
            !s.is_empty()
                && s.len() <= 32
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if self.buttons.is_empty() || self.buttons.len() > MAX_BUTTONS {
            return Err(format!("a prompt needs 1 to {MAX_BUTTONS} buttons"));
        }
        if self.text.trim().is_empty() {
            return Err("a prompt needs text".into());
        }
        let mut seen = std::collections::HashSet::new();
        for b in &self.buttons {
            if !id_ok(&b.id) || !seen.insert(b.id.as_str()) {
                return Err(format!(
                    "button id {:?}: unique, 1-32 of a-z A-Z 0-9 - _",
                    b.id
                ));
            }
            if b.label.trim().is_empty() || b.label.chars().count() > 80 {
                return Err(format!("button {:?}: label of 1-80 characters", b.id));
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
                    if !id_ok(&f.id) || !fields.insert(f.id.as_str()) {
                        return Err(format!(
                            "field id {:?}: unique, 1-32 of a-z A-Z 0-9 - _",
                            f.id
                        ));
                    }
                    if f.label.trim().is_empty() || f.label.chars().count() > 45 {
                        return Err(format!("field {:?}: label of 1-45 characters", f.id));
                    }
                }
            }
        }
        Ok(())
    }

    /// The button `id`.
    pub fn button(&self, id: &str) -> Option<&PromptButton> {
        self.buttons.iter().find(|b| b.id == id)
    }

    /// Whether `user` may answer.
    pub fn may_answer(&self, user: &UserId) -> bool {
        self.allowed.is_empty() || self.allowed.contains(user)
    }
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
            allowed: vec![],
        }
    }

    fn button(id: &str, form: Option<Form>) -> PromptButton {
        PromptButton {
            id: id.into(),
            label: "OK".into(),
            style: ButtonStyle::Primary,
            form,
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
