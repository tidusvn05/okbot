# Automations

An automation watches something and acts without being asked: "tell me when my manager writes",
"every weekday at 8, summarise unread mail", "post new form answers to the team"
([ADR 0013](adr/0013-automations.md)). You write automations in `okbot.toml`. Each one has a
**trigger**, an optional **source** and an **action**:

| Trigger | Runs | Acts on |
|---|---|---|
| `every = "5m"` (1 minute to 7 days) | polls the source | new items only |
| `schedule = "weekdays 08:00"` (`daily 18:30`, `weekly mon,fri 09:00`, `monthly 1 09:00`, `every 3h`) | at those times, in `[bot] timezone` | every item (nothing when the source is empty) |
| `webhook = true` + `token_env` | `POST /automation/<name>` | the posted JSON, once per `id` |

| Action | Does | Costs |
|---|---|---|
| `text = "…"` + `to = [...]` | sends a template to chats | no agent, no quota |
| `prompt = "…"` | asks the agent as `run_as`, with the items as data | one agent turn per run |

The **source** is one tool of an `[[mcp]]` server, called without the agent and with `run_as`'s
credentials (their own Gmail through `/connect`, see [services.md](services.md)). What it reads with
someone's own account goes only to them: `to` may name their direct chat or a room marked
`personal`, nothing else. Pick a tool that only reads: sources are not asked about.

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

Each poll calls the tool. The first poll only records what is already there. Later polls start an
agent turn when the result changes. Without `items`, the whole result text is one item, identified
by its content, so a new mail changes it (and so does a mail being read). When a tool returns JSON
(as structured content, or JSON text), name the list and the id so that each item counts once. For
example, for a tool `list_unread` that returns `{"messages": [{"id": …, "from": …, "subject": …}]}`:

```toml
[automation.source]
mcp = "mail"
tool = "list_unread"
items = "/messages"   # JSON pointer to the list
id = "/id"            # JSON pointer to each item's id
```

Check a tool's output format in its server's documentation, or call it once with a
`[[command]]`.

The answer goes to `run_as`'s direct chat, or to the one chat in `to`.

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
tool = "list_unread"          # the JSON tool from the example above
with = { from = "boss@example.com" }
items = "/messages"
id = "/id"
```

Templates take `{{item}}` (the item, as text or JSON), `{{item.field.sub}}`, `{{count}}` and
`{{name}}`. With `{{item…}}`, the template repeats once per new item, and the results are joined
into one message. `to` takes the [`[[notify]]` targets](notify.md): `room:<key>`,
`user:<platform>:<instance>/<id>`, `person:<id>`, `admins`.

## A morning briefing

```toml
[[automation]]
name = "morning"
schedule = "weekdays 07:30"
prompt = "Look at my calendar and unread mail for today and give me a short plan."
```

A schedule without a source just runs the prompt (like a routine). With a source, it acts only
when the source returns something. When the bot was down at a planned time, the missed run happens
when it starts, if it is no later than `[schedule] late_grace_minutes`.

## Forms, CI and other programs

```toml
[http]
listen = "127.0.0.1:8787"

[[automation]]
name = "form"
webhook = true
token_env = "OKBOT_FORM_TOKEN"    # in .env; never a bearer [[mcp]] token
text = "📝 New answer from {{item.name}}: {{item.message}}"
to = ["room:discord:team|1104…|1290…|-"]
```

```sh
curl -fsS http://127.0.0.1:8787/automation/form \
  -H "Authorization: Bearer $OKBOT_FORM_TOKEN" -H 'Content-Type: application/json' \
  -d '{"id": "row-42", "name": "Lan", "message": "Hello"}'
```

A body with an `id` that was already posted answers `{"duplicate": true}`. The answer is 200 when
it acted, 401 for a wrong name or token, 409 when paused, 429 over the daily limit, and 502 when
delivery failed.

## Limits and safety

- `max_runs_per_day` (24 with a prompt, 200 with a text) and `max_items` per run (20; the rest are
  counted as "…and N more" and come with the next poll). `paused = true` stops an automation.
- A prompt's turn is **restricted**. It has no web tools (`mcp-only`), so mail cannot make it fetch
  a URL that leaks data. Every `[[mcp]]` call not marked read-only waits for `run_as`'s
  **Allow**, whatever the server's `confirm` says. The bot's own tools only read: no memory, task,
  note or knowledge changes. It never batches with messages a person sends. The history keeps only
  its first line, and the agent session is emptied afterwards, so the raw data does not reach later
  turns (the agent's answer does, like any answer).
- Only the operator writes automations, so a prompt injection cannot create one. The agent can
  create routines (paused until you `/resume`) for scheduled prompts in chat.
- When a source needs an account that is not connected, `run_as` is told once a day to
  `/connect` it.
- New items whose action fails (no chat yet, a platform error) come again with the next poll; a
  webhook that answered with an error can be retried with the same `id`. `okbot automation list`
  and `okbot doctor` show each automation's last runs and errors.

For flows with many steps, branches or other services, use a workflow engine (n8n, Activepieces)
that posts to an automation webhook or to [`[[notify]]`](notify.md).
