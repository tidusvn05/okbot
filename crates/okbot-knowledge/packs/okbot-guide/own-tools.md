---
type: Guide
title: Your own tools and commands
description: Connecting your own MCP server with a shared secret or a signed run identity, asking people with buttons and forms, and turning a tool into a chat command with [[command]].
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-30
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

**Tasks for the person who asked.** A server that creates reminders through the okbot CLI can pass
the token it received: `OKBOT_RUN_TOKEN=<token> okbot task add "Renew cert" --remind "…" --json`
(or `--run-token`). okbot checks the signature, expiry and bot, and the task belongs to the person
linked to the token's `user` in `[people]` (the owner on a personal bot), so no `--person` is
needed. `--person` still wins when given. This picks the owner; it is not a permission check
(whoever can run `okbot task` can pass `--person`), so keep authorizing in your server. Prefer the
variable to the flag: arguments show up in `ps`.

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

Names use `a-z`, `0-9` and `_` (at most 32) and cannot reuse a built-in command. The reserved
names are `help`, `reset`, `new`, `clear`, `whoami`, `stop`, `cancel`, `remember`, `forget`,
`memories`, `memory`, `pair`, `tasks`, `todo`, `todos`, `done`, `snooze`, `later`, `resume`,
`start`, `pause`, `note`, `approve`, `deny`, `answer`, `grant`, `revoke`, `roles`, `connect`,
`disconnect` and `connections`. `okbot commands --builtin` prints them (`--json` for scripts), and
`okbot doctor` names every `[[command]]` that uses one in a single report.

Test without a platform: `okbot chat --strict`, then `/logs …`.

### Cards and buttons in a command's answer

A tool can answer a command with a view instead of text: cards with fields, a table or a chart,
link buttons, and buttons that run another `[[command]]`. Put it in the result's
`structuredContent` under `okbot_view` (or make the whole text `{"okbot_view": {…}}`):

```json
{"okbot_view": {
  "text": "Services of **freach**",
  "cards": [{"title": "channel-broker", "tone": "success",
             "fields": [{"label": "Pods", "value": "3/3"}],
             "table": {"columns": ["Pod", "State"], "rows": [["cb-1", "running"]]}}],
  "buttons": [{"label": "Refresh", "command": "/status freach", "style": "primary"},
              {"label": "Restart", "command": "/restart freach", "style": "danger"}],
  "links": [{"label": "Grafana", "url": "https://grafana.example.com/d/1"}]
}}
```

- Cards, tables and charts are the same as in [Interactive replies](interactive-replies.md), with
  no choices or forms. Buttons and links together are at most 5.
- The view is shown on every platform, whatever `reply_mode` says: natively on Discord, Slack,
  Google Chat and Telegram, as text elsewhere (a button becomes the command to type).
- A button runs only a `[[command]]` of this bot (never a built-in or a message to the agent), as
  the person who presses it, with that person's `roles` and the server's rules. When the server's
  `confirm` wants an approval for that tool, the press first asks **Run?**. Only the presser can
  answer it (or the admins, with `confirm_by = "admins"`). Typed commands are never asked.
- The first press closes the view; the new answer comes with fresh buttons.
- A view okbot cannot show (an unknown command, a field it does not know) is logged, and the chat
  gets the text of the result instead. A signed server can tell a command from an agent by the
  `command` claim of its token, and send the view only to commands.
