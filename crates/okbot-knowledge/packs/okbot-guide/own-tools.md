---
type: Guide
title: Your own tools and commands
description: Connecting your own MCP server with a shared secret or a signed run identity, asking people with buttons and forms, and turning a tool into a chat command with [[command]].
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [mcp server riêng, tool riêng, công cụ riêng, lệnh chat, slash command, "[[command]]", signed, nút bấm, form, api nội bộ]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Your own tools and commands

## Your own MCP server

Any MCP server over streamable HTTP or stdio can give the agent your tools (orders, stock, logs,
deploys). Write it in any language with an MCP SDK (`rmcp` for Rust).

```toml
[[mcp]]
name = "ops"                          # tools appear to the agent as mcp__ops__<tool>
url = "http://127.0.0.1:8790/mcp"
token_env = "OPS_MCP_TOKEN"           # shared secret in .env
auth = "signed"                       # bearer (default): the secret itself; signed: who asked and where
```

- Only Claude and Codex take per-run MCP servers. `tool_policy = "mcp-only"` suits a bot whose job
  is your tools.
- Add `roles`, `rooms`, `tools`, `confirm` as in [Permissions](permissions.md).
- Annotate your tools (`readOnlyHint`, `destructiveHint`) so approvals ask only for changes.

## Signed identity (`auth = "signed"`)

The server receives `Authorization: Bearer okbot1.<claims>.<signature>`: an HMAC-SHA256 of the
claims with your secret. The claims name the `user` (`discord:main/1234`), whether they are an
`admin`, the conversation, an expiry and an `interact` URL. Verify the signature and expiry on every
request, then authorize by `user` and `admin`; the agent cannot change them.

With the signature, a server can keep **per-person OAuth tokens** for services that have no MCP
server of their own: see `docs/extending/accounts.md` and `examples/mcp/accounts-server.mjs` in the
okbot repository.

## Ask the person, not the agent

A signed server can POST a prompt to the `interact` URL: text, buttons and (on Discord) a form.
okbot shows it in the chat and posts the answer back to the server, signed. Form values (one-time
codes, API keys) never reach the agent or the history. Discord shows buttons and forms; Telegram,
Slack and Google Chat show buttons; other platforms show text with `/answer <id> <n>`.

## Chat commands without the agent: `[[command]]`

```toml
[[command]]
name = "logs"                              # /logs
description = "Recent errors of a service"
mcp = "ops"
tool = "company_logs"
args = ["company", "service", "since?"]    # positional; `x?` optional, `x...` the rest of the line
with = { level = "error" }                 # always passed; chat cannot change it
roles = ["ops"]
```

`/logs acme api 24h` calls the tool at once, without the agent: it answers even when the agent's
quota is used up, costs nothing and answers the same way every time. Other arguments can be set as
`name=value`. Commands obey the server's rules, are in `okbot audit`, and are not confirmed (the
person typed them). okbot fills the command menus of Discord and Telegram with them. On Slack and
Google Chat, mention the bot first: `@bot /logs …`.

Test without a platform: `okbot chat --strict`, then `/logs …`.
