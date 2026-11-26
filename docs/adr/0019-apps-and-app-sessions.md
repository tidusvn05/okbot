# 0019 — Apps: sandboxed web pages behind chat summaries, opened with launch tickets

Status: Accepted (2026-11-26)

## Context
Chat is a poor place for a long list: 34 open tasks, a month of reminders, the runs of an
automation. Replies get cut, and paging by command is slow. Views (ADR 0015, 0017) give cards and
buttons, but are limited to a few cards per message by every platform. People want a summary in
chat and the full picture one press away, without a login form. The data behind that picture is
personal (tasks, notes, memory) or belongs to a room. So the page must know who pressed and where,
must not let someone else reuse the link, and must apply the same visibility rules as chat
(design §5.3). Operators also want apps okbot did not write, so apps must run code nobody in the
okbot project reviewed without putting the bot or other people's data at risk. Full design:
[`docs/design-apps.md`](../design-apps.md).

## Decision
- **An app is a manifest and static files** (`app.toml`, HTML, JS, CSS), served by the bot under
  `[http] public_url` + `/apps/`. Nothing from an app runs in the okbot process. Built-in apps are
  embedded in the binary and use the same API as every other app. Apps never start an agent turn.
- **Sandbox.** okbot's shell page loads each app in an `<iframe sandbox="allow-scripts
  allow-forms">` without `allow-same-origin`. App files are served with a CSP that allows
  scripts, styles, images and fonts from the bot only and sets `connect-src 'none'`,
  `form-action 'none'` and `frame-ancestors` to the bot. The app has no cookies, no `fetch`, no
  forms, no pop-ups and no top navigation. It calls the **App API v1** through the shell over a
  `MessageChannel` port that the shell hands to the frame's first page (`/apps/sdk/v1.js`).
  The sandbox cannot stop a frame from navigating itself, which could carry data out in a URL.
  So the shell closes the channel and the frame at any second page load, and registry review and
  the permission grants are what bound a bad app. One app is one page.
  The shell, and then the server again, check each call against the app's granted permissions
  (`me`, `tasks:read|write`, `notes:*`, `memory:*`, `knowledge:read`, `schedule:read`,
  `automations:read`, `connections:read`, `mcp:<server>/<tool>`).
- **Identity comes from the chat platform.** Pressing an app button (an `app` button kind in
  views; list commands such as `/tasks` show the first 10 items and an Open button) mints a **launch
  ticket for the presser**: 32 random bytes, valid 5 minutes, single use, bound to the identity,
  the context (`direct` or the room) and the app view. Only its SHA-256 is stored (`app_tickets`).
  Where the platform reports the presser, the ticket goes to them privately: a Discord ephemeral
  follow-up, Slack `chat.postEphemeral`, a Google Chat private message, a Telegram direct
  message (with an alert only the presser sees). Elsewhere (Zalo, the console), the link is sent
  only in a direct chat.
- **Tickets travel in the URL fragment** (`/apps/#t=…`), which browsers do not send to servers. The
  shell posts the ticket to `POST /apps/api/session`, removes it from the address bar and gets an
  `HttpOnly; Secure; SameSite=Lax; Path=/apps` session cookie (`app_sessions`, hashed). Sessions
  last 12 h idle and 7 days in all on team bots, and 30 days on personal bots. A room stays in a
  session for 12 h after its last press: rooms have members who leave, and platforms do not tell
  okbot. Places have stable ids within a session. A ticket for another account than the one signed
  in asks before it switches (login CSRF). Ending a session cannot be undone by a request already
  in flight: requests only update a session that exists. API calls also need
  a CSRF header and the bot's `Origin`.
- **Sessions carry who and where, never permissions.** Each API call rebuilds the requester's
  `Scope` from the identity and the chosen context, and checks the access lists, roles, personal-bot
  owner and `[apps.<name>] rooms|roles|permissions` as they are at that moment (ADR 0018). A room
  context never unlocks personal data. A `direct` context never unlocks another room. Writes go through the
  functions chat commands use, are audited with `source = app:<name>`, and keep their rules: routines
  still need the owner or an admin. A `mcp.call` whose tool needs `[[mcp]] confirm` shows a dialog in the
  trusted shell with the tool's arguments, and the person's click there is the approval. okbot's
  dialogs say who asks, focus Cancel, ignore presses for a moment and come one at a time, so an
  app cannot time one to catch a keystroke.
- **Operators install apps; nobody else.** `okbot app add|remove|update` and `[apps]` in
  `okbot.toml`. The agent and chat users cannot install, enable or widen an app. The agent's `show`
  may name an installed app and a declared view, never a URL. Apps need `[http] public_url` with
  `https`. Without it they are off and list commands keep their text fallback (`[Next 10]`).
- `/app <name>` opens an app, and `/app sessions` and `/app logout` manage sessions.
  `okbot app sessions` does the same for operators. A new crate, `okbot-apps`, holds manifests,
  built-in apps, the shell, the SDK and the App API router. It depends only on `okbot-core` ports, and the
  binary mounts its routes.

## Consequences
- Long lists, calendars and run histories have a real page, reached in one or two presses with no
  password, and they cost no agent quota.
- A link seen by someone else is useless after 5 minutes or one use, and unfurlers cannot consume
  it. In groups, each member's press opens their own view with their own rights.
- Removing someone's access takes effect on their next API call, not at session expiry.
- Third-party app code can show wrong data or annoy, but cannot read cookies, call methods it was
  not granted or act outside the presser's rights. It can still carry what it was allowed to read
  out of the browser by navigating its frame (the frame is closed when it does): registry review
  and narrow grants are the defence there.
- Bots without a public HTTPS address get no apps. Schema version bump for `app_tickets` and
  `app_sessions`, kept through `ConversationStore` (hashes only). `UiCaps.private_reply` says a
  platform can answer a presser alone, and `InboundSink::launch` turns a press into that answer.
  Every platform with buttons reports the presser, so no other capability is needed.
- Telegram's `web_app` and `login_url` buttons, which would sign the presser in one press, are
  left for later. The link in the direct chat works everywhere and needs no BotFather domain
  setup.
- Built-in app front-ends are plain ES modules with no build step, so the repository and
  `scripts/verify.sh` stay free of a Node toolchain.
