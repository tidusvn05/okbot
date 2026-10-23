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
    /// `<a href="url">text</a>`
    Html,
}

#[derive(Debug, Clone, Copy)]
enum CodeBlockStyle {
    Fenced,
    /// Every line indented by two of the given space, which also replaces the line's own leading
    /// spaces.
    Indented(char),
    /// `<pre><code>` with the code escaped.
    Html,
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
    /// `& < >` as entities (Slack, Telegram HTML).
    Entities,
}

/// Opening and closing markup around a span.
type Marker = Option<(&'static str, &'static str)>;

/// The same markup on both sides (`**bold**`).
const fn same(m: &'static str) -> (&'static str, &'static str) {
    (m, m)
}

#[derive(Debug, Clone, Copy)]
struct Style {
    strong: Marker,
    emphasis: Marker,
    strike: Marker,
    code: Marker,
    link: LinkStyle,
    /// Headings as `#` lines (true) or as strong text (false).
    heading_hashes: bool,
    code_block: CodeBlockStyle,
    table: TableStyle,
    bullet: &'static str,
    /// Prefix of every quoted line; empty for `<blockquote>`.
    quote: &'static str,
    /// The prefix of a `-# ` note line; without one, notes are written in emphasis.
    note: Option<&'static str>,
    escape: Escape,
}

impl Style {
    fn of(dialect: MarkdownDialect) -> Self {
        match dialect {
            MarkdownDialect::ZaloMd => Self {
                strong: Some(same("**")),
                emphasis: Some(same("_")),
                strike: Some(same("~~")),
                code: Some(same("`")),
                link: LinkStyle::Plain,
                heading_hashes: false,
                // Zalo drops leading spaces; no-break spaces stay and copy as spaces.
                code_block: CodeBlockStyle::Indented('\u{a0}'),
                table: TableStyle::List,
                bullet: "- ",
                quote: "> ",
                note: None,
                escape: Escape::MarkdownLite,
            },
            MarkdownDialect::GChatText => Self {
                strong: Some(same("*")),
                emphasis: Some(same("_")),
                strike: Some(same("~")),
                code: Some(same("`")),
                link: LinkStyle::Angle,
                heading_hashes: false,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::List,
                bullet: "• ",
                quote: "> ",
                note: None,
                escape: Escape::None,
            },
            MarkdownDialect::SlackMrkdwn => Self {
                strong: Some(same("*")),
                emphasis: Some(same("_")),
                strike: Some(same("~")),
                code: Some(same("`")),
                link: LinkStyle::Angle,
                heading_hashes: false,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::List,
                bullet: "• ",
                quote: "> ",
                note: None,
                escape: Escape::Entities,
            },
            MarkdownDialect::DiscordMd => Self {
                strong: Some(same("**")),
                emphasis: Some(same("_")),
                strike: Some(same("~~")),
                code: Some(same("`")),
                link: LinkStyle::Markdown,
                heading_hashes: true,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::List,
                bullet: "- ",
                quote: "> ",
                note: Some("-# "),
                escape: Escape::Discord,
            },
            MarkdownDialect::TelegramHtml => Self {
                strong: Some(("<b>", "</b>")),
                emphasis: Some(("<i>", "</i>")),
                strike: Some(("<s>", "</s>")),
                code: Some(("<code>", "</code>")),
                link: LinkStyle::Html,
                heading_hashes: false,
                code_block: CodeBlockStyle::Html,
                table: TableStyle::List,
                bullet: "• ",
                quote: "",
                note: None,
                escape: Escape::Entities,
            },
            MarkdownDialect::Plain => Self {
                strong: None,
                emphasis: None,
                strike: None,
                code: None,
                link: LinkStyle::Plain,
                heading_hashes: false,
                code_block: CodeBlockStyle::Indented(' '),
                table: TableStyle::List,
                bullet: "- ",
                quote: "> ",
                note: None,
                escape: Escape::None,
            },
            // CommonMark and dialects added later fall back to CommonMark.
            _ => Self {
                strong: Some(same("**")),
                emphasis: Some(same("_")),
                strike: Some(same("~~")),
                code: Some(same("`")),
                link: LinkStyle::Markdown,
                heading_hashes: true,
                code_block: CodeBlockStyle::Fenced,
                table: TableStyle::Markdown,
                bullet: "- ",
                quote: "> ",
                note: None,
                escape: Escape::None,
            },
        }
    }
}

/// A rendered top-level block, the unit the splitter packs into messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chunk {
    Text(String),
    /// A fenced code block: the splitter re-wraps each part in the same `open` and `close`.
    Fenced {
        open: String,
        body: String,
        close: String,
    },
}

