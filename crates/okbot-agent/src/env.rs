//! Keeping okbot's secrets out of agent processes.

use std::ffi::OsString;

/// Decides which environment variables are removed before an agent CLI starts.
///
/// Agent CLIs inherit okbot's environment, which holds platform tokens. A prompt-injected agent
/// must not be able to read them, so every variable named here, and every variable whose name
/// starts with one of the prefixes, is removed from the child.
#[derive(Debug, Clone)]
pub struct EnvScrub {
    names: Vec<String>,
    prefixes: Vec<String>,
}

impl Default for EnvScrub {
    fn default() -> Self {
        Self {
            names: vec![
                "GOOGLE_APPLICATION_CREDENTIALS".into(),
                "DISCORD_TOKEN".into(),
            ],
            prefixes: vec![
                "OKBOT_".into(),
                "ZALO_".into(),
                "SLACK_".into(),
                "DISCORD_".into(),
                "GCHAT_".into(),
            ],
        }
    }
}

impl EnvScrub {
    /// Also removes `name` (for example a configured `token_env`).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        if !self.names.contains(&name) {
            self.names.push(name);
        }
        self
    }

    /// Also removes every variable starting with `prefix`.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefixes.push(prefix.into());
        self
    }

    /// The variables to remove, given the current environment's variable names.
    pub fn names_to_remove(&self, current: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
        let mut out: Vec<OsString> = self.names.iter().map(OsString::from).collect();
        for name in current {
            let Some(text) = name.to_str() else { continue };
            if self.prefixes.iter().any(|p| text.starts_with(p.as_str())) && !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// The variables to remove from the current process environment.
    pub fn current(&self) -> Vec<OsString> {
        self.names_to_remove(std::env::vars_os().map(|(k, _)| k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_names_and_prefixes() {
        let scrub = EnvScrub::default().with_name("MY_TOKEN");
        let removed = scrub.names_to_remove(
            ["PATH", "ZALO_BOT_TOKEN", "OKBOT_X", "HOME", "MY_TOKEN"]
                .into_iter()
                .map(OsString::from),
        );
        assert!(removed.contains(&OsString::from("ZALO_BOT_TOKEN")));
        assert!(removed.contains(&OsString::from("OKBOT_X")));
        assert!(removed.contains(&OsString::from("MY_TOKEN")));
        assert!(!removed.contains(&OsString::from("PATH")));
        assert_eq!(
            removed.iter().filter(|n| *n == "MY_TOKEN").count(),
            1,
            "no duplicates"
        );
    }
}
