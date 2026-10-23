---
type: Guide
title: Zalo
description: Creating a Zalo Bot, connecting it to okbot with polling or a webhook, what works in Zalo, and fixing common Zalo problems.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [zalo, zalo bot, tạo bot zalo, kết nối zalo, zalo webhook, zalo token, Zalo Bot Creator]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Zalo

okbot uses the Zalo Bot Platform (bot.zapps.me).

## 1. Create the bot

1. In the Zalo app, open the official account **Zalo Bot Manager** and choose **Tạo bot**.
2. Name the bot; the name must start with `Bot` (`Bot MyShop`).
3. Zalo sends the **bot token** (`12345689:abc-xyz`) to your Zalo account. It does not expire until
   you reset it. Treat it like a password.

## 2. Connect it

```sh
okbot new shop --platform zalo        # new bot; paste the token when asked
okbot channel add zalo --bot shop     # or add Zalo to an existing bot
```

```toml
[channels.zalo.shop]
token_env = "ZALO_BOT_TOKEN"     # the token is in .env
delivery = "polling"             # or "webhook"
# public_url = "https://bot.example.com/zalo/shop"
# secret_env = "ZALO_WEBHOOK_SECRET"
```

| | `polling` | `webhook` |
|---|---|---|
| Public URL | not needed | HTTPS URL to okbot's `/zalo/<instance>` |
| Zalo's advice | development and testing | production |

okbot registers or removes the webhook itself when it starts. For webhooks, set `[http] listen`
and route the path through a reverse proxy ([Running on a VPS](deploy.md)).
`okbot doctor shop --test-webhook` asks Zalo to call the URL.

## 3. What to expect

- **Groups** (beta at Zalo): add the bot by its invite link; it gets only messages that mention it
  or reply to it.
- **Formatting**: bold, italic, lists, quotes. No links, code blocks or tables: URLs are plain text
  and tables become lists. 2000 characters per message.
- **No editing and no buttons**: the bot shows "typing…" and sends the answer when ready. Tool
  approvals arrive as text with `/approve <code>` and `/deny <code>`.
- **Images** are answered from their caption; stickers and voice are acknowledged only.
- **Protected users** (children and others Zalo protects): Zalo withholds the content, and okbot
  answers with a fixed message.
- The bot cannot write first: reminders and notifications reach a person only after they wrote to
  the bot directly once.

## 4. Your responsibilities

Zalo's terms treat you as the provider of an AI system: you answer for what the bot says, and
incidents with user data or generated content must be reported to Zalo within 24 hours.

## 5. Problems

| Symptom | Fix |
|---|---|
| `okbot doctor` says the token is invalid | It was reset in Zalo Bot Creator; update `.env`. |
| Webhook gets nothing, `webhook.http.403` | A WAF or CDN blocks Zalo's `Java/…` user agent. |
| `webhook.err.tls` | The certificate chain is incomplete. Zalo does not follow redirects. |
| Late or missing replies with polling | Polling can miss events; use `webhook` in production. |
