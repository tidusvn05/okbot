//! Property tests for splitting.

use okbot_core::{ChannelCaps, LengthUnit, MarkdownDialect, Renderer};
use okbot_render::MarkdownRenderer;
use proptest::prelude::*;

fn caps(dialect: MarkdownDialect, max: usize, unit: LengthUnit) -> ChannelCaps {
    ChannelCaps {
        max_message_len: max,
        length_unit: unit,
        dialect,
        ..ChannelCaps::basic()
    }
}

fn len(s: &str, unit: LengthUnit) -> usize {
    match unit {
        LengthUnit::Utf16 => s.encode_utf16().count(),
        _ => s.chars().count(),
    }
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn parts_fit_and_keep_words(
        text in "[a-zA-Zàáạảãđêôơư😀 ]{0,40}(\n\n[a-zA-Zàáạảãđêôơư😀 ]{0,60}){0,8}",
        max in 4usize..80,
        utf16 in any::<bool>(),
    ) {
        let unit = if utf16 { LengthUnit::Utf16 } else { LengthUnit::Chars };
        let parts = MarkdownRenderer.render(&text, &caps(MarkdownDialect::Plain, max, unit));
        for part in &parts {
            prop_assert!(len(&part.text, unit) <= max, "part {:?} longer than {}", part.text, max);
        }
        // Splitting may break a word only when it is longer than a whole message.
        let joined: String = parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ");
        let expected: String = words(&text).concat();
        prop_assert_eq!(joined.split_whitespace().collect::<String>(), expected);
    }
}
