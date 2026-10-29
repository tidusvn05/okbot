---
type: Guide
title: Personal or team bot
description: The difference between a personal assistant and a team bot, pairing your accounts, and linking one person's accounts on several platforms.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [trợ lý cá nhân, bot cá nhân, bot nhóm, personal assistant, okbot pair, ghép tài khoản, liên kết tài khoản]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Personal or team bot

| | `personal` | `team` (default) |
|---|---|---|
| Who it answers | only your paired accounts; others get "This is a private assistant" | everyone the access rules allow |
| Memory | one shared memory for you on every platform (`kb/memory/people/owner/`) | one memory per account, or per linked person |
| Typical use | your own assistant: mail, calendar, reminders, notes | a shop, a support desk, a team helper |

Choose it in the wizard, with `okbot new me --profile personal`, or later with `okbot profile
personal --bot me`.

## Pair your accounts (personal)

```sh
okbot pair me             # prints `/pair 483921`, valid 10 minutes, single use
okbot pair me --list      # your paired accounts
okbot pair me --remove <account>
```

Send the printed `/pair …` line to the bot in a direct message, once per platform. `okbot chat` in
the bot's directory is always you.

## In groups

In a group, a personal bot answers only you, when you mention it or reply to it. Everyone in a
group reads the answers, so personal memory, tasks and notes are not used there. Mark a group where
that is fine (a family group) as `personal`:

```toml
[[channels.zalo.me.rooms]]
room = "zgr-…"        # /whoami in the group shows the id
personal = true
```

## One person, several platforms (team)

A team bot keeps one memory per account. When someone uses Zalo and Discord, the operator can link
the accounts so they share one profile, memory, limits and connected services:

```sh
okbot people link lan zalo:shop/<id> discord:shop/<id>   # ids from /whoami
okbot people list
```

The same in `okbot.toml`:

```toml
[people.lan]
accounts = ["zalo:shop/123", "discord:shop/456"]
```

A running bot applies links within seconds, without a restart. Scripts and MCP servers can use
`okbot people list --json` (and `--json` on `link` and `unlink`).

Only link accounts the person confirms are theirs: linked accounts see each other's notes. In
roles, notifications and automations, write the person as `person:lan`.
