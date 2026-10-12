//! Writes blocks in a platform dialect.

use std::fmt::Write as _;

use okbot_core::MarkdownDialect;

use crate::ir::{Block, Inline, plain};

#[derive(Debug, Clone, Copy)]
enum LinkStyle {
    /// `[text](url)`
    Markdown,
    /// `<url|text>` (Slack, Google Chat)
    Angle,
    /// `text (url)`
    Plain,
}

#[derive(Debug, Clone, Copy)]
enum CodeBlockStyle {
    Fenced,
    Indented,
}

#[derive(Debug, Clone, Copy)]
enum TableStyle {
    Markdown,
    List,
}

#[derive(Debug, Clone, Copy)]
enum Escape {
    None,
    /// Backslash before `* _ ~ \``.
    MarkdownLite,
    /// Backslash before Discord's markup characters.
    Discord,
    /// `& < >` as entities (Slack).
    Entities,
}

#[derive(Debug, Clone, Copy)]
struct Style {
    strong: Option<&'static str>,
    emphasis: Option<&'static str>,
    strike: Option<&'static str>,
    code: Option<&'static str>,
    link: LinkStyle,
    /// Headings as `#` lines (true) or as strong text (false).
    heading_hashes: bool,
    code_block: CodeBlockStyle,
    table: TableStyle,
    bullet: &'static str,
    quote: &'static str,
    escape: Escape,
}

impl Style {
    fn of(dialect: MarkdownDialect) -> Self {
        match dialect {
            MarkdownDialect::ZaloMd => Self {
                strong: Some("**"),
                emphasis: Some("_"),
                strike: Some("~~"),
                code: Some("`"),
                link: LinkStyle::Plain,
                heading_hashes: false,
                code_block: CodeBlockStyle::Indented,
                table: TableStyle::List,
                bullet: "- ",
                quote: "> ",
                escape: Escape::MarkdownLite,
            },
            MarkdownDialect::GChatText => Self {
                strong: Some("*"),
                emphasis: Some("_"),
                strike: Some("~"),
                code: Some("`"),
                link: LinkStyle::Angle,
                heading_hashes: false,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::List,
                bullet: "• ",
                quote: "> ",
                escape: Escape::None,
            },
            MarkdownDialect::SlackMrkdwn => Self {
                strong: Some("*"),
                emphasis: Some("_"),
                strike: Some("~"),
                code: Some("`"),
                link: LinkStyle::Angle,
                heading_hashes: false,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::List,
                bullet: "• ",
                quote: "> ",
                escape: Escape::Entities,
            },
            MarkdownDialect::DiscordMd => Self {
                strong: Some("**"),
                emphasis: Some("_"),
                strike: Some("~~"),
                code: Some("`"),
                link: LinkStyle::Markdown,
                heading_hashes: true,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::List,
                bullet: "- ",
                quote: "> ",
                escape: Escape::Discord,
            },
            MarkdownDialect::Plain => Self {
                strong: None,
                emphasis: None,
                strike: None,
                code: None,
                link: LinkStyle::Plain,
                heading_hashes: false,
                code_block: CodeBlockStyle::Indented,
                table: TableStyle::List,
                bullet: "- ",
                quote: "> ",
                escape: Escape::None,
            },
            // CommonMark and dialects added later fall back to CommonMark.
            _ => Self {
                strong: Some("**"),
                emphasis: Some("_"),
                strike: Some("~~"),
                code: Some("`"),
                link: LinkStyle::Markdown,
                heading_hashes: true,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::Markdown,
                bullet: "- ",
                quote: "> ",
                escape: Escape::None,
            },
        }
    }
}

/// A rendered top-level block, the unit the splitter packs into messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chunk {
    Text(String),
    /// A fenced code block: the splitter re-wraps each part in the same fences.
    Fenced {
        open: String,
        body: String,
    },
}

/// Renders blocks into chunks, one per top-level block.
pub(crate) fn emit(blocks: &[Block], dialect: MarkdownDialect) -> Vec<Chunk> {
    let style = Style::of(dialect);
    blocks
        .iter()
        .filter_map(|block| {
            if let (Block::Code { lang, text }, CodeBlockStyle::Fenced) = (block, style.code_block)
            {
                return Some(Chunk::Fenced {
                    open: format!("```{}", lang.as_deref().unwrap_or("")),
                    body: text.clone(),
                });
            }
            let text = block_text(block, &style, "");
            (!text.trim().is_empty()).then_some(Chunk::Text(text))
        })
        .collect()
}

