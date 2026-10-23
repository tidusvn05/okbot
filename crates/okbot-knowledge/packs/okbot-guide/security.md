---
type: Guide
title: Security and privacy
description: What okbot protects by design (no shell, scoped memory, sealed tokens), prompt injection risks, and the settings that keep a bot safe.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-24
aliases: [bảo mật, an toàn, security, prompt injection, rò rỉ dữ liệu, secret, token, quyền riêng tư, privacy]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Security and privacy

## What okbot does by design

- **The agent is untrusted.** It never gets a shell or file access, runs in an empty directory per
  conversation, and has no secrets in its environment. CLIs that cannot be locked down are refused.
- **Chat text is data.** Messages, mail and web pages are passed as data, never as instructions to
  okbot. Rules come only from `okbot.toml`.
- **Scoped knowledge and memory.** Each turn sees only what its user and room may see; one user can
  never read another's memory.
- **Secrets stay out of chats and logs.** Tokens live in `.env` (mode 0600), OAuth tokens are
  sealed with `state/secrets.key`, logs redact secrets, and agent errors are never echoed into chats.
- **The gateway.** The agent reaches MCP servers only through okbot, with a token that ends with its
  turn. Every call is in `okbot audit`.

## Prompt injection

Anyone who can put text in front of the agent (a web page, an email, a document, a group message)
can try to steer it. Limit what a steered agent could do:

- Use `confirm = "destructive"` on servers that can change things or reach other people.
- Offer only the tools a bot needs (`tools = [...]`, `deny_tools`).
- Use `tool_policy = "mcp-only"` where sensitive data is read, so nothing leaves through a URL.
- Personal accounts are never used in groups; automations' agent turns have no web tools.
- Routines the agent creates start paused until a person resumes them.

## Settings to check

| Setting | Safe value |
|---|---|
| `[agent] allow_unrestricted_agents` | `false` (never `true` for a bot others use) |
| tokens in `okbot.toml` | none: only `*_env` names |
| `[http] listen` | `127.0.0.1:…` behind a reverse proxy |
| MCP servers | only reviewed ones ([Popular services](popular-services.md)); none that offer a shell or file access on the bot's machine; `https://` for other hosts |
| `[bot] retention_days` | as short as you need (default 90) |

## Privacy for chat users

The bot keeps recent messages for context and deletes them after the retention period. People can
ask what it remembers about them and ask it to forget. On Zalo, the operator must report incidents
with user data to Zalo within 24 hours.
