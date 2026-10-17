//! Slash commands: the bot's commands in Discord's `/` menu. A chosen command arrives as an
//! interaction and is handed to the runtime as the text message `/name value…`.

use okbot_core::CommandInfo;
use twilight_model::application::command::{
    Command, CommandOption, CommandOptionType, CommandType,
};
use twilight_model::application::interaction::application_command::{
    CommandData, CommandOptionValue,
};
use twilight_model::id::Id;

/// Discord's locales (application command localization).
const LOCALES: &[&str] = &[
    "id", "da", "de", "en-GB", "en-US", "es-ES", "es-419", "fr", "hr", "it", "lt", "hu", "nl",
    "no", "pl", "pt-BR", "ro", "fi", "sv-SE", "vi", "tr", "cs", "el", "bg", "ru", "uk", "hi", "th",
    "zh-CN", "ja", "zh-TW", "ko",
];

/// Language codes (`en`, `ja`) as Discord locales (`en-US` and `en-GB`, `ja`); unknown ones are left out.
fn localizations(texts: &[(String, String)]) -> Option<std::collections::HashMap<String, String>> {
    let map: std::collections::HashMap<String, String> = texts
        .iter()
        .flat_map(|(code, text)| {
            LOCALES
                .iter()
                .filter(move |l| l.split('-').next() == Some(code.as_str()))
                .map(move |l| ((*l).to_string(), clip(text, 100)))
        })
        .collect();
    (!map.is_empty()).then_some(map)
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    let text = if text.is_empty() { "-" } else { text };
    text.chars().take(max).collect()
}

/// Discord's command definitions (every parameter is a string option).
pub(crate) fn definitions(commands: &[CommandInfo]) -> Vec<Command> {
    commands
        .iter()
        .map(|c| {
            #[allow(deprecated, reason = "the struct still has dm_permission")]
            Command {
                application_id: None,
                contexts: None,
                default_member_permissions: None,
                dm_permission: None,
                description: clip(&c.description, 100),
                description_localizations: localizations(&c.descriptions),
                guild_id: None,
                id: None,
                integration_types: None,
                kind: CommandType::ChatInput,
                name: c.name.clone(),
                name_localizations: None,
                nsfw: None,
                options: c
                    .params
                    .iter()
                    .map(|p| CommandOption {
                        autocomplete: None,
                        channel_types: None,
                        choices: None,
                        description: clip(&p.name.replace(['_', '-'], " "), 100),
                        description_localizations: None,
                        kind: CommandOptionType::String,
                        max_length: None,
                        max_value: None,
                        min_length: None,
                        min_value: None,
                        name: p.name.to_ascii_lowercase(),
                        name_localizations: None,
                        options: None,
                        required: Some(p.required),
                    })
                    .collect(),
                version: Id::new(1),
            }
        })
        .collect()
}

/// The text message a chosen command stands for: `/name a "b c"` (values with spaces quoted).
pub(crate) fn invocation(data: &CommandData) -> String {
    let mut text = format!("/{}", data.name);
    for option in &data.options {
        let value = match &option.value {
            CommandOptionValue::String(s) => s.clone(),
            CommandOptionValue::Integer(i) => i.to_string(),
            CommandOptionValue::Number(n) => n.to_string(),
            CommandOptionValue::Boolean(b) => b.to_string(),
            _ => continue,
        };
        let value = value.replace('"', "'");
        text.push(' ');
        if value.contains(char::is_whitespace) || value.is_empty() {
            text.push('"');
            text.push_str(&value);
            text.push('"');
        } else {
            text.push_str(&value);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::CommandParam;

    #[test]
    fn definitions_and_invocations() {
        let defs = definitions(&[CommandInfo {
            name: "allow".into(),
            description: "Allow a LINE user".into(),
            descriptions: vec![
                ("ja".into(), "LINE ユーザーを許可".into()),
                ("en".into(), "Allow".into()),
            ],
            params: vec![
                CommandParam {
                    name: "company".into(),
                    required: true,
                    rest: false,
                },
                CommandParam {
                    name: "note".into(),
                    required: false,
                    rest: true,
                },
            ],
        }]);
        let json = serde_json::to_value(&defs).unwrap();
        assert_eq!(json[0]["name"], "allow");
        assert_eq!(json[0]["type"], 1);
        assert_eq!(json[0]["options"][0]["type"], 3);
        assert_eq!(json[0]["options"][0]["required"], true);
        assert_eq!(json[0]["options"][1]["required"], false);
        assert_eq!(
            json[0]["description_localizations"]["ja"],
            "LINE ユーザーを許可"
        );
        assert_eq!(json[0]["description_localizations"]["en-GB"], "Allow");
        assert_eq!(json[0]["description_localizations"]["en-US"], "Allow");

        let data: CommandData = serde_json::from_value(serde_json::json!({
            "id": "1", "name": "allow", "type": 1,
            "options": [
                {"name": "company", "type": 3, "value": "acme"},
                {"name": "note", "type": 3, "value": "Lan \"sales\""}
            ]
        }))
        .unwrap();
        assert_eq!(invocation(&data), "/allow acme \"Lan 'sales'\"");
    }
}
