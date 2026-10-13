# 0006 — Tasks as bundle files, firings in SQLite, routines as scheduled turns

Status: Accepted (2026-10-13)

## Context
A personal assistant needs to-dos, reminders and scheduled agent work ("every morning, summarise my
day"). Until now the bot only answered messages: an agent cannot wait or send later (the prompt
forbids it), and nothing in okbot runs on a timer. Reminders and to-dos overlap in how people ask for
them. Hard rule 5 keeps user data in files and only operational data in SQLite.

## Decision
- One `Task` model with three kinds: `todo`, `reminder`, `routine` (design §5.6). Tasks are OKF
  markdown files in `kb/tasks/<owner folder>/`, using the same owner folders and scope rules as memory.
- SQLite records only firings (`task_fires`, keyed by task and firing time), which makes delivery
  idempotent across crashes and restarts.
- A scheduler in `okbot-runtime` (through `Clock`) sends reminders verbatim with `Channel::send`; no
  agent runs for a plain reminder.
- A routine fires an `InboundEvent` with `source: Schedule` into its conversation queue, so it uses the
  normal pipeline. Only the owner (personal) or admins (team) may create routines, the role is checked
  at every firing, runs are capped per day, and routines created during a turn that used web tools
  start paused until confirmed in chat.
- `okbot-core` gains a `TaskStore` port; `okbot-knowledge` implements it on the bundle.

## Consequences
- Users and operators can read and edit tasks as files (`okbase view`, an editor, git), and the agent
  can query them with `kb_query type=Task`.
- The scheduler re-reads task files every 30 seconds, so hand edits and new tasks need no signal.
- Routines spend agent quota without a user present; the daily cap and the role check bound it.
- `InboundEvent` gets a `source` field, a change to the public SDK before 1.0.
