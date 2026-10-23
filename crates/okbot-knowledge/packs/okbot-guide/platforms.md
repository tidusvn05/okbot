---
type: Guide
title: Choosing a chat platform
description: A comparison of the chat platforms okbot supports (Zalo, Telegram, Discord, Slack, Google Chat, console, bridge) and how to add or remove one.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [nền tảng chat, chọn nền tảng, kênh chat, platform, channel, thêm kênh, okbot channel add]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Choosing a chat platform

One bot can serve several platforms at once, with one knowledge base and one set of rules.

| Platform | Public URL needed | Buttons (approvals) | Message limit | Bot can start a direct chat | Guide |
|---|---|---|---|---|---|
| Zalo Bot | no (polling); yes for webhook | no: text `/approve <code>` | 2000 | no, the person writes first | [Zalo](zalo.md) |
| Telegram | no (polling); yes for webhook | yes | 4096 | yes, after the person pressed **Start** | [Telegram](telegram.md) |
| Discord | no | yes, with forms | 2000 | yes (unless they block DMs) | [Discord](discord.md) |
| Slack | no (Socket Mode) | yes | 3000 | no, the person writes first | [Slack](slack.md) |
| Google Chat | no (Pub/Sub); yes for HTTP | yes | 4096 | no, the person messages the app first | [Google Chat](google-chat.md) |
| Console (`okbot chat`) | no | yes, with forms | — | — | [Console and other platforms](other-platforms.md) |
| Anything else | depends | depends | configurable | no | [Console and other platforms](other-platforms.md) |

Quick picks:

- **Vietnamese customers or family**: Zalo.
- **Easiest to set up, works everywhere**: Telegram or Discord.
- **A company on Slack or Google Workspace**: Slack or Google Chat.
- **Trying things out**: `okbot chat` in the terminal, no account needed.

## Add, list and remove platforms

```sh
okbot channel add telegram --bot shop     # asks for the token, checks it, saves it to .env
okbot channel list --bot shop             # channels and whether their secrets are set
okbot channel remove slack:shop --bot shop
okbot restart shop
```

## Who the bot answers on every platform

Each channel section takes the same access settings:

```toml
[channels.discord.main]
token_env = "DISCORD_TOKEN"
reply_on = ["dm", "mention"]   # dm, mention, all (every group message)
allow_users = []               # only these user ids; empty = everyone
blocked_users = []             # never answered
```

`/whoami` in a chat shows the ids. More in [Permissions](permissions.md).

## What every platform does the same

- In direct chats every message is answered; in groups, when mentioned or replied to.
- Messages sent while the bot is answering are answered together right after.
- Long answers are split; tables become lists where the platform has no tables.
- `/help`, `/reset`, `/stop`, `/whoami` work everywhere. On Slack and Google Chat, where the app
  claims `/`, mention the bot first: `@bot /help`.
