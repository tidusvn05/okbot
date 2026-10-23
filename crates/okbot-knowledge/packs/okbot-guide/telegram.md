---
type: Guide
title: Telegram
description: Creating a Telegram bot with BotFather, connecting it to okbot, groups and privacy mode, and fixing common Telegram problems.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [telegram, botfather, tạo bot telegram, kết nối telegram, telegram webhook, privacy mode]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Telegram

## 1. Create the bot

1. Open **@BotFather** in Telegram and send `/newbot`.
2. Pick a display name and a username ending in `bot`.
3. Copy the **bot token** (`123456789:AA…`). `/revoke` in BotFather replaces it.

## 2. Connect it

```sh
okbot channel add telegram --bot shop     # asks for the token, checks it, saves it to .env
```

```toml
[channels.telegram.shop]
token_env = "TELEGRAM_BOT_TOKEN"
delivery = "polling"                       # or "webhook"
# public_url = "https://bot.example.com/telegram/shop"
# secret_env = "TELEGRAM_WEBHOOK_SECRET"   # 1–256 of A-Z a-z 0-9 _ -
reply_on = ["dm", "mention"]
```

Polling needs no public URL and suits most bots. Webhooks need HTTPS on port 443, 80, 88 or 8443,
routed to okbot's `/telegram/<instance>`. Only one program may poll a token at a time.

## 3. What to expect

- **Groups**: with privacy mode on (the default), Telegram delivers only commands, replies to the
  bot and messages that mention `@YourBot`. For `reply_on = ["all"]`, turn privacy mode off in
  BotFather (`/setprivacy` → Disable) and add the bot to the group again.
- **Forum topics**: each topic is its own conversation.
- **Buttons** for approvals and prompts (no forms). "typing…" while the agent works.
- **Photos and files** up to 20 MB: the agent can look at photos; a personal bot's owner can save
  documents ("save this").
- **Direct messages first**: the bot can message someone only after they pressed **Start** once.
- `/tasks@YourBot` works like `/tasks`.

## 4. Problems

| Symptom | Fix |
|---|---|
| `401 Unauthorized` | The token was revoked in BotFather; update `.env`. |
| `409 Conflict` in the log | Another program polls the same token, or a webhook is set elsewhere. |
| No group messages | Privacy mode is on and the message does not mention the bot. |
| Webhook gets nothing | `okbot doctor` shows Telegram's last delivery error; check the port and certificate. |
