---
type: Guide
title: Console and other platforms
description: Trying a bot in the terminal with okbot chat, testing scenarios, and connecting platforms okbot does not ship (Matrix, WhatsApp, Messenger) through a bridge adapter.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [okbot chat, console, terminal, thử bot, bridge, matrix, whatsapp, messenger, nền tảng khác]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Console and other platforms

## The console: `okbot chat`

`okbot chat` in a bot's directory runs the bot in the terminal. It needs no platform account.

```sh
okbot chat                         # you, in a direct chat
okbot chat --like zalo             # Zalo's formatting, length limit and lack of buttons
okbot chat --strict                # admins, roles, rooms and [[mcp]] rules as `okbot run` applies them
okbot chat --agent echo            # no agent CLI: every message is echoed
```

Console commands start with `:`: `:as alice` (write as another user), `:room team` (a group room;
mention the bot with `@okbot`), `:dm`, `:attach <file>`, `:press 1` (press a button), `:help`.

Without `--strict`, every console user gets all servers and roles because you own the directory.
Use `--strict` to test permissions.

**Scenarios as tests.** A text file of lines with `:expect <text>` checks is a test for CI:
`okbot chat --fresh --strict --replies replies.toml < scenario.txt` exits 1 when a check fails.
`--replies` answers from rules in a TOML file instead of a real agent, so it costs nothing.

## Other platforms: bridge adapters

For Matrix, WhatsApp, Messenger, LINE or an in-house chat, write a **bridge adapter** in any
language. okbot starts it as a child process and talks JSON-RPC over stdin and stdout.

```toml
[channels.bridge.main]
platform = "matrix"                  # the channel id becomes matrix:main
command = "okbot-channel-matrix"     # or a path relative to the bot directory
env = ["MATRIX_TOKEN"]               # passed from .env
max_message_len = 4096
dialect = "common-mark"              # common-mark, plain, slack-mrkdwn, discord-md, g-chat-text, zalo-md, telegram-html
edit = true                          # the adapter can edit messages ("thinking…" then the answer)
reply_on = ["dm", "mention"]
```

Start from the reference adapter `examples/bridge/echo-channel.mjs` in the okbot repository (Node,
no dependencies) and replace the parts marked `PLATFORM`. The protocol is in `docs/bridge.md`.
Never put secrets in `[channels.bridge.<name>.settings]`: `okbot.toml` is not a secret file.

A native Rust adapter (a new crate implementing `Channel`) is the choice for platforms okbot
should ship with; see `docs/extending/channel.md`.
