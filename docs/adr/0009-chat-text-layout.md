# 0009 — Chat text layout

Status: Accepted (2026-10-17)

## Context
Every platform okbot serves shows a line break as a line break, and the people, agents and MCP servers that
write replies write for that. CommonMark does not: a single newline inside a paragraph is a space, and a line
right after a list continues the last item. Replies such as a status line followed by a list and a summary line
reached Discord as one run-on line. Small print (who asked, versions, sources) had no portable form, and the
agent had no guidance on layout beyond the dialect.

## Decision
- **Chat line semantics in `okbot-render`.** Before parsing, an unindented line right after a list item starts a
  new paragraph; inside paragraphs every line break is kept (`Inline::Break`). Code blocks are untouched. This
  applies to everything rendered: agent answers, `[[command]]` results and prompts from MCP servers.
- **`-# ` note lines.** A line starting with `-# ` is a note (`Block::Note`): Discord shows it as its own small
  grey subtext, other dialects as emphasis, plain text as is.
- **A layout convention in the system prompt** (`<format>`): the first line answers or concludes, then short
  lines and flat lists, routine details summed up in one line, the next step last; the dialect line says what
  the platform shows (tables, `-# `).

## Consequences
- Text written with single newlines reads the same in every dialect; hard-wrapped paragraphs now keep their
  breaks (no writer of chat replies hard-wraps).
- Blocks are still joined by a blank line.
- MCP servers that write text for people (signed `command` runs, prompts) can use the same layout and notes.
