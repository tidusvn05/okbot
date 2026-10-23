---
type: Guide
title: Knowledge base
description: Adding your documents to a bot's knowledge base, the persona, private documents, checking the bundle and how memory is kept.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [knowledge, kiến thức, tài liệu, thêm tài liệu, okbase, persona, tính cách bot, kb lint, kb add, tri thức]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Knowledge base

Each bot has one **okbase** bundle in `kb/`: markdown files with a short header (front matter). The
bot re-reads changed files within a few seconds.

```
kb/
├── persona/assistant.md   # who the bot is and how it talks (sent with every question)
├── system/                # how to use the bot, privacy
├── knowledge/             # your documents: one topic per file
├── memory/                # what the bot remembers about people, rooms, the team
├── tasks/                 # to-dos, reminders, routines
└── notes/                 # quick notes and journal
```

## Add documents

Write one topic per file in `kb/knowledge/`, with a header:

```markdown
---
type: Policy
title: Refunds
description: Customers may return products within 7 days with the receipt.
tags: [bot]
lang: en
status: stable
updated: 2026-10-23
---

Returns are accepted within 7 days with the receipt.
```

Or let okbot convert files:

```sh
okbot kb add price-list.pdf             # PDF, Word, PowerPoint, HTML, markdown, text, image
okbot kb add https://example.com/faq    # a web page
okbot kb inbox                          # what was saved, to move into folders later
okbot kb lint                           # check against the okbase standard (L2)
okbot kb upgrade                        # add default files that are missing (never overwrites)
```

`okbase adopt` converts whole folders of plain markdown, and `okbase view` shows the bundle in a
browser. Tags must be listed in `kb/_meta/vocabulary.md`.

## Who sees what

- Everything in `knowledge/` is visible to everyone the bot answers.
- A document with `audience: private` in its header is hidden in rooms with web tools, unless the
  room is marked `internal = true` ([Permissions](permissions.md)).
- Memory is scoped: a person's memory only in their own chats, a room's memory only in that room,
  team memory for everyone. One user can never read another user's memory.

## Persona

Edit `kb/persona/assistant.md` to change the bot's name, tone and limits ("answer in the user's
language", "never quote prices without the price list"). Restart the bot after changing it.

## Knowledge packs

okbot ships ready-made folders of knowledge:

```sh
okbot kb pack list
okbot kb pack add okbot-guide           # this guide, in kb/knowledge/okbot-guide/
```

Running `pack add` again after upgrading okbot replaces the pack's files with the new version. Do
not edit files inside a pack's folder; put your own notes elsewhere.

## Versioning

When `kb/` is a git repository, every change the bot makes (memory, tasks, notes) is a commit, so it
can be reviewed and reverted.
