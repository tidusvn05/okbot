---
type: Guide
title: What okbot is
description: The parts of an okbot bot (chat platforms, agent CLI, knowledge base, tools) and where to start for each goal.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-24
aliases: [okbot là gì, giới thiệu okbot, okbot overview, bắt đầu với okbot]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# What okbot is

okbot is an open-source chat assistant (MIT or Apache-2.0, written in Rust). One program connects
chat platforms to an **agent CLI** (Claude Code, Codex, OpenCode or Devin) and to a **knowledge
base** of markdown files. People chat with the bot on Zalo, Telegram, Discord, Slack or Google
Chat. The agent answers from the knowledge base and the web, and calls the tools you allow.

```
chat platforms ─ okbot runtime (access rules, history, queue) ─ agent CLI
                         │                                        │
                 knowledge base (kb/)  ◄──── MCP tools ───────────┘
                 your MCP servers through okbot's gateway (Gmail, Notion, your APIs)
```

- **No API key.** okbot runs the agent CLI that is already logged in on the machine and uses its
  subscription or quota.
- **The agent never gets a shell or the files of the bot's machine.** It can search the web and
  use the MCP tools you configure, nothing else.
- **One bot = one directory**: `okbot.toml` (settings, no secrets), `.env` (tokens), `kb/`
  (knowledge and memory) and `state/` (database). Many bots can run on one machine.

## Where to start

| You want to | Read |
|---|---|
| Install okbot and make a first bot | [Install okbot and create a bot](first-bot.md) |
| A private assistant only for yourself | [Personal or team bot](personal-or-team.md) |
| Pick or set up a chat platform | [Choosing a chat platform](platforms.md) |
| Choose the agent CLI and model | [Agent CLIs](agents.md) |
| Control who may use the bot and its tools | [Permissions](permissions.md) |
| Connect Gmail, Drive, Notion, GitHub… | [Connecting services through MCP](services-mcp.md) |
| Run the bot on a server | [Running on a VPS](deploy.md) |
| Fix something that does not work | [Troubleshooting](troubleshooting.md) |

## Words used in this guide

- **Bot**: one okbot directory with its settings, knowledge and history.
- **Channel**: one connection to a chat platform, written `platform:instance` (`zalo:shop`).
- **Account**: one person on one channel, written `platform:instance/user-id`. `/whoami` shows it.
- **Room**: a group chat, channel or space.
- **MCP server**: a program that offers tools to the agent (Model Context Protocol).
- **Gateway**: the part of okbot that sits between the agent and MCP servers and enforces the rules.
