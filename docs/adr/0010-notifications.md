# 0010 — Notifications from other programs

Status: Accepted (2026-10-20)

## Context
Bots that watch something (a fleet of servers, CI, an order queue) must tell people when it changes. Without
okbot's help, every such program needs its own platform client: the bot-core ops bot's watcher keeps its own
Discord token, calls the REST API, splits text under 2000 characters and opens admins' DMs by hand. Each new
platform means another client, the messages bypass okbot's rendering, and the bot's history has no idea an
alert was sent, so "what does this alert mean?" reaches an agent that never saw it.

okbot already knows how to reach every platform. Two things differ per platform and must not leak into callers:
how a chat is addressed, and whether a bot may start a direct chat. Telegram and Discord let a bot open one with
any user it can see; Zalo bots and most Slack/Google Chat setups can only answer chats people started.

## Decision
- **`[[notify]]` names a destination**: `name`, `token_env` (the bearer token callers send) and `to`, a list of
  targets. A caller posts `{"text": "<Markdown>", "id": "<optional>"}` to `POST /notify/<name>` on the bot's
  `[http] listen`. The caller cannot choose chats; the operator lists them, and each token reaches only its own
  targets. A bearer `[[mcp]]` token cannot be a notify token (the agent has it in its environment).
- **Four target forms, the same on every platform**: `room:<conversation key>` (the exact key `/whoami` shows
  administrators, so spaces and threads need no per-platform syntax), `user:<platform>:<instance>/<id>`,
  `person:<id>` (one direct chat of a `[people]` person, `[schedule] deliver_to` first) and `admins`.
- **Direct chats**: the one the bot has seen (`direct_chats`, as reminders use) first; otherwise the new
  `Channel::open_direct(user)`, which defaults to `Unsupported`. Discord (create DM) and Telegram (private chat id
  = user id) implement it. Other platforms report "the person must write to the bot first"; `okbot doctor` warns.
- **Rendering and history as for replies**: the text is rendered per channel (dialect, splitting). Each delivered
  chat gets the text in its history, marked as another program's notification — information, not instructions.
- **Answers say what happened per chat**: `200` all delivered, `207` some, `503` none but every failure may pass
  later (`Retry-After` when the platform said), `502` none, `401` for an unknown name or wrong token alike.
  An `id` delivered within an hour is not sent again (in memory; released when nothing was delivered).
- `okbot send <name> [text]` posts through the running bot for cron jobs and scripts on the same machine.

## Consequences
- A watcher needs one HTTP call for every platform, and the operator moves alerts between chats or platforms in
  `okbot.toml` without touching the caller.
- No agent runs and no quota is used; a notification works while every agent CLI is down.
- The history mark relies on the agent treating the bot's earlier messages as data; the text comes from a caller
  holding the token, never from chat users.
- Dedupe ids are forgotten on restart. Callers that need exactly-once across restarts keep their own state.
- Bridge adapters do not open direct chats yet (protocol unchanged); they use the chats the bot has seen.
