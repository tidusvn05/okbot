//! Safe edits of an existing `okbot.toml` (`okbot channel`, `okbot people`).
//!
//! Comments and layout are kept (`toml_edit`). [`ConfigEditor::save`] writes the file, loads it
//! again with every validation rule, and restores the previous text when that fails, so a command
//! never leaves a bot with a configuration it cannot start with.

use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table};

use crate::{CONFIG_FILE, Config, ConfigError};

/// An `okbot.toml` being edited.
#[derive(Debug)]
pub struct ConfigEditor {
    dir: PathBuf,
    original: String,
    doc: DocumentMut,
}

impl ConfigEditor {
    /// Reads `dir/okbot.toml`.
    pub fn open(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join(CONFIG_FILE);
        let original = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let doc = original
            .parse::<DocumentMut>()
            .map_err(|e| ConfigError::Parse {
                path,
                message: e.to_string(),
            })?;
        Ok(Self {
            dir: dir.to_path_buf(),
            original,
            doc,
        })
    }

    /// Appends TOML text (new sections) at the end of the file.
    pub fn append(&mut self, text: &str) -> Result<(), ConfigError> {
        let mut combined = self.doc.to_string();
        if !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(text);
        self.doc = combined
            .parse::<DocumentMut>()
            .map_err(|e| ConfigError::Invalid(format!("generated TOML does not parse: {e}")))?;
        Ok(())
    }

    /// Sets `[bot] profile`.
    pub fn set_profile(&mut self, profile: crate::BotProfile) {
        let value = match profile {
            crate::BotProfile::Team => "team",
            crate::BotProfile::Personal => "personal",
        };
        if let Some(bot) = self.doc.get_mut("bot").and_then(Item::as_table_like_mut) {
            match bot.get_mut("profile").and_then(Item::as_value_mut) {
                Some(existing) => {
                    // Keep the comment after the value.
                    let decor = existing.decor().clone();
                    *existing = toml_edit::Value::from(value);
                    *existing.decor_mut() = decor;
                }
                None => {
                    bot.insert("profile", toml_edit::value(value));
                }
            }
        }
    }

    /// Whether `[http] listen` is set.
    pub fn has_http_listen(&self) -> bool {
        self.doc.get("http").and_then(|h| h.get("listen")).is_some()
    }

    /// Removes `[channels.<kind>.<instance>]` (and `[channels.<kind>]` when it becomes empty).
    /// Returns whether it existed.
    pub fn remove_channel(&mut self, kind: &str, instance: &str) -> bool {
        let Some(channels) = self
            .doc
            .get_mut("channels")
            .and_then(Item::as_table_like_mut)
        else {
            return false;
        };
        let Some(platform) = channels.get_mut(kind).and_then(Item::as_table_like_mut) else {
            return false;
        };
        let removed = platform.remove(instance).is_some();
        if platform.is_empty() {
            channels.remove(kind);
        }
        removed
    }

    /// Adds `accounts` to `[people.<person>] accounts` (creating the person). Returns the
    /// accounts that were not linked yet.
    pub fn link_accounts(&mut self, person: &str, accounts: &[String]) -> Vec<String> {
        let accounts_array = self.person_accounts(person);
        let mut added = Vec::new();
        for account in accounts {
            let present = accounts_array
                .iter()
                .any(|v| v.as_str() == Some(account.as_str()));
            if !present {
                accounts_array.push(account.as_str());
                added.push(account.clone());
            }
        }
        added
    }

    /// Removes `accounts` from a person; the person goes when no account is left. Returns the
    /// accounts that were removed.
    pub fn unlink_accounts(&mut self, person: &str, accounts: &[String]) -> Vec<String> {
        let mut removed = Vec::new();
        let Some(people) = self.doc.get_mut("people").and_then(Item::as_table_like_mut) else {
            return removed;
        };
        let Some(entry) = people.get_mut(person).and_then(Item::as_table_like_mut) else {
            return removed;
        };
        if let Some(array) = entry.get_mut("accounts").and_then(Item::as_array_mut) {
            array.retain(|v| {
                let hit = v.as_str().is_some_and(|a| accounts.iter().any(|x| x == a));
                if hit {
                    removed.push(v.as_str().unwrap_or_default().to_string());
                }
                !hit
            });
            if array.is_empty() {
                people.remove(person);
            }
        }
        removed
    }

