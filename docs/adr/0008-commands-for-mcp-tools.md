# 0008 — Chat commands for MCP tools

Status: Accepted (2026-10-17)

## Context
A bot whose job is a set of `[[mcp]]` tools (operations, internal systems) reaches them only through the agent.
For the everyday requests (status, logs, restart) the agent adds nothing but latency, cost and variance, and it
is the part most likely to be down when it is needed most: quota, an expired login, a provider outage. Every
platform okbot supports can carry a text command; Discord and Telegram also show a command menu.

## Decision
- **`[[command]]` maps a command to one tool**: `name`, `description`, `mcp`, `tool`, positional `args`
  (`x`, `x?`, `x...`), fixed `with` arguments, `admins_only`. okbot runs it in the conversation worker, in order
  with the messages around it, without an agent turn.
- **A minimal MCP client in the runtime** (streamable HTTP: `initialize`, `tools/list`, `tools/call`, JSON or
  event-stream replies, session closed after). Arguments are typed from the tool's input schema, so the
  configuration does not repeat types; the listed schema also tells whether the person may use the tool.
- **The same credentials as an agent turn.** Signed servers get the run token with a new optional claim,
  `command` (its name), so a server can answer readable text to a person and JSON to an agent. Servers keep
  their own permission checks and prompts; okbot adds `admins_only`.
- **Commands are text.** A command is a message starting with `/name`. `Channel::set_commands` (default: no-op)
  lets an adapter list the commands in its platform's menu; an adapter whose menu sends something other than a
  message (Discord interactions) converts it to that text, so the runtime has one path. The runtime calls it
  only when the bot has `[[command]]`s, so existing bots keep their menus.
- Names are limited to `a-z0-9_` (≤ 32) — the intersection of Discord's and Telegram's rules — and cannot reuse
  a built-in command.

## Consequences
- Operations keep working without the agent, and each command costs one local HTTP round trip.
- Results are not in the conversation history (a resumed agent session would not see them either); a person who
  wants the agent's reading of a result asks in words.
- Slack and Google Chat need `@bot /name` until their adapters register native commands (an app-manifest change).
- Discord's global slash commands replace any the application had; a bot shared with other command sources should
  not use `[[command]]`.
