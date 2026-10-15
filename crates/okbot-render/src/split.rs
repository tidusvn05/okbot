//! Packs rendered chunks into messages within a platform's length limit.

use okbot_core::LengthUnit;

use crate::emit::Chunk;

/// Length of `s` as the platform counts it.
pub(crate) fn measure(s: &str, unit: LengthUnit) -> usize {
    match unit {
        LengthUnit::Utf16 => s.encode_utf16().count(),
        _ => s.chars().count(),
    }
}

fn char_len(c: char, unit: LengthUnit) -> usize {
    match unit {
        LengthUnit::Utf16 => c.len_utf16(),
        _ => 1,
    }
}

/// Greedily packs chunks (joined by a blank line) into parts of at most `max` units.
pub(crate) fn pack(chunks: Vec<Chunk>, max: usize, unit: LengthUnit) -> Vec<String> {
    let max = max.max(1);
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for chunk in chunks {
        let pieces = match chunk {
            Chunk::Text(text) => {
                if measure(&text, unit) <= max {
                    vec![text]
                } else {
                    split_text(&text, max, unit)
                }
            }
            Chunk::Fenced { open, body, close } => fenced(&open, &body, &close, max, unit),
        };
        for piece in pieces {
            if current.is_empty() {
                current = piece;
                continue;
            }
            let joined_len = measure(&current, unit) + 2 + measure(&piece, unit);
            if joined_len <= max {
                current.push_str("\n\n");
                current.push_str(&piece);
            } else {
                parts.push(std::mem::replace(&mut current, piece));
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Splits a code block into parts that each carry the opening and closing fence.
fn fenced(open: &str, body: &str, close: &str, max: usize, unit: LengthUnit) -> Vec<String> {
    let whole = format!("{open}{body}{close}");
    if measure(&whole, unit) <= max {
        return vec![whole];
    }
    let overhead = measure(open, unit) + measure(close, unit);
    if overhead >= max {
        // The fences alone do not fit: send the code as plain text.
        return split_text(body, max, unit);
    }
    split_text(body, max - overhead, unit)
        .into_iter()
        .map(|part| format!("{open}{part}{close}"))
        .collect()
}

/// Splits text at line, then word, then character boundaries.
fn split_text(text: &str, max: usize, unit: LengthUnit) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for line in text.split('\n') {
        let pieces = if measure(line, unit) <= max {
            vec![line.to_string()]
        } else {
            split_line(line, max, unit)
        };
        for piece in pieces {
            if current.is_empty() {
                current = piece;
            } else if measure(&current, unit) + 1 + measure(&piece, unit) <= max {
                current.push('\n');
                current.push_str(&piece);
            } else {
                parts.push(std::mem::replace(&mut current, piece));
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Splits one long line at the last space before the limit, or hard at the limit.
fn split_line(line: &str, max: usize, unit: LengthUnit) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = line;
    while measure(rest, unit) > max {
        let mut used = 0;
        let mut cut = 0;
        let mut last_space = None;
        for (i, c) in rest.char_indices() {
            let len = char_len(c, unit);
            if used + len > max {
                break;
            }
            used += len;
            cut = i + c.len_utf8();
            if c == ' ' {
                last_space = Some(i);
            }
        }
        if cut == 0 {
            // A single character wider than `max` (a surrogate pair with max 1): take it anyway.
            cut = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        let (head, tail) = match last_space {
            Some(space) if space > 0 => (&rest[..space], &rest[space + 1..]),
            _ => (&rest[..cut], &rest[cut..]),
        };
        parts.push(head.to_string());
        rest = tail;
    }
    if !rest.is_empty() {
        parts.push(rest.to_string());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_blocks_under_limit() {
        let chunks = vec![
            Chunk::Text("aaaa".into()),
            Chunk::Text("bbbb".into()),
            Chunk::Text("cccc".into()),
        ];
        assert_eq!(
            pack(chunks, 10, LengthUnit::Chars),
            vec!["aaaa\n\nbbbb", "cccc"]
        );
    }

    #[test]
    fn splits_long_line_at_spaces() {
        assert_eq!(
            split_line("one two three four", 9, LengthUnit::Chars),
            vec!["one two", "three", "four"]
        );
    }

    #[test]
    fn code_parts_keep_fences() {
        let body = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let parts = pack(
            vec![Chunk::Fenced {
                open: "```rust\n".into(),
                body,
                close: "\n```".into(),
            }],
            60,
            LengthUnit::Chars,
        );
        assert!(parts.len() > 1);
        for part in &parts {
            assert!(
                part.starts_with("```rust\n") && part.ends_with("\n```"),
                "{part}"
            );
            assert!(part.chars().count() <= 60);
        }
    }

    #[test]
    fn utf16_counts_surrogate_pairs() {
        assert_eq!(measure("a😀", LengthUnit::Utf16), 3);
        let parts = split_line("😀😀😀", 4, LengthUnit::Utf16);
        assert_eq!(parts, vec!["😀😀", "😀"]);
    }
}