/// Renders blocks into chunks, one per top-level block.
pub(crate) fn emit(blocks: &[Block], dialect: MarkdownDialect) -> Vec<Chunk> {
    let style = Style::of(dialect);
    blocks
        .iter()
        .filter_map(|block| {
            if let Block::Code { lang, text } = block {
                match style.code_block {
                    CodeBlockStyle::Fenced => {
                        return Some(Chunk::Fenced {
                            open: format!("```{}\n", lang.as_deref().unwrap_or("")),
                            body: text.clone(),
                            close: "\n```".into(),
                        });
                    }
                    CodeBlockStyle::Html => {
                        return Some(Chunk::Fenced {
                            open: html_pre(lang.as_deref()),
                            body: escape(text, Escape::Entities),
                            close: "</code></pre>".into(),
                        });
                    }
                    CodeBlockStyle::Indented(_) => {}
                }
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
        Block::Quote(blocks) => {
            let inner = blocks
                .iter()
                .map(|b| block_text(b, style, ""))
                .collect::<Vec<_>>()
                .join("\n\n");
            if style.quote.is_empty() {
                format!("<blockquote>{inner}</blockquote>")
            } else {
                inner
                    .lines()
                    .map(|line| format!("{}{line}", style.quote))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        Block::Code { lang, text } => match style.code_block {
            CodeBlockStyle::Fenced => {
                format!("```{}\n{text}\n```", lang.as_deref().unwrap_or(""))
            }
            CodeBlockStyle::Html => format!(
                "{}{}</code></pre>",
                html_pre(lang.as_deref()),
                escape(text, Escape::Entities)
            ),
            CodeBlockStyle::Indented(space) => text
                .lines()
                .map(|line| {
                    let code = line.trim_start_matches(' ');
                    let depth = 2 + line.len() - code.len();
                    format!(
                        "{}{}",
                        String::from(space).repeat(depth),
                        escape(code, style.escape)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        },
        Block::Table { header, rows } => table_text(header, rows, style),
        Block::Note(inlines) => {
            let text = inline_text(inlines, style);
            match style.note {
                Some(prefix) => format!("{prefix}{text}"),
                None => wrap(&text, style.emphasis),
            }
        }
        Block::Rule => match (style.escape, style.link) {
            (Escape::MarkdownLite, _) | (_, LinkStyle::Html) => "———".to_string(),
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

fn wrap(text: &str, marker: Marker) -> String {
    match marker {
        Some((open, close)) if !text.trim().is_empty() => format!("{open}{text}{close}"),
        _ => text.to_string(),
    }
}

/// `<pre><code>` opening an HTML code block, with the language as a class when it is safe.
fn html_pre(lang: Option<&str>) -> String {
    match lang.filter(|l| {
        l.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '#'))
    }) {
        Some(lang) if !lang.is_empty() => format!("<pre><code class=\"language-{lang}\">"),
        _ => "<pre><code>".to_string(),
    }
}

fn inline_text(inlines: &[Inline], style: &Style) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(&escape(t, style.escape)),
            Inline::Code(c) => match style.code {
                // Markdown code spans show their content verbatim; HTML still needs entities.
                Some((open, close)) if matches!(style.link, LinkStyle::Html) => {
                    let _ = write!(out, "{open}{}{close}", escape(c, Escape::Entities));
                }
                Some((open, close)) => {
                    let _ = write!(out, "{open}{c}{close}");
                }
                None => out.push_str(c),
            },
            Inline::Strong(i) => out.push_str(&wrap(&inline_text(i, style), style.strong)),
            Inline::Emphasis(i) => out.push_str(&wrap(&inline_text(i, style), style.emphasis)),
            Inline::Strike(i) => out.push_str(&wrap(&inline_text(i, style), style.strike)),
            Inline::Link { url, text } => {
                let label = inline_text(text, style);
                let bare = plain(text);
                // Only web and mail links: Slack reads `<!channel|x>` as a mention of everyone.
                let web = ["https://", "http://", "mailto:"]
                    .iter()
                    .any(|p| url.to_ascii_lowercase().starts_with(p));
                // URLs are written as-is in Markdown dialects; entity dialects need `&` escaped.
                let shown = match style.escape {
                    Escape::Entities => escape(url, Escape::Entities),
                    _ => url.clone(),
                };
                let _ = match style.link {
                    _ if !web && bare.is_empty() => write!(out, "{}", escape(url, style.escape)),
                    _ if !web => write!(out, "{label}"),
                    _ if bare.is_empty() || bare == *url => write!(out, "{shown}"),
                    LinkStyle::Markdown => write!(out, "[{label}]({url})"),
                    LinkStyle::Angle => {
                        write!(out, "<{shown}|{}>", bare.replace(['<', '>', '|'], ""))
                    }
                    LinkStyle::Plain => write!(out, "{label} ({url})"),
                    LinkStyle::Html => write!(
                        out,
                        "<a href=\"{}\">{label}</a>",
                        shown.replace('"', "&quot;")
                    ),
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
