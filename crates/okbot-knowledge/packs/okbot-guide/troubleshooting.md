---
type: Guide
title: Troubleshooting
description: Finding out why a bot does not answer, connect or deliver, with okbot doctor, logs and fixes for the most common problems.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [lỗi, sửa lỗi, không trả lời, bot không hoạt động, debug, troubleshooting, okbot doctor, logs, invalid_grant]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Troubleshooting

Start with:

```sh
okbot doctor shop            # every check, with what to fix
okbot logs shop -f           # the service log
okbot history runs --errors  # failed agent turns
okbot audit --since 1h       # tool calls that were refused
```

## The bot does not answer

| Cause | Check |
|---|---|
| The service is not running | `okbot status shop` |
| Wrong or reset token | `okbot doctor`; update `.env`, then `okbot restart` |
| In a group without a mention | mention the bot or reply to it, or set `reply_on = ["all"]` (Telegram: privacy mode off; Discord: Message Content intent) |
| Personal bot, account not paired | `okbot pair <bot>` and send `/pair <code>` |
| `allow_users` does not list the person | `/whoami` shows the id to add |
| Over a daily limit | `[limits]`; admins are exempt |
| Agent CLI logged out or out of quota | `okbot agent status`; add a fallback with `okbot agent chain` |

## Answers are wrong or ignore the knowledge

- `okbot kb lint` must show no errors; a document needs `title`, `description` and `tags`.
- One topic per file, with a description that says what is in it.
- The persona (`kb/persona/assistant.md`) changes only after `okbot restart`.

## Tools and services

| Symptom | Fix |
|---|---|
| The agent says a service is not connected | the person sends `/connect <name>` in a direct chat |
| Works in a direct chat, not in a group | personal accounts are never used in groups; mark the room `personal` or use `credentials = "shared"` |
| Every call asks for **Allow** | the server has no tool annotations: narrow `tools = [...]` and set `confirm` accordingly |
| A tool is "not available to this person here" | `roles`, `rooms`, `tools` or `[[mcp.restrict]]` exclude it |
| `/connect` link fails after login | `[http] listen` (and `public_url` for other machines) must reach `/oauth/callback` |
| Google asks to reconnect every week | the consent screen is in Testing: publish it ([Google Workspace](google-workspace.md)) |

## Reminders and notifications do not arrive

- The person must have written to the bot in a direct chat on that platform (Zalo, Slack, Google
  Chat always; Telegram after **Start**).
- `[bot] timezone` must be set for times to be right.
- `okbot doctor` lists people the bot cannot reach yet and reminders that were missed.

## Webhooks

- Zalo: `okbot doctor --test-webhook`; `403` means a WAF blocks Zalo, `err.tls` an incomplete
  certificate chain; Zalo does not follow redirects.
- Telegram: ports 443, 80, 88 or 8443 only; `409 Conflict` means another program polls the token.
- Google Chat HTTP: exactly one of `audience_url` or `audience_project_number`.

Platform pages have more: [Zalo](zalo.md), [Telegram](telegram.md), [Discord](discord.md),
[Slack](slack.md), [Google Chat](google-chat.md).