    /// Removes `[people.<person>]`. Returns whether it existed.
    pub fn remove_person(&mut self, person: &str) -> bool {
        self.doc
            .get_mut("people")
            .and_then(Item::as_table_like_mut)
            .is_some_and(|people| people.remove(person).is_some())
    }

    fn person_accounts(&mut self, person: &str) -> &mut Array {
        let people = self.doc.entry("people").or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        let entry = people
            .as_table_like_mut()
            .expect("[people] is a table: okbot.toml was validated on load")
            .entry(person)
            .or_insert_with(|| Item::Table(Table::new()));
        let entry = entry
            .as_table_like_mut()
            .expect("[people.<id>] is a table: okbot.toml was validated on load");
        if entry.get("accounts").and_then(Item::as_array).is_none() {
            entry.insert("accounts", toml_edit::value(Array::new()));
        }
        entry
            .get_mut("accounts")
            .and_then(Item::as_array_mut)
            .expect("accounts was just set to an array")
    }

    /// The edited text.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// Writes the file and loads it again; on a validation error the previous text is restored
    /// and the error returned.
    pub fn save(self) -> Result<Config, ConfigError> {
        let path = self.dir.join(CONFIG_FILE);
        let write = |text: &str| {
            std::fs::write(&path, text).map_err(|source| ConfigError::Read {
                path: path.clone(),
                source,
            })
        };
        write(&self.doc.to_string())?;
        match Config::load(&self.dir) {
            Ok(config) => Ok(config),
            Err(error) => {
                write(&self.original)?;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "# my bot\nconfig_version = 1\n\n[bot]\nname = \"x\" # keep me\n\n[channels.zalo.a]\ntoken_env = \"Z\"\n";

    fn bot() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), BASE).unwrap();
        dir
    }

    #[test]
    fn adds_and_removes_channels_keeping_comments() {
        let dir = bot();
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.append("\n[channels.discord.a]\ntoken_env = \"D\"\n")
            .unwrap();
        let config = e.save().unwrap();
        assert!(config.channels.discord.contains_key("a"));
        let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("# keep me") && text.contains("# my bot"));

        let mut e = ConfigEditor::open(dir.path()).unwrap();
        assert!(e.remove_channel("discord", "a"));
        assert!(!e.remove_channel("discord", "a"));
        let config = e.save().unwrap();
        assert_eq!(config.channels.discord.len(), 0);
        assert!(
            !std::fs::read_to_string(dir.path().join(CONFIG_FILE))
                .unwrap()
                .contains("discord")
        );
    }

    #[test]
    fn sets_the_profile() {
        let dir = bot();
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.set_profile(crate::BotProfile::Personal);
        assert_eq!(e.save().unwrap().bot.profile, crate::BotProfile::Personal);
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.set_profile(crate::BotProfile::Team);
        let config = e.save().unwrap();
        assert_eq!(config.bot.profile, crate::BotProfile::Team);
        let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("# keep me"), "{text}");
    }

    #[test]
    fn invalid_edits_are_rolled_back() {
        let dir = bot();
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.append("\n[channels.discord.a]\nunknown_key = 1\n")
            .unwrap();
        assert!(e.save().is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap(),
            BASE
        );
    }

    #[test]
    fn links_people() {
        let dir = bot();
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        let added = e.link_accounts("luan", &["zalo:a/u1".into(), "discord:a/9".into()]);
        assert_eq!(added.len(), 2);
        assert_eq!(
            e.link_accounts("luan", &["zalo:a/u1".into()]),
            Vec::<String>::new()
        );
        let config = e.save().unwrap();
        assert_eq!(config.people["luan"].accounts, ["zalo:a/u1", "discord:a/9"]);

        let mut e = ConfigEditor::open(dir.path()).unwrap();
        assert_eq!(
            e.unlink_accounts("luan", &["zalo:a/u1".into()]),
            ["zalo:a/u1"]
        );
        let config = e.save().unwrap();
        assert_eq!(config.people["luan"].accounts, ["discord:a/9"]);

        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.unlink_accounts("luan", &["discord:a/9".into()]);
        let config = e.save().unwrap();
        assert_eq!(config.people.len(), 0, "a person without accounts goes");

        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.link_accounts("luan", &["zalo:a/u1".into()]);
        e.link_accounts("minh", &["zalo:a/u1".into()]);
        assert!(e.save().is_err(), "one account, two people");
    }
}
