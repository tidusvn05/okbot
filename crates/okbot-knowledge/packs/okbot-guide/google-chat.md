---
type: Guide
title: Google Chat
description: Creating a Google Chat app with a service account, choosing Pub/Sub or HTTP delivery, and how okbot behaves in Google Chat.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [google chat, gchat, chat app, kết nối google chat, pub/sub, service account]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Google Chat

Chat apps need a **Google Workspace** account (not a personal Gmail) and a Google Cloud project.

## 1. Create the app

1. In the Google Cloud console, create or pick a project and enable the **Google Chat API** (and
   the **Pub/Sub API** for Pub/Sub delivery).
2. Create a **service account** and a JSON key. okbot posts replies as this account.
3. Configure the Chat app (APIs & Services → Google Chat API → Configuration): name, avatar and the
   connection settings below.

## 2. Pub/Sub or HTTP

| | `pubsub` | `http` |
|---|---|---|
| Public URL | not needed | HTTPS URL to okbot's `/gchat/<instance>` |
| Setup | a topic and a pull subscription; grant `chat-api-push@system.gserviceaccount.com` **Pub/Sub Publisher** on the topic, and your service account **Pub/Sub Subscriber** on the subscription | none beyond the app |
| Billing | the project needs billing enabled (a small bot stays in the free tier) | not required |

```sh
okbot new corp --platform gchat \
  --gchat-credentials ~/Downloads/my-project-1234.json \
  --gchat-delivery pubsub --gchat-subscription projects/my-project/subscriptions/okbot
```

```toml
[channels.gchat.corp]
credentials = "gchat-service-account.json"   # copied into the bot directory, mode 0600
delivery = "pubsub"                          # or "http"
subscription = "projects/my-project/subscriptions/okbot"
# HTTP delivery needs exactly one of:
# audience_url = "https://bot.example.com/gchat/corp"
# audience_project_number = "123456789012"
reply_on = ["dm", "mention"]
```

## 3. What to expect

- Direct messages: one conversation per person. Spaces: the app gets messages that @mention it;
  each thread is a conversation and okbot replies in the thread.
- okbot posts "_Thinking…_" and replaces it with the answer (agent turns take longer than Chat's
  30-second window).
- Card buttons for approvals and prompts (no forms). 4096 characters per message.
- Ids look like `users/<id>` and `spaces/<id>`; `/whoami` shows them.
- The app cannot start a direct chat: people message it first.

Google Chat as a chat platform is separate from reading Gmail or Drive: for those, see
[Google Workspace](google-workspace.md).
