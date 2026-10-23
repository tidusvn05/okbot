---
type: Guide
title: Install okbot and create a bot
description: Installing okbot, logging in an agent CLI, creating a bot with the wizard, trying it in the terminal and running it as a service.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [cài đặt okbot, tạo bot, install okbot, okbot new, bắt đầu, quick start, cài okbot]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Install okbot and create a bot

Requirements: Linux or macOS, and at least one agent CLI installed and logged in. Claude Code
(`claude`) or Codex (`codex login`) are recommended because okbot can lock down their tools.

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/tidusvn05/okbot/main/install.sh | sh   # prebuilt, checksum verified
cargo install --locked okbot                                                         # or build (Rust 1.89+)
```

A container image is also published: `ghcr.io/tidusvn05/okbot`.

## 2. Create the bot

```sh
okbot new shop            # wizard: agent CLI, personal or team, platform and its token
cd shop
okbot chat                # talk to it in the terminal, no platform needed
okbot doctor              # checks the agent, knowledge, platform tokens and settings
```

The wizard writes:

```
shop/
├── okbot.toml   # settings (no secrets): safe to keep in git
├── .env         # tokens, mode 0600
├── kb/          # knowledge base: persona, your documents, memory, tasks
└── state/       # SQLite history, agent work directories
```

Tokens always go to `.env` and are referenced by name in `okbot.toml` (`token_env =
"ZALO_BOT_TOKEN"`). Never put a token in `okbot.toml` or on the command line.

## 3. Run it

```sh
okbot start shop          # systemd service, restarts on failure and at boot
okbot status shop
okbot logs shop -f
okbot restart shop        # after editing okbot.toml or kb/persona
```

Without systemd (containers), run `okbot run` under your own supervisor.

## 4. Next steps

- Add your documents to `kb/knowledge/` ([Knowledge base](knowledge-base.md)).
- Change the bot's personality in `kb/persona/assistant.md`.
- Add more platforms: `okbot channel add discord` ([Choosing a chat platform](platforms.md)).
- Decide who may use it ([Permissions](permissions.md)).

## Setting up with a coding agent

`okbot help-agent` prints a contract for coding agents (Claude Code, Codex): the non-interactive
flags, JSON output and exit codes. Code 3 means a value is missing and names the flag to ask for.
