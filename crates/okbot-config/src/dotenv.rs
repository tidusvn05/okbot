//! A minimal `.env` reader: `NAME=value` lines, `#` comments, optional quotes.
//!
//! Values stay in okbot's memory and are never exported to the process environment, so agent
//! processes cannot inherit them.

use std::collections::HashMap;

pub(crate) fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let value = value.trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))
            .unwrap_or(value);
        out.insert(name.to_string(), value.to_string());
    }
    out
}

/// Formats one `.env` line, quoting when needed.
pub(crate) fn line(name: &str, value: &str) -> String {
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '#' || c == '"')
    {
        format!("{name}='{value}'")
    } else {
        format!("{name}={value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let env = parse("# comment\nA=1\nexport B=\"two words\"\nC='x=y'\nbad line\n 1X=no\nD=\n");
        assert_eq!(env["A"], "1");
        assert_eq!(env["B"], "two words");
        assert_eq!(env["C"], "x=y");
        assert_eq!(env["D"], "");
        assert!(!env.contains_key("bad line"));
    }

    #[test]
    fn lines_round_trip() {
        for value in ["plain", "with space", "12345:abc-xyz"] {
            assert_eq!(parse(&line("K", value))["K"], value);
        }
    }
}
