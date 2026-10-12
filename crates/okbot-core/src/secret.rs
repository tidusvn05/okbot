//! Secrets and redaction.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Deserializer};

/// A secret value that never shows up in `Debug` output or logs.
///
/// It deliberately implements neither `Display` nor `Serialize`: reading the value takes an explicit
/// [`Secret::expose`] call, which is easy to find in review.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps a value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The secret value. Do not log or format it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Secret)
    }
}

/// Replaces known secret values in text (log lines, error messages) with `[REDACTED]`.
///
/// Values shorter than 6 characters are ignored, because replacing them would mangle ordinary text.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Shortest value that is redacted.
    pub const MIN_LEN: usize = 6;

    /// An empty redactor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a secret to redact.
    pub fn add(&mut self, secret: &Secret<String>) {
        let value = secret.expose();
        if value.len() >= Self::MIN_LEN && !self.secrets.contains(value) {
            self.secrets.push(value.clone());
            // Longer secrets first, so a secret containing another is replaced whole.
            self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        }
    }

    /// The text with every known secret replaced.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = Cow::Owned(out.replace(secret.as_str(), "[REDACTED]"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_value() {
        let s = Secret::new("hunter22".to_string());
        assert_eq!(format!("{s:?}"), "Secret([REDACTED])");
        assert_eq!(s.expose(), "hunter22");
    }

    #[test]
    fn redactor_replaces_known_secrets() {
        let mut r = Redactor::new();
        r.add(&Secret::new("123456:abc-xyz".to_string()));
        r.add(&Secret::new("short".to_string()));
        assert_eq!(
            r.redact("GET https://bot-api/bot123456:abc-xyz/getMe short"),
            "GET https://bot-api/bot[REDACTED]/getMe short"
        );
        assert!(matches!(r.redact("nothing here"), Cow::Borrowed(_)));
    }
}
