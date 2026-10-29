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
    /// Replaces the active fallback order, keeping per-provider settings for later reuse.
    pub fn set_agent_chain(&mut self, chain: &[String]) -> Result<(), ConfigError> {
        let mut seen = std::collections::HashSet::new();
        if chain.iter().any(|name| !seen.insert(name)) {
            return Err(ConfigError::Invalid(
                "agent chain must not repeat a provider".into(),
            ));
        }
        let values: Array = chain.iter().map(String::as_str).collect();
        self.set_agent_value(None, "chain", Some(toml_edit::Value::Array(values)))
    }

    /// Sets or removes one provider override (`model` or `cli`).
    pub fn set_agent_provider(
        &mut self,
        provider: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), ConfigError> {
        self.set_agent_value(Some(provider), key, value.map(toml_edit::Value::from))
    }

    fn set_agent_value(
        &mut self,
        provider: Option<&str>,
        key: &str,
        value: Option<toml_edit::Value>,
    ) -> Result<(), ConfigError> {
        let mut table: &mut dyn toml_edit::TableLike = self.doc.as_table_mut();
        let mut path = vec!["agent"];
        if let Some(provider) = provider {
            path.extend(["providers", provider]);
        }
        for part in path {
            if !table.contains_key(part) {
                if value.is_none() {
                    return Ok(());
                }
                let mut child = Table::new();
                child.set_implicit(true);
                table.insert(part, Item::Table(child));
            }
            table = table
                .get_mut(part)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| {
                    ConfigError::Invalid(format!("agent configuration `{part}` must be a table"))
                })?;
        }
        if let Some(mut value) = value {
            if let Some(existing) = table.get(key).and_then(Item::as_value) {
                *value.decor_mut() = existing.decor().clone();
            }
            table.insert(key, Item::Value(value));
        } else {
            table.remove(key);
        }
        Ok(())
    }

    /// Validates the edited text without writing it.
    pub fn validate(&self) -> Result<Config, ConfigError> {
        Config::from_text(&self.dir, &self.doc.to_string())
    }
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
        self.validate()?;
        let path = self.dir.join(CONFIG_FILE);
        let current = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        if current != self.original {
            return Err(ConfigError::Invalid(
                "okbot.toml changed while editing; retry the command".into(),
            ));
        }
        let write = |text: &str| {
            replace_file(&path, text).map_err(|source| ConfigError::Read {
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

/// Replaces `path` with `text` at once (a sibling temporary file, then a rename), so a running bot
/// that re-reads the file never sees half of it. The file keeps its permissions.
fn replace_file(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let name = path.file_name().map_or_else(
        || std::ffi::OsString::from("okbot.toml"),
        std::ffi::OsStr::to_os_string,
    );
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(&name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = std::fs::write(&tmp, text).and_then(|()| {
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
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
    fn saves_by_rename_and_keeps_the_mode() {
        let dir = bot();
        let file = dir.path().join(CONFIG_FILE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&file).unwrap().ino()
        };
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.append("\n[channels.discord.a]\ntoken_env = \"D\"\n")
            .unwrap();
        e.save().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = std::fs::metadata(&file).unwrap();
            assert_ne!(meta.ino(), inode, "replaced, not rewritten in place");
            assert_eq!(meta.permissions().mode() & 0o777, 0o640);
        }
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            [std::ffi::OsString::from(CONFIG_FILE)],
            "no temporary file"
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
    fn agent_edits_preview_before_saving_and_keep_inactive_overrides() {
        let dir = bot();
        let original = format!(
            "{BASE}\n[agent]\nchain = [\"claude\"] # order\n\n[agent.providers.claude]\nmodel = \"sonnet@high\" # model\n"
        );
        std::fs::write(dir.path().join(CONFIG_FILE), &original).unwrap();
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.set_agent_chain(&["codex".into()]).unwrap();
        e.set_agent_provider("claude", "model", Some("sonnet@low"))
            .unwrap();
        assert_eq!(e.validate().unwrap().agent.chain, ["codex"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap(),
            original
        );
        let c = e.save().unwrap();
        assert_eq!(
            c.agent.providers["claude"].model.as_deref(),
            Some("sonnet@low")
        );
        let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("# order") && text.contains("# model") && text.contains("# keep me"));
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        assert!(
            e.set_agent_chain(&["codex".into(), "codex".into()])
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap(),
            text
        );
    }

    #[test]
    fn edits_inline_agent_tables_without_panicking() {
        let dir = bot();
        std::fs::write(dir.path().join(CONFIG_FILE),
            "config_version = 1\nagent = { chain = [\"claude\"], providers = { claude = { model = \"sonnet\" } } }\n[bot]\nname = \"x\"\n").unwrap();
        let mut e = ConfigEditor::open(dir.path()).unwrap();
        e.set_agent_provider("claude", "model", Some("sonnet@high"))
            .unwrap();
        assert_eq!(
            e.save().unwrap().agent.providers["claude"].model.as_deref(),
            Some("sonnet@high")
        );
    }

    #[test]
    fn changed_files_and_invalid_table_shapes_are_preserved() {
        let dir = bot();
        let mut editor = ConfigEditor::open(dir.path()).unwrap();
        editor.set_agent_chain(&["codex".into()]).unwrap();
        let external = format!("{BASE}\n# an operator changed this file\n");
        std::fs::write(dir.path().join(CONFIG_FILE), &external).unwrap();
        assert!(editor.save().is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap(),
            external
        );
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "config_version = 1\nagent = \"bad\"\n[bot]\nname = \"x\"\n",
        )
        .unwrap();
        let mut editor = ConfigEditor::open(dir.path()).unwrap();
        assert!(editor.set_agent_chain(&["codex".into()]).is_err());
    }

    #[test]
    fn legacy_repeated_providers_still_load() {
        let dir = bot();
        let text = format!("{BASE}\n[agent]\nchain = [\"claude\", \"claude\"]\n");
        assert_eq!(
            Config::from_text(dir.path(), &text).unwrap().agent.chain,
            ["claude", "claude"]
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
