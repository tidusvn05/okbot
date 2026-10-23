---
type: Guide
title: Slack
description: Creating a Slack app in Socket Mode with the scopes and events okbot needs, and how okbot behaves on Slack.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [slack, slack app, tạo app slack, kết nối slack, socket mode, xoxb, xapp]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Slack

okbot uses **Socket Mode**, so no public URL is needed.

## 1. Create the app

1. At api.slack.com/apps create an app and enable **Socket Mode**. Create an **app-level token**
   with `connections:write` (`xapp-…`).
2. Bot token scopes: `app_mentions:read`, `chat:write`, `im:history`, `im:read`.
3. Event subscriptions (bot events): `app_mention`, `message.im`. In **App Home**, allow users to
   message the app (Messages tab).
4. Turn on **Interactivity & Shortcuts** (no request URL needed with Socket Mode), for buttons.
5. Install the app to the workspace and copy the **bot token** (`xoxb-…`).
6. `okbot new acme --platform slack`, or `okbot channel add slack`, and paste both tokens.

```toml
[channels.slack.acme]
app_token_env = "SLACK_APP_TOKEN"
bot_token_env = "SLACK_BOT_TOKEN"
reply_on = ["dm", "mention"]
```

## 2. What to expect

- Mentions in channels are answered **in a thread**; each thread is a conversation. Direct
  messages are one conversation per person.
- No typing indicator: okbot posts "_Thinking…_" and edits it into the answer.
- Approvals and prompts are Block Kit buttons (no forms). A press by someone else gets a private
  "not yours" notice.
- Slack claims `/` for its own commands: write `@bot /help`.
- mrkdwn formatting, under 3000 characters per message.
- The bot cannot open a direct chat first: people write to it once, then reminders and
  notifications reach them. It posts only in channels it was invited to.

To read Slack messages as **data** (search channels, summarise threads) rather than chat in Slack,
use a Slack MCP server: see [Popular services](popular-services.md).