fn block_text(block: &Block, style: &Style, indent: &str) -> String {
    match block {
        Block::Paragraph(inlines) => inline_text(inlines, style),
        Block::Heading(level, inlines) => {
            let text = inline_text(inlines, style);
            if style.heading_hashes {
                format!("{} {text}", "#".repeat(usize::from(*level)))
            } else {
                wrap(&text, style.strong)
            }
        }
        Block::List { start, items } => {
            let mut lines = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let marker = match start {
                    Some(n) => format!("{}. ", n + i as u64),
                    None => style.bullet.to_string(),
                };
                // Content under an item lines up with the text after its marker.
                let inner = format!("{indent}{}", " ".repeat(marker.chars().count()));
                let mut first = true;
                for block in item {
                    if let Block::List { .. } = block {
                        lines.push(block_text(block, style, &inner));
                        first = false;
                        continue;
                    }
                    for line in block_text(block, style, "").lines() {
                        if first {
                            lines.push(format!("{indent}{marker}{line}"));
                            first = false;
                        } else {
                            lines.push(format!("{inner}{line}"));
                        }
                    }
                }
                if first {
                    lines.push(format!("{indent}{}", marker.trim_end()));
                }
            }
            lines.join("\n")
        }
        Block::Quote(blocks) => blocks
            .iter()
            .map(|b| block_text(b, style, ""))
            .collect::<Vec<_>>()
            .join("\n\n")
            .lines()
            .map(|line| format!("{}{line}", style.quote))
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Code { lang, text } => match style.code_block {
            CodeBlockStyle::Fenced => {
                format!("```{}\n{text}\n```", lang.as_deref().unwrap_or(""))
            }
            CodeBlockStyle::Indented => text
                .lines()
                .map(|line| format!("  {}", escape(line, style.escape)))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        Block::Table { header, rows } => table_text(header, rows, style),
        Block::Rule => match style.escape {
            Escape::MarkdownLite => "———".to_string(),
            _ => "---".to_string(),
        },
    }
}

fn table_text(header: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>], style: &Style) -> String {
    match style.table {
        TableStyle::Markdown => {
            let row = |cells: &[Vec<Inline>]| {
                let cells: Vec<String> = cells.iter().map(|c| inline_text(c, style)).collect();
                format!("| {} |", cells.join(" | "))
            };
            let mut lines = vec![row(header)];
            lines.push(format!("|{}|", vec!["---"; header.len().max(1)].join("|")));
            lines.extend(rows.iter().map(|r| row(r)));
            lines.join("\n")
        }
        TableStyle::List => {
            let names: Vec<String> = header.iter().map(|h| plain(h)).collect();
            rows.iter()
                .map(|cells| {
                    let parts: Vec<String> = cells
                        .iter()
                        .enumerate()
                        .map(|(i, cell)| {
                            let value = inline_text(cell, style);
                            match names.get(i).filter(|n| !n.trim().is_empty()) {
                                Some(name) => format!("{}: {value}", escape(name, style.escape)),
                                None => value,
                            }
                        })
                        .collect();
                    format!("{}{}", style.bullet, parts.join("; "))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn wrap(text: &str, marker: Option<&str>) -> String {
    match marker {
        Some(m) if !text.trim().is_empty() => format!("{m}{text}{m}"),
        _ => text.to_string(),
    }
}

fn inline_text(inlines: &[Inline], style: &Style) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(&escape(t, style.escape)),
            Inline::Code(c) => match style.code {
                Some(m) => {
                    let _ = write!(out, "{m}{c}{m}");
                }
                None => out.push_str(c),
            },
            Inline::Strong(i) => out.push_str(&wrap(&inline_text(i, style), style.strong)),
            Inline::Emphasis(i) => out.push_str(&wrap(&inline_text(i, style), style.emphasis)),
            Inline::Strike(i) => out.push_str(&wrap(&inline_text(i, style), style.strike)),
            Inline::Link { url, text } => {
                let label = inline_text(text, style);
                let bare = plain(text);
                let _ = match style.link {
                    _ if bare.is_empty() || bare == *url => write!(out, "{url}"),
                    LinkStyle::Markdown => write!(out, "[{label}]({url})"),
                    LinkStyle::Angle => write!(out, "<{url}|{bare}>"),
                    LinkStyle::Plain => write!(out, "{label} ({url})"),
                };
            }
            Inline::Break => out.push('\n'),
        }
    }
    out
}

fn escape(text: &str, escape: Escape) -> String {
    let specials: &[char] = match escape {
        Escape::None => return text.to_string(),
        Escape::Entities => {
            return text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
        }
        Escape::MarkdownLite => &['\\', '*', '_', '~', '`'],
        Escape::Discord => &['\\', '*', '_', '~', '`', '|'],
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if specials.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
