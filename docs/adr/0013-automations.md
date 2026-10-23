# 0013 — Automations: triggers, sources and actions

Status: Accepted (2026-10-23)

## Context
A personal assistant earns its keep when it acts without being asked: "tell me when my manager
writes", "every weekday at 8, summarise unread mail", "when the form gets a new row, post it to the
team". okbot has the parts: routines run an agent prompt on a schedule (ADR 0006), `[[notify]]`
posts another program's text (ADR 0010), `[[command]]` calls one MCP tool without the agent
(ADR 0008), and OAuth reaches Gmail, Drive and Notion (ADR 0012). Missing are triggers other than a
clock, a way to notice what is *new*, and a way to act without spending an agent turn on every
check. Workflow engines (n8n, Activepieces) do this well and can already post to `[[notify]]`; the
built-in part should cover the assistant's everyday cases with okbot's own permissions and
credentials, and leave complex flows to those engines.

## Decision
- **`[[automation]]` in `okbot.toml`**, written by the operator: `name`, `run_as` (a `[people]`
  person, `person:owner`, or an account), one trigger, an optional source and one action. A source
  that reads `run_as`'s own account may only send to them or to a `personal` room.
- **Triggers.** `every = "5m"` (1 minute to 7 days) polls the source and acts on new items only;
  `schedule = "weekdays 08:00"` (the task repeat syntax) acts every time; `webhook = true` with
  `token_env` accepts `POST /automation/<name>` (bearer token, JSON body) and acts on new items.
- **Source.** `[automation.source] mcp, tool, with` calls one `[[mcp]]` tool with `run_as`'s
  credentials, without the agent. `items` (a JSON pointer to an array in the tool's structured
  content, or in its text parsed as JSON) splits the result into items, `id` (a pointer in each
  item) identifies them; without `items` the whole text is one item identified by its hash. Seen ids
  are kept in SQLite (`automation_items`), so a restart does not repeat them; the first poll only
  records what is already there. Items past `max_items`, and items whose action failed, are released
  and come with the next poll.
- **Actions.** `text` sends a template (`{{item}}`, `{{item.field}}`, `{{count}}`, `{{name}}`) to
  `to` (the `[[notify]]` targets) without an agent and without quota. `prompt` starts an agent turn
  as `run_as` in their direct chat (or the one room in `to`), with the items appended as data. The
  turn is restricted: `mcp-only` (no web, so data cannot be sent out through a URL), every
  `[[mcp]]` call that is not marked read-only waits for `run_as`'s approval (whatever the server's
  `confirm` says), the bot's own tools only read (no memory, task, note or knowledge writes), it
  never batches with messages a person sent, the history keeps only its first line and the agent
  session is emptied afterwards.
- **Limits.** `max_runs_per_day` per automation (24 for prompts, 200 for texts), `max_items` per run
  (20). Each run is recorded (`automation_runs`: time, items, action, error) for `okbot automation
  list` and `okbot doctor`. A source that needs a connection the person has not made tells them once
  a day.
- **Chat-created schedules stay routines** (ADR 0006): the agent may create those, paused until the
  person resumes them. Automations come only from the configuration, so a prompt injection cannot
  add one.

## Consequences
- "Report new mail to chat" is a few lines of configuration over the person's own Gmail connection;
  checking costs one MCP call, and only new mail costs an agent turn (or none, with `text`).
- Text from mail, forms and webhooks reaches the agent as data inside a restricted turn; the
  remaining risk is the agent misreading it, bounded by read-only tools and approvals. Its answer is
  kept in the history like any answer, so text it repeats from the data reaches later turns as the
  bot's own words.
- Polling is not push: `every` trades latency for simplicity. Gmail push through Pub/Sub can be a
  later trigger; webhooks already serve services that push.
- Complex flows (branching, many services, retries per step) belong in a workflow engine posting to
  `[[notify]]` or an automation webhook.
