# Notifications from other programs

A monitoring script, a CI job or your own service can post a message through the bot to the chats you choose,
on every platform the bot runs on: one HTTP call, rendered for each platform, no agent and no quota
([ADR 0010](adr/0010-notifications.md)).

## Set it up

1. Find the chats. Send `/whoami` in the room as a `[bot] admins` member: the answer ends with
   `For [[notify]] to, this chat is room:…`. People are addressed by account or by `[people]` id.
2. Declare a target in `okbot.toml` and put a token in `.env` (`openssl rand -hex 32`):

   ```toml
   [http]
   listen = "127.0.0.1:8787"            # callers post here; required for [[notify]]

   [[notify]]
   name = "alerts"                      # POST /notify/alerts
   token_env = "OKBOT_NOTIFY_ALERTS"    # Authorization: Bearer <token>
   to = [
     "room:discord:ops|1104…|1290…|-",  # a room, exactly as /whoami shows it
     "admins",                          # every [bot] admins account, in a direct chat
   ]

   [[notify]]
   name = "deploys"
   token_env = "OKBOT_NOTIFY_DEPLOYS"
   to = ["person:lan", "user:telegram:main/12345678"]
   ```

3. `okbot doctor` checks the token and warns about people the bot cannot reach yet; `okbot restart <bot>`.

| Target | Goes to |
|---|---|
| `room:<conversation>` | that room, channel, space or thread |
| `user:<platform>:<instance>/<user id>` | that account's direct chat (ids from `/whoami`) |
| `person:<id>` | one direct chat of a `[people]` person: a platform where they already wrote first, `[schedule] deliver_to` preferred (`person:owner` on a personal bot) |
| `admins` | the direct chat of every `[bot] admins` account (and the owner of a personal bot) |

A chat named twice (a user who is also an admin) gets the message once. The caller cannot add chats: each
token reaches only its own `to` list, so give every caller its own target and token. Never reuse the token
of a bearer `[[mcp]]` server — the agent has that one.

## Send

```sh
curl -fsS http://127.0.0.1:8787/notify/alerts \
  -H "Authorization: Bearer $OKBOT_NOTIFY_ALERTS" \
  -H 'Content-Type: application/json' \
  -d '{"text": "**vps01** disk 91%", "id": "vps01-disk-2026-10-20"}'

okbot send alerts "**vps01** disk 91%"          # from the bot's machine (reads .env)
journalctl -n 20 | okbot send alerts --id daily-log   # text from stdin
```

- `text` is Markdown, at most 8000 characters; long texts are split where a platform needs it.
- `id` is optional: the same id within an hour is not sent again (`{"duplicate": true}`), so a caller can retry
  safely. When nothing was delivered the id is released, so a retry tries again. Ids are forgotten when the bot
  restarts.

The answer lists every chat:

```json
{"sent": [{"to": "admins: discord:ops/2106…", "conversation": "discord:ops|-|1301…|-", "messages": 1}],
 "failed": [{"to": "person:lan", "error": "no direct chat yet: on zalo, the person must write to the bot first", "retry": false}]}
```

| Status | Meaning |
|---|---|
| 200 | delivered to every chat (or a duplicate `id`) |
| 207 | delivered to some chats; see `failed` |
| 503 | delivered nowhere, but every failure may pass later (`Retry-After` when the platform said how long) |
| 502 | delivered nowhere |
| 400 / 413 | bad request / text too long |
| 401 | unknown target name or wrong token (they look the same) |

`okbot send` exits 0 only when every chat got the message.

## Platforms

Rooms work everywhere the bot can post. Direct chats with someone who has never written to the bot depend on the
platform:

| Platform | Rooms | Direct chat without a first message | Notes |
|---|---|---|---|
| Discord | channels and threads the bot can see | yes, okbot opens the DM | fails when the person blocks DMs from server members |
| Telegram | groups, channels, forum topics | yes (private chat id = user id) | Telegram refuses until the person pressed **Start** once |
| Slack | channels the bot was invited to, threads | no: they write to the bot first | |
| Google Chat | spaces the app is in, threads | no: they message the app first | |
| Zalo Bot | chats | no: they write to the bot first | |
| Bridge adapters | rooms of the adapter | no (not in the bridge protocol yet) | |

Once a person has written to the bot in a direct chat, okbot remembers that chat (as for reminders) and uses it on
every platform. Formatting follows each platform's dialect: Discord Markdown, Slack mrkdwn, Google Chat text
formatting, Telegram HTML, and Zalo's Markdown (no links, code blocks or tables).

## History

Each delivered chat keeps the text in its history, marked as a notification posted by another program. When
someone asks "what does this alert mean?", the agent sees it — as information, never as instructions. Only
callers holding the token can write there, never chat users.
