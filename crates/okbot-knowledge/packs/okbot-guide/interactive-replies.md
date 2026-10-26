---
type: Guide
title: Interactive replies
description: reply_mode = "interactive" lets the agent show cards, buttons, menus, forms and link buttons, natively on each chat platform and as text elsewhere.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-27
aliases: [interactive, reply_mode, buttons, nút bấm, menu, form, card, thẻ, components, Block Kit, cardsV2, rich message, tin nhắn tương tác, chart, biểu đồ, グラフ, charts, PNG]
generated: {by: okbot-guide-pack, at: "2026-10-27"}
---

# Interactive replies

By default a bot answers with chat text. With `reply_mode = "interactive"` the agent can also show,
after its answer:

- **cards**: a title, a colour (info, success, warning, error) and text, fields, a table or a chart
  (bar, column or line);
- **choices**: 2 to 25 options; up to 5 become buttons, more become a menu;
- **a form**: a button that opens up to 5 fields to fill in;
- **link buttons** to `https` pages.

The agent decides when they help (a decision, a list to pick from, data with structure); short
answers stay text. Typing still works: the reply can always be written instead of clicked.

```toml
[bot]
name = "Shop"
reply_mode = "interactive"     # text (default) or interactive

[channels.discord.main]
token_env = "DISCORD_TOKEN"
reply_mode = "interactive"     # per channel, over [bot]

[[channels.discord.main.rooms]]
room = "123456789012345678"
reply_mode = "text"            # per room, over the channel
```

It needs an agent that takes MCP servers per run (Claude Code or Codex); with others the answers
stay text and `okbot doctor` warns. Turns of automations and tasks from other bots never show views.

## What each platform shows

| Platform | Cards | Buttons and menu | Forms |
| --- | --- | --- | --- |
| Discord | coloured containers (Components V2) | yes | modal |
| Slack | header and section blocks | yes | modal |
| Google Chat | cards | yes | dialog (HTTP delivery only) |
| Telegram | as text | inline keyboard, one row per menu option | as a list of fields to type |
| Zalo, console, bridge | as text | a numbered list | as a list of fields to type |

## Charts as pictures

Charts are text bars (`Tea ▇▇▇▇ 3 kg`) unless you turn on pictures:

```toml
[charts]
images = true                  # default false
fonts = ["fonts/NotoSansSC-Regular.otf"]   # optional: Chinese, Korean…
```

- Three kinds: horizontal **bars** (comparing, ranking; up to 15), **columns** (months, days; up
  to 12, 1–3 series) and **lines** (trends; up to 60 points, 1–3 series). One label can be
  highlighted.
- Vietnamese, English and Japanese are built in (the Japanese font holds the 2,965 common kanji
  of JIS level 1). Numbers follow the language: `1.234.500` and `1,2 tr` in Vietnamese,
  `1,234,500` and `1.2M` in English, `123万` in Japanese. Other scripts need a font in `fonts`;
  a chart with characters no font has stays text, never empty boxes.
- **Discord** shows the picture inside the card (the bot needs *Attach Files*). **Telegram**
  sends it as a photo with the text and buttons. **Slack** posts it as a file just before the
  card (add the `files:write` scope and reinstall the app). The **console** saves it as a PNG file
  and prints the path. Google Chat and Zalo keep text.
- The picture has a description (alt text), and the chat history keeps the chart as text.
- It only applies to interactive rooms.

## When someone clicks

- The pick (or the form's values) becomes that person's next message, starting with `→`, and the
  agent answers it. The message with the buttons shows who chose what, and its buttons disappear.
- The first click wins. People the bot would not answer in that room are ignored.
- Views stay answerable for 7 days, also after a restart.
- A click counts like a message for `[limits]` and the rate limit.

## Safety

- What people type into a form reaches the agent and the chat history. The agent is told never to
  ask for passwords, codes, tokens or card numbers, and okbot refuses form fields named like
  them. To collect a secret, use a prompt of your own MCP server ([Your own tools](own-tools.md)):
  its values never reach the agent.
- Buttons from the agent are not approvals. Tools that change real data still need
  `confirm` on their `[[mcp]]` server ([Permissions](permissions.md)).
- Link buttons only open `https` addresses, and messages never mention `@everyone`.
