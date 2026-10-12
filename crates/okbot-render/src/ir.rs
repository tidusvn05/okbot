//! A small document model between CommonMark and the platform dialects.

use std::fmt::Write as _;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Code {
        lang: Option<String>,
        text: String,
    },
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

/// An inline element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Inline {
    Text(String),
    Code(String),
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { url: String, text: Vec<Inline> },
    Break,
}

/// Parses CommonMark (with tables and strikethrough) into blocks.
pub(crate) fn parse(markdown: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let events: Vec<Event<'_>> = Parser::new_ext(markdown, options).collect();
    let mut pos = 0;
    parse_blocks(&events, &mut pos, None)
}

fn parse_blocks(events: &[Event<'_>], pos: &mut usize, until: Option<TagEnd>) -> Vec<Block> {
    let mut blocks = Vec::new();
    // Loose inline content (list items without paragraphs).
    let mut loose: Vec<Inline> = Vec::new();
    while *pos < events.len() {
        let event = &events[*pos];
        if let (Event::End(end), Some(until)) = (event, until)
            && *end == until
        {
            *pos += 1;
            break;
        }
        match event {
            Event::Start(Tag::Paragraph) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                blocks.push(Block::Paragraph(parse_inlines(
                    events,
                    pos,
                    TagEnd::Paragraph,
                )));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                let level = heading_level(*level);
                let end = TagEnd::Heading(match level {
                    1 => HeadingLevel::H1,
                    2 => HeadingLevel::H2,
                    3 => HeadingLevel::H3,
                    4 => HeadingLevel::H4,
                    5 => HeadingLevel::H5,
                    _ => HeadingLevel::H6,
                });
                blocks.push(Block::Heading(level, parse_inlines(events, pos, end)));
            }
            Event::Start(Tag::BlockQuote(kind)) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                let inner = parse_blocks(events, pos, Some(TagEnd::BlockQuote(*kind)));
                blocks.push(Block::Quote(inner));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                blocks.push(parse_code(events, pos, kind));
            }
            Event::Start(Tag::List(start)) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                blocks.push(parse_list(events, pos, *start));
            }
            Event::Start(Tag::Table(_)) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                blocks.push(parse_table(events, pos));
            }
            Event::Rule => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                blocks.push(Block::Rule);
            }
            Event::Start(Tag::HtmlBlock) | Event::End(_) => *pos += 1,
            Event::Html(html) => {
                *pos += 1;
                flush(&mut loose, &mut blocks);
                let text = html.trim_end().to_string();
                if !text.is_empty() {
                    blocks.push(Block::Paragraph(vec![Inline::Text(text)]));
                }
            }
            _ => {
                // Inline content directly inside a tight list item.
                if let Some(inline) = parse_inline(events, pos) {
                    loose.push(inline);
                } else {
                    *pos += 1;
                }
            }
        }
    }
    flush(&mut loose, &mut blocks);
    blocks
}

fn parse_code(events: &[Event<'_>], pos: &mut usize, kind: &CodeBlockKind<'_>) -> Block {
    let lang = match kind {
        CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
        _ => None,
    };
    let mut text = String::new();
    while *pos < events.len() {
        match &events[*pos] {
            Event::End(TagEnd::CodeBlock) => {
                *pos += 1;
                break;
            }
            Event::Text(t) => text.push_str(t),
            _ => {}
        }
        *pos += 1;
    }
    let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
    Block::Code { lang, text }
}

fn parse_list(events: &[Event<'_>], pos: &mut usize, start: Option<u64>) -> Block {
    let ordered = start.is_some();
    let mut items = Vec::new();
    while *pos < events.len() {
        match &events[*pos] {
            Event::Start(Tag::Item) => {
                *pos += 1;
                items.push(parse_blocks(events, pos, Some(TagEnd::Item)));
            }
            Event::End(TagEnd::List(o)) if *o == ordered => {
                *pos += 1;
                break;
            }
            _ => *pos += 1,
        }
    }
    Block::List { start, items }
}

fn flush(loose: &mut Vec<Inline>, blocks: &mut Vec<Block>) {
    if !loose.is_empty() {
        blocks.push(Block::Paragraph(std::mem::take(loose)));
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn parse_table(events: &[Event<'_>], pos: &mut usize) -> Block {
    let mut header = Vec::new();
    let mut rows = Vec::new();
    let mut row: Vec<Vec<Inline>> = Vec::new();
    while *pos < events.len() {
        match &events[*pos] {
            Event::End(TagEnd::Table) => {
                *pos += 1;
                break;
            }
            Event::End(TagEnd::TableHead) => {
                header = std::mem::take(&mut row);
                *pos += 1;
            }
            Event::End(TagEnd::TableRow) => {
                rows.push(std::mem::take(&mut row));
                *pos += 1;
            }
            Event::Start(Tag::TableCell) => {
                *pos += 1;
                row.push(parse_inlines(events, pos, TagEnd::TableCell));
            }
            _ => *pos += 1,
        }
    }
    Block::Table { header, rows }
}

fn parse_inlines(events: &[Event<'_>], pos: &mut usize, end: TagEnd) -> Vec<Inline> {
    let mut out = Vec::new();
    while *pos < events.len() {
        if let Event::End(e) = &events[*pos]
            && *e == end
        {
            *pos += 1;
            break;
        }
        match parse_inline(events, pos) {
            Some(inline) => out.push(inline),
            None => *pos += 1,
        }
    }
    out
}

/// Parses one inline element at `pos`, advancing past it. `None` for events that are not inline.
fn parse_inline(events: &[Event<'_>], pos: &mut usize) -> Option<Inline> {
    let inline = match &events[*pos] {
        Event::Text(t) => {
            *pos += 1;
            Inline::Text(t.to_string())
        }
        Event::Code(c) => {
            *pos += 1;
            Inline::Code(c.to_string())
        }
        Event::SoftBreak => {
            *pos += 1;
            Inline::Text(" ".into())
        }
        Event::HardBreak => {
            *pos += 1;
            Inline::Break
        }
        Event::InlineHtml(h) => {
            *pos += 1;
            Inline::Text(h.to_string())
        }
        Event::Start(Tag::Strong) => {
            *pos += 1;
            Inline::Strong(parse_inlines(events, pos, TagEnd::Strong))
        }
        Event::Start(Tag::Emphasis) => {
            *pos += 1;
            Inline::Emphasis(parse_inlines(events, pos, TagEnd::Emphasis))
        }
        Event::Start(Tag::Strikethrough) => {
            *pos += 1;
            Inline::Strike(parse_inlines(events, pos, TagEnd::Strikethrough))
        }
        Event::Start(Tag::Link { dest_url, .. }) => {
            *pos += 1;
            let url = dest_url.to_string();
            Inline::Link {
                url,
                text: parse_inlines(events, pos, TagEnd::Link),
            }
        }
        Event::Start(Tag::Image { dest_url, .. }) => {
            *pos += 1;
            let url = dest_url.to_string();
            Inline::Link {
                url,
                text: parse_inlines(events, pos, TagEnd::Image),
            }
        }
        _ => return None,
    };
    Some(inline)
}

/// The visible text of inlines, without any markup.
pub(crate) fn plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) => out.push_str(t),
            Inline::Strong(i) | Inline::Emphasis(i) | Inline::Strike(i) => out.push_str(&plain(i)),
            Inline::Link { url, text } => {
                let text = plain(text);
                if text.is_empty() || text == *url {
                    out.push_str(url);
                } else {
                    let _ = write!(out, "{text} ({url})");
                }
            }
            Inline::Break => out.push('\n'),
        }
    }
    out
}
