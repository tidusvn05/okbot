---
type: Guide
title: Automations
description: Making the bot act without being asked, such as reporting new mail to chat or a morning briefing, with triggers, sources, actions, limits and safety rules.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [automation, tự động, tự động hoá, báo cáo mail, báo mail về chat, webhook, lịch, morning brief, tóm tắt mail, n8n]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Automations

An `[[automation]]` in `okbot.toml` has a **trigger**, an optional **source** and one **action**.

| Trigger | Runs | Acts on |
|---|---|---|
| `every = "5m"` (1 minute to 7 days) | polls the source | new items only |
| `schedule = "weekdays 08:00"` | at those times, in `[bot] timezone` | every item |
| `webhook = true` + `token_env` | `POST /automation/<name>` | the posted JSON, once per `id` |

| Action | Does | Cost |
|---|---|---|
| `text = "…"` + `to = [...]` | sends a template | no agent, free |
| `prompt = "…"` | an agent turn as `run_as`, with the items as data | one agent turn |

The **source** is one tool of an `[[mcp]]` server, called with `run_as`'s connected account. Pick a
tool that only reads.

## Report new mail to chat

```toml
[[automation]]
name = "boss-mail"
every = "5m"
run_as = "person:lan"            # personal bots default to the owner
prompt = "Summarise each new mail in two lines and say if it needs an answer today."

[automation.source]
mcp = "google"
tool = "search_gmail_messages"
with = { query = "is:unread from:boss@example.com newer_than:1d" }
```

The first poll only records what is there; later polls act on what is new. When the tool returns
JSON, name the list and the id so each item counts once:

```toml
[automation.source]
mcp = "mail"
tool = "list_unread"
items = "/messages"   # JSON pointer to the list
id = "/id"            # JSON pointer to each item's id
```

## The same without an agent

```toml
[[automation]]
name = "boss-mail-ping"
every = "2m"
run_as = "person:lan"
text = "📧 **{{item.from}}**: {{item.subject}}"
to = ["person:lan"]

[automation.source]
mcp = "mail"
tool = "list_unread"
items = "/messages"
id = "/id"
```

Templates take `{{item}}`, `{{item.field.sub}}`, `{{count}}` and `{{name}}`.

## A morning briefing

```toml
[[automation]]
name = "morning"
schedule = "weekdays 07:30"
prompt = "Look at my calendar and unread mail for today and give me a short plan."
```

## Forms, CI and other programs

```toml
[[automation]]
name = "form"
webhook = true
token_env = "OKBOT_FORM_TOKEN"
text = "📝 New answer from {{item.name}}: {{item.message}}"
to = ["room:discord:team|1104…|1290…|-"]
```

```sh
curl -fsS http://127.0.0.1:8787/automation/form -H "Authorization: Bearer $OKBOT_FORM_TOKEN" \
  -H 'Content-Type: application/json' -d '{"id": "row-42", "name": "Lan", "message": "Hello"}'
```

It needs `[http] listen`. For flows with many steps or branches, use a workflow engine (n8n,
Activepieces) that posts to an automation webhook or to a [notification](notifications.md).

## Limits and safety

- `max_runs_per_day` (24 with a prompt, 200 with a text), `max_items` per run (20), `paused = true`.
- A prompt's turn has **no web tools**, every MCP call not marked read-only waits for `run_as`'s
  **Allow**, the bot's own tools only read, and the raw data is not kept in the history.
- What a source reads with someone's own account goes only to them or a `personal` room.
- Only the operator writes automations, so a prompt injection cannot create one.
- `okbot automation list` and `okbot doctor` show the last runs and errors.
