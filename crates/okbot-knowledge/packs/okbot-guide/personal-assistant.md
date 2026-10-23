---
type: Guide
title: Personal assistant features
description: Memory, to-dos and reminders, routines, quick notes and saved documents, with their chat commands and terminal commands.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [nhắc nhở, reminder, ghi nhớ, memory, task, việc cần làm, routine, ghi chú, note, lưu tài liệu, múi giờ, timezone]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Personal assistant features

Set the time zone first; reminders and notes use it:

```toml
[bot]
timezone = "Asia/Ho_Chi_Minh"
```

## Memory

"Remember that I take my coffee black", "what do you remember about me?". The bot keeps short notes
in `kb/memory/` and reads them before every answer.
Chat: `/remember <text>`, `/memories`, `/forget <id>`. Terminal: `okbot memory list|log|edit|forget`.

## To-dos and reminders

"Remind me to call mum at 8 pm", "every weekday at 9:00 remind me to check email".

| Chat | |
|---|---|
| `/tasks`, `/tasks all` | your to-dos and reminders |
| `/done <n>` | finish one |
| `/snooze [10m\|2h\|1d\|15:30]` | the last reminder again later |
| `/pause <n>`, `/resume <n>` | stop and start one |

Terminal: `okbot task add "Pay rent" --remind "2026-11-01 09:00" --repeat "monthly 1 09:00"`.
Repeat rules: `daily 08:00`, `weekdays 08:00`, `weekly mon,fri 09:00`, `monthly 1 09:00`,
`every 3h`. A reminder reaches you only on a platform where you have written to the bot directly
at least once.

## Routines

"Every morning at 7, summarise my tasks for today" runs the agent at those times. New routines
**start paused**: send `/resume <id>` to start one. This stops a web page the agent read from
setting up a recurring job. At most 24 routine runs a day.

For routines that read mail or calendars and report, use [Automations](automations.md).

## Notes

"Note: met Minh about project X", or `/note <text>`. Notes go into one file per day and are searched
when you ask ("what did I note about project X last week?").

## Saving documents

Send a PDF, Word file, image or link and say "save this". It goes to `kb/knowledge/inbox/` and can be
used at once. Only the owner (personal bot) can do this. Zalo delivers images only.

## Settings

```toml
[schedule]
enabled = true
deliver_to = "telegram:me"     # where reminders made in the terminal go
late_grace_minutes = 360       # a reminder missed while the bot was down is still sent within this
max_agent_runs_per_day = 24
```
