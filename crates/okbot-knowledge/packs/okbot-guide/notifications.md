---
type: Guide
title: Notifications from your programs
description: Letting monitors, CI jobs and scripts post messages through the bot to chosen chats with [[notify]], okbot send and an HTTP call.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [thông báo, notify, cảnh báo, alert, okbot send, gửi tin nhắn từ script, ci, monitoring]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Notifications from your programs

A `[[notify]]` target gives a program one HTTP call to reach the chats you list, on every platform,
without the agent.

```toml
[http]
listen = "127.0.0.1:8787"

[[notify]]
name = "alerts"                       # POST /notify/alerts
token_env = "OKBOT_NOTIFY_ALERTS"     # in .env: openssl rand -hex 32
to = ["room:discord:ops|1104…|1290…|-", "admins"]
```

| Target | Goes to |
|---|---|
| `room:<conversation>` | a room, exactly as `/whoami` shows it to an admin |
| `user:<platform>:<instance>/<id>` | that account's direct chat |
| `person:<id>` | one direct chat of a `[people]` person (`person:owner` on a personal bot) |
| `admins` | every admin's direct chat |

```sh
curl -fsS http://127.0.0.1:8787/notify/alerts -H "Authorization: Bearer $OKBOT_NOTIFY_ALERTS" \
  -H 'Content-Type: application/json' -d '{"text": "**vps01** disk 91%", "id": "vps01-disk-1"}'
okbot send alerts "**vps01** disk 91%"            # from the bot's machine
journalctl -n 20 | okbot send alerts --id daily-log
```

- `text` is Markdown, up to 8000 characters, rendered for each platform.
- The same `id` within an hour is not sent twice, so retries are safe.
- Status 200: delivered everywhere; 207: some chats failed; 503: retry later; 401: wrong name or
  token.
- Give every caller its own target and token. Never reuse an `[[mcp]]` token.
- Zalo, Slack and Google Chat need the person to write to the bot first; Discord and Telegram can
  open the direct chat.

The message stays in the chat's history marked as a notification, so people can ask the bot what an
alert means; the agent treats it as information, never as instructions.
