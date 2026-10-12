//! Snapshot tests: one shared corpus rendered in every dialect.

use okbot_core::{ChannelCaps, LengthUnit, MarkdownDialect, Renderer};
use okbot_render::MarkdownRenderer;

fn render(dialect: MarkdownDialect, max: usize) -> String {
    let caps = ChannelCaps {
        max_message_len: max,
        length_unit: LengthUnit::Chars,
        dialect,
        ..ChannelCaps::basic()
    };
    let markdown = include_str!("corpus/mixed.md");
    MarkdownRenderer
        .render(markdown, &caps)
        .into_iter()
        .enumerate()
        .map(|(i, m)| format!("===== part {} =====\n{}", i + 1, m.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn commonmark() {
    insta::assert_snapshot!(render(MarkdownDialect::CommonMark, 4000));
}

#[test]
fn zalo() {
    insta::assert_snapshot!(render(MarkdownDialect::ZaloMd, 2000));
}

#[test]
fn gchat() {
    insta::assert_snapshot!(render(MarkdownDialect::GChatText, 4096));
}

#[test]
fn slack() {
    insta::assert_snapshot!(render(MarkdownDialect::SlackMrkdwn, 3000));
}

#[test]
fn discord() {
    insta::assert_snapshot!(render(MarkdownDialect::DiscordMd, 2000));
}

#[test]
fn plain() {
    insta::assert_snapshot!(render(MarkdownDialect::Plain, 4000));
}

#[test]
fn zalo_split_small() {
    insta::assert_snapshot!(render(MarkdownDialect::ZaloMd, 120));
}
