# A personal assistant

What a bot created with `--profile personal` can do for its owner, and how to use it from chat and
the terminal. The design is in [design §5.5–5.9](design.md#55-assistant-data-the-areas-of-the-bundle).

Set the time zone first: reminders and notes use it.

```toml
# okbot.toml
[bot]
timezone = "Asia/Ho_Chi_Minh"
```

Everything below is stored as markdown in the bot's `kb/` folder, so you can read and edit it with any
editor, `okbase view`, or `okbot memory edit <id>`. When `kb/` is a git repository, every change the
bot makes is a commit.

## Memory: lasting facts

Ask in chat: "remember that I take my coffee black", "what do you remember about me?", "I moved to
Da Nang, update that". The bot keeps one short note per fact in `kb/memory/people/owner/` and reads
them before every answer. Commands: `/remember <text>`, `/memories`, `/forget <id>`.
Terminal: `okbot memory list|log|edit|forget`.

## To-dos and reminders

Ask in chat: "remind me to call mum at 8 pm", "every weekday at 9:00 remind me to check email", "add
buy milk to my list", "what's on my list?". The bot answers with the time it stored; at that time it
sends `⏰ 20:00 · **Call mum**` to the chat where you asked (or to the channel you name: "remind me on
Discord").

| Command | |
|---|---|
| `/tasks`, `/tasks all` | your to-dos and reminders, numbered |
| `/done <n>` | finish one |
| `/snooze [10m\|2h\|1d\|15:30]` | the last reminder again later (10 minutes by default) |
| `/pause <n>`, `/resume <n>` | stop and start one |

Terminal: `okbot task list [--upcoming|--all]`, `okbot task add "Pay rent" --remind "2026-11-01 09:00"
[--repeat "monthly 1 09:00"] [--to discord:main]`, `okbot task done|pause|resume|rm <id>`.
Repeat rules: `daily 08:00`, `weekdays 08:00`, `weekly mon,fri 09:00`, `monthly 1 09:00`, `every 3h`.

A reminder reaches you only on a channel where you have written to the bot in a direct message at
least once. If the bot was stopped when a reminder was due, it is sent late within 6 hours
(`[schedule] late_grace_minutes`), otherwise reported by `okbot doctor`.

## Routines: the agent at a set time

Ask in chat: "every morning at 7, summarise my tasks for today". A routine runs the agent with your
instruction at each time and posts its answer, as if you had asked then.

New routines **start paused**: the bot replies with `/resume <id>`, and the routine runs only after you
send it. This keeps a web page the agent read from setting up a recurring job. Changing a routine's
instruction pauses it again. At most 24 routine runs a day (`[schedule] max_agent_runs_per_day`).
Terminal: `okbot task add "Morning brief" --repeat "daily 07:00" --routine "Summarise my tasks for
today"` (starts at once).

## Quick notes and journal

Ask in chat: "note: met Minh about project X", or send `/note <text>`. Notes go to one file per day
(`kb/notes/people/owner/2026/2026-10-14.md`) and you find them later by asking: "what did I note about
project X last week?". Memory is for a few facts the bot always keeps in mind; notes are a log it
searches when asked.

## Saving documents

Send a PDF, Word or PowerPoint file, an image or a link and say "save this". The bot stores it in
`kb/knowledge/inbox/` and can answer from it right away. Only the owner can do this, because
`knowledge/` is what the bot uses to answer everyone. Files can be sent on Discord; Zalo delivers
images only.

Terminal: `okbot kb add <file|https://…> [--title …]`, `okbot kb inbox` (what was saved, to move into
the right folder later).

## Settings

```toml
[schedule]
enabled = true                 # send reminders and run routines
deliver_to = "discord:main"    # where reminders made in the terminal go (default: any chat you used)
late_grace_minutes = 360
max_agent_runs_per_day = 24
```
