# 0011 — An MCP gateway, roles and per-person limits

Status: Accepted (2026-10-21)

## Context
`[[mcp]]` handed the agent a server's URL and credential: every tool of the server was allowed, the only
rule was `admins_only` for the whole server, and okbot saw none of the calls. Operators asked for what
MCP gateways and team chat bots offer: tool-level allow-lists bound to groups of people, rooms where a
server is offered, a person's confirmation before a destructive call, an audit trail, and a cap on what
one person can spend. Other assistants show how such rules fail: enforced in several places, one path
skips them (a per-chat policy bypassed through a loopback MCP route, an `/allowlist` command without an
admin check).

## Decision
- **One gateway.** okbot serves every `[[mcp]]` server to the agent through a gateway on a random loopback
  port (`/mcp/<name>`). The agent gets a random token bound to its turn; the gateway forwards to the server
  with the configured secret or a signed run token (ADR 0007), so the agent never holds either. Every
  agent call to an extra server passes through `permission::Access`, the single function that decides
  whether a person may use a server and a tool; `[[command]]` runs use it too.
- **Roles.** `[roles] ops = ["discord:main/1234", "person:lan"]` names groups of accounts (or `[people]`).
  `admins` (the `[bot] admins` and the owner of a personal bot) and `everyone` are built in. The store
  also holds temporary grants (`okbot role grant ops <account> --for 2h`, or `/grant` by an admin): never
  for `admins`, always with an expiry.
- **Server and tool rules** in `[[mcp]]`: `roles` (who), `rooms` (where), `tools` and `deny_tools`
  (patterns with `*`; `deny_tools` wins), and `[[mcp.restrict]] tools = […] roles = […]` for tools only some
  roles may call. `tools/list` replies are filtered, so the agent does not see what it may not call, and
  `tools/call` of anything else is refused with a tool error. `admins_only = true` stays as
  `roles = ["admins"]`.
- **Confirmation.** `confirm = "destructive"` asks the requester (or `confirm_by = "admins"`) with Allow and
  Deny buttons before a tool whose annotations do not say `readOnlyHint` and say (or default to)
  `destructiveHint`; `"always"` asks for every call. Platforms without buttons show the same prompt as text
  with `/approve <code>` and `/deny <code>`, which the dispatcher handles before the conversation queue (the
  turn that waits holds that queue). A confirmation not answered within the turn's time is a refusal.
  `[[command]]` runs are typed by the person and are not confirmed.
- **Audit.** Each `tools/call` through the gateway or a command is stored in SQLite (`tool_calls`: run,
  person, conversation, server, tool, decision, error, duration; never arguments or results, which may
  hold secrets) and listed by `okbot audit`.
- **Limits.** `[limits] turns_per_day` and `tokens_per_day` cap each person over a rolling 24 hours, counted
  from `agent_runs` (which now records the person). Admins are exempt. Zero means no limit.
- **stdio servers.** `[[mcp]] command = [...]` runs a local server per agent session behind the same gateway,
  with an empty environment (plus the variables it names), its own empty directory, and no inherited
  secrets; it is killed when the session or the turn ends.

## Consequences
- The rules hold for every MCP-capable agent, since enforcement does not depend on CLI flags.
- The gateway buffers each reply to find the JSON-RPC response; progress notifications arrive with the result,
  and server-to-client requests (sampling, elicitation) are not relayed. Servers ask people through
  `/interact` instead. `GET` streams answer 405, which the protocol allows.
- JSON-RPC batches are refused (the 2025-06-18 protocol removed them).
- Tool annotations come from the server the operator configured, so they are trusted as hints for
  confirmation, not as permissions: permissions come only from `okbot.toml` and grants.
- Schema version 4 adds `tool_calls`, `role_grants` and `agent_runs.person`.
