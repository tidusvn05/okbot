---
type: Guide
title: Discord
description: Creating a Discord bot application, inviting it to a server with the right permissions, and how okbot behaves on Discord.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-27
aliases: [discord, discord bot, tạo bot discord, kết nối discord, developer portal, message content intent]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Discord

## 1. Create the bot

1. In the Discord Developer Portal (discord.com/developers/applications), create an application,
   add a **Bot**, and copy its token (**Reset Token**).
2. `okbot new main --platform discord`, or `okbot channel add discord --bot main`, and paste the
   token (it goes to `.env`).
3. `channel add` and `okbot doctor` print an invite link with the permissions okbot needs: *View
   Channels*, *Send Messages*, *Send Messages in Threads*, *Read Message History*, *Attach Files*
   (for chart images). Open it and pick
   your server. To DM the bot you must share a server with it.

```toml
[channels.discord.main]
token_env = "DISCORD_TOKEN"
message_content = false     # true (and enable the intent in the portal) to read unmentioned messages
reply_on = ["dm", "mention"]
```

## 2. What to expect

- Direct messages, mentions and replies to the bot are answered. Their text arrives without the
  privileged *Message Content* intent, so it stays off unless you want `reply_on = ["all"]`.
- Replies quote the message they answer and show "typing…". The bot never pings: `@everyone`,
  roles and users in an answer notify no one.
- Buttons **and forms** for approvals and prompts (one-time codes, API keys never reach the agent).
- With `[[command]]` configured, okbot registers Discord slash commands.
- 2000 characters per message; longer answers are split. Tables become lists.
- The bot can open a DM with a member for reminders and notifications, unless they block DMs from
  server members.
