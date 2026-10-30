---
type: Guide
title: Permissions
description: Who the bot answers, administrators, roles and temporary grants, per-room settings, which MCP tools each person may call, approvals, audit and daily limits.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [phân quyền, quyền, admin, role, vai trò, cấp quyền, giới hạn, permission, okbot role grant, audit, allow_users, duyệt, approve]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Permissions

Permissions in okbot have four layers. Each is written in `okbot.toml` and applied by okbot, never
by the agent, so a message cannot talk the bot out of them.

## 1. Who the bot answers

Per channel:

```toml
[channels.telegram.shop]
token_env = "TELEGRAM_BOT_TOKEN"
reply_on = ["dm", "mention"]     # dm, mention, all
allow_users = ["12345678"]       # only these user ids; empty = everyone
blocked_users = []
```

A `personal` bot answers only the owner's paired accounts ([Personal or team bot](personal-or-team.md)).

## 2. Administrators and roles

```toml
[bot]
admins = ["telegram:shop/12345678"]          # accounts as /whoami shows them

[roles]
sales = ["zalo:shop/111", "person:lan"]      # accounts or [people] ids
support = []                                 # filled by temporary grants only
```

- `admins` and `everyone` are built-in roles. On a personal bot the owner is an admin.
- **No restart for access changes.** A running bot re-reads `[bot] admins`, `allow_users`,
  `blocked_users`, `[people]` and `[roles]` about four seconds after a change to `okbot.toml`
  (`okbot people link` included); `okbot reload <bot>` applies them at once. Conversations keep
  going; a turn already running finishes with the old lists. A file with a mistake changes nothing
  (the log says why). Other settings still need `okbot restart`.
- After upgrading okbot, `okbot restart <bot>` once: the bot keeps running the old binary until
  then, and `okbot reload` refuses a bot that was started by an older okbot or is still starting
  (it says to restart instead).
- Temporary grants: `okbot role grant sales zalo:shop/222 --for 2h`, or `/grant sales <account> 2h`
  by an admin in chat. `okbot role revoke`, `/revoke`, `okbot role list`, `/roles`. A grant lasts at
  most 30 days; `admins` and `everyone` cannot be granted.

## 3. Rooms

```toml
[[channels.discord.main.rooms]]
room = "987654"            # channel id from /whoami
tool_policy = "mcp-only"   # no web search or fetch in this room
internal = true            # may see knowledge marked `audience: private`
personal = false           # true: answers may use the asker's personal memory, tasks and services
```

Use `mcp-only` in rooms with sensitive knowledge: without web tools, the agent cannot send data out
through a URL.

## 4. Tools of MCP servers

Every `[[mcp]]` server says who may use it, where, and which tools:

```toml
[[mcp]]
name = "ops"
url = "http://127.0.0.1:8790/mcp"
token_env = "OPS_MCP_TOKEN"
roles = ["ops", "support"]          # who (default: everyone the bot answers); admins_only = true is short for ["admins"]
rooms = ["discord:main/987654"]     # where (default: everywhere)
tools = ["status", "logs_*"]        # offered tools, * matches anything (default: all)
deny_tools = ["logs_purge"]         # never offered; wins over tools
confirm = "destructive"             # never (default), destructive, always
confirm_by = "requester"            # requester (default) or admins

[[mcp.restrict]]                    # these tools only for these roles
tools = ["deploy*"]
roles = ["ops"]
```

- A tool a person may not call is hidden from the agent, and a call to it is refused.
- **Approvals.** With `confirm = "destructive"`, every call that the server does not mark as
  read-only waits: the chat shows the tool and its arguments with **Allow** and **Deny** (or
  `/approve <code>` and `/deny <code>` on Zalo). No answer in time counts as a refusal.
  `always` asks for every call.
- **Personal accounts** connected with `/connect` are used only in that person's direct chats and
  `personal` rooms, never in groups ([Connecting services through MCP](services-mcp.md)).

## Audit and limits

```sh
okbot audit --since 24h             # every tool call and command: who, where, allowed or refused
okbot audit --person lan --json
```

Arguments and results are not stored (they may hold secrets).

```toml
[limits]                  # rolling 24 hours, admins exempt
turns_per_day = 50        # 0 (default) = no limit
tokens_per_day = 2000000
```

A person over the limit gets a fixed reply. Linked accounts share one limit.

## Testing permissions

`okbot chat --strict` applies the real rules in the terminal; `:as alice` writes as another user.
