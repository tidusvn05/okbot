# okbot apps — design proposal

> Version 0.2 (2026-11-26) · Status: **Accepted** (ADR 0019, ADR 0020) · Target: 0.14 → 0.16
> Builds on: ADR 0006 (tasks), 0007 (signed identity), 0012 (OAuth, `[http] public_url`),
> 0013 (automations), 0015 (interactive replies), 0017 (views), 0018 (reloading access lists)

## 0. Summary

A chat message is a poor place for a list of 40 tasks, a month of reminders or the run history of
an automation. **Apps** are small web pages served by the bot itself that show the full picture
behind a chat summary. The chat stays the entry point: `/tasks` shows the ten most urgent tasks and
an **Open Tasks** button; the button opens the Tasks app, already signed in as the person who
pressed it, showing only what that person may see in that place.

```
 chat                                   browser
┌──────────────────────────────┐       ┌──────────────────────────────────────────┐
│ /tasks                       │       │ https://bot.example.com/apps/            │
│ 📋 10 of 34 open tasks       │ press │ ┌ okbot shell (trusted) ───────────────┐ │
│ • File report — Fri          │──────►│ │ Signed in as Ana (Discord) · Log out │ │
│ • Call bank — today 15:00    │ link  │ │ ┌ sandboxed iframe: app "tasks" ───┐ │ │
│ …                            │ with  │ │ │  filters, all 34 tasks, Done,    │ │ │
│ [Open Tasks]                 │ ticket│ │ │  Snooze, edit …                  │ │ │
└──────────────────────────────┘       │ │ └──────── postMessage ▲ ──────────┘ │ │
                                       │ └── App API (session, Scope) ─┼──────┘ │
                                       └───────────────────────────────┼────────┘
                                                                        ▼
                                                     okbot: TaskStore, notes, memory, …
```

Three kinds of apps, one model:

| Kind | Where it comes from | Trust |
|---|---|---|
| **Built-in** | Embedded in the binary (`include_dir`, like knowledge packs) | okbot's own code |
| **Registry** | The `okbot-apps` repository, fetched by `okbot app add <name>` without a new okbot release | Reviewed by maintainers, signed index |
| **Local** | A directory or any git URL the operator points to | Operator's responsibility, marked *unverified* |

All three are the same thing on disk (a manifest plus static files) and run in the same sandbox
against the same **App API**. Built-in apps are not special-cased: they prove the API is enough.

### Decisions proposed

| # | Question | Proposal |
|---|---|---|
| A1 | What is an app? | **Static files + manifest, no server-side code.** HTML/JS/CSS run in the browser; all data goes through okbot's versioned App API. Nothing from an app ever runs inside the okbot process |
| A2 | How is third-party JS contained? | Each app runs in a **sandboxed iframe with an opaque origin** inside okbot's shell page; it has no cookies, no network (`connect-src 'none'`) and talks to the shell over `postMessage`. The shell holds the session and enforces the app's declared permissions |
| A3 | How does a click become a signed-in browser? | **One-time launch ticket** bound to the verified clicker, carried in the URL **fragment**, exchanged for a session cookie. Where the platform reports who clicked (Discord, Slack, Google Chat, Telegram), the ticket is minted *at the click* and delivered privately to the clicker |
| A4 | Who may install apps? | **Operators only**, from the terminal (`okbot app add`), like automations (ADR 0013). Chat users and the agent cannot install, enable or widen an app |
| A5 | How do registry apps update without a release? | A **signed `index.json`** in a separate `okbot-apps` repository pins each app to a commit and a SHA-256 per file; okbot verifies with an Ed25519 public key built into the binary. Updates are explicit (`okbot app update`), never automatic |
| A6 | Cost | Apps never start an agent turn. They read and write through the same code paths as `/tasks`, `/done`, `okbot task`, so they cost no quota |
| A7 | Front-end stack for built-in apps | **No build step**: plain ES modules and Web Components, checked in as written. The repo stays Rust-only; `scripts/verify.sh` needs no Node toolchain |
| A8 | Where does the registry live? | **A separate repository, `tidusvn05/okbot-apps`.** Built-in apps stay in the okbot repository (they ship with the binary); registry apps get their own reviews, CODEOWNERS, issues and signing workflow (§6.1) |
| A9 | Trust tiers | **Three, no more:** built-in (in the binary), registry (reviewed and signed, one key), local (unverified). The registry has no unsigned community section; anyone may run their own signed index |
| A10 | Remember this device? | **Yes for personal bots:** sessions last 30 days there; team bots keep 12 h idle / 7 days |
| A11 | Zalo | **A link in the direct chat** for now; a Zalo Mini App is a later option |

---

## 1. Requirements

| # | Requirement (from the request) | Design consequence |
|---|---|---|
| R1 | Built-in apps: tasks, schedule, automation, … the features okbot already has | Built-in apps over existing ports (`TaskStore`, notes, memory, automation runs); §5 |
| R2 | Users can add their own apps | Local apps and registry apps share the manifest, sandbox and API (§3, §6) |
| R3 | Marketplace = a folder in okbot's git repo, no new okbot version needed | Registry client, signed index, `api` compatibility field (§6) |
| R4 | Chat shows a summary; "see more" opens the app | Summary + `app` button in command replies and views (§4) |
| R5 | "See more" differs per platform: Discord button, elsewhere a URL | `ChannelCaps.ui` decides; callback button where the platform verifies the clicker, link elsewhere (§4.2) |
| R6 | The click must be authenticated: the right person, with the right to see it, and smooth UX | Launch tickets, sessions, per-request Scope and access checks (§2) |

Non-goals for this proposal: apps with their own backend code (WASM plugins), payments, apps that
talk to the internet directly, embedding inside platform clients (Discord Activities, Zalo Mini App
platform). §9 lists them as later options.

---

## 2. Authentication and authorization

This is the part that has to be right first, because every app depends on it.

### 2.1 Principles

1. **Identity comes from the chat platform, never from the browser.** A browser is only ever
   trusted because it presented a ticket that okbot minted for a person the platform identified.
2. **Permissions are not snapshotted.** A session carries *who* and *where from*, not what they
   may do. Every App API call rebuilds the `Scope` and re-checks access lists, roles and
   `[apps.<name>]` rules as they are now (ADR 0018), exactly as a typed message would be.
3. **Personal data needs a private channel.** A ticket only grants the personal context (the
   person's own tasks, notes, memory) when it reached the person privately: a direct chat, an
   ephemeral reply, or a platform-signed login. A link posted where others can read it grants at
   most the room's context, or is not posted at all.
4. **Nothing reusable in a URL a server can see.** Tickets travel in the fragment (`#t=…`), which
   browsers never send to servers. Link unfurlers (Slack, Discord, Telegram previews), reverse
   proxy logs and `Referer` headers cannot see or consume them.

### 2.2 Objects

| Object | Contents | Lifetime | Stored |
|---|---|---|---|
| **Launch ticket** | 32 random bytes (base64url). Bound to: bot, identity (`person:<id>` or account), context (`direct` \| `room:<channel>/<room>`), `personal` flag, app, view path and params | 5 minutes, single use | SQLite `app_tickets`: SHA-256 of the ticket only |
| **Session** | Random 32-byte id in an `HttpOnly; Secure; SameSite=Lax; Path=/apps` cookie; identity; the contexts proven so far; CSRF secret; user agent hint | Team bots: 12 h idle, 7 days absolute. Personal bots: 30 days ("remember this device"), since the only person is the owner. Configurable | SQLite `app_sessions`: SHA-256 of the id |
| **App frame token** | Per app frame load: app name, granted permissions, context, nonce | Lives in the shell's JS memory only | Never |

A session gains contexts: opening Tasks from a group adds that room; opening it from a direct chat
adds `direct`. The shell shows a context switcher when there is more than one. A room context never
unlocks personal data, a `direct` context never unlocks another room.

### 2.3 Launch flow

```
Person presses [Open Tasks]
        │
        ├─ platform reports the clicker (Discord / Slack / Google Chat / Telegram callback)
        │     1. okbot receives the interaction like any view press (ADR 0015):
        │        rate limit, access gate, personal-bot owner, [apps.tasks] rooms/roles
        │     2. mints a ticket for THE CLICKER (not for whoever ran /tasks)
        │     3. answers privately: Discord ephemeral follow-up, Slack chat.postEphemeral,
        │        Google Chat privateMessageViewer, Telegram direct message to the clicker
        │        → a link button "Open Tasks ↗" (expires in 5 min)
        │
        └─ platform has no verified click (Zalo, console, plain-text fallback)
              the link itself is sent, and only in a direct chat; in a group the bot answers
              "Opened in our direct chat" and sends it there (or tells them to write /app tasks
              to the bot directly when it cannot start a DM)

Browser opens  https://bot.example.com/apps/#t=<ticket>&app=tasks&view=/open
        │
        4. shell JS reads the fragment, removes it (history.replaceState)
        5. POST /apps/api/session  {ticket}  (Origin checked)
        6. okbot: hash lookup, unused, unexpired → mark used; create or extend the session;
           Set-Cookie; respond with identity, contexts, the app's manifest and permissions
        7. shell shows "Signed in as Ana (Discord)" and loads the app frame
```

UX notes:

- **One press, no login form.** On every platform that has buttons, the person presses twice at
  most (Open → Open ↗). Telegram can do it in one (§4.2).
- **Sessions persist.** Within the session lifetime, later presses reuse the cookie; the ticket
  just adds a context and deep-links to the view. Bookmarks work while the session lives.
- **Expired link.** The page says so in the person's language and tells them which command
  produces a fresh one (`/app tasks`). It never asks for a password, because there is none.
- **Showing who is signed in** guards against "login CSRF" (someone sends you a ticket for *their*
  account): the banner names the account, and every write shows it.

### 2.4 Authorizing each call

```
App iframe ──postMessage("tasks.done", {id}, nonce)──► shell
shell: is "tasks:write" in this app's granted permissions? ─ no → reject
shell ──POST /apps/api/v1/tasks.done  (cookie + X-Okbot-Csrf + X-Okbot-App: tasks + context)──► okbot
okbot:
  1. session valid, not revoked; CSRF header matches; Origin is public_url
  2. app installed, enabled, permission granted to this app (checked again server-side)
  3. identity still allowed: access lists, personal-bot owner, [apps.tasks] roles and rooms
  4. Requester rebuilt from identity + context → Scope (design §5.3); personal only for direct
  5. the same function /done uses, with the requester as actor → audit row (source = app:tasks)
```

The shell's permission check is a convenience; the server's is the one that counts (step 2).

### 2.5 Revocation and abuse

- `/app logout` ends the sessions of the person; `/app sessions` lists them (device hint, last use).
- `okbot app sessions [--revoke <person>|--all]` for operators.
- Removing someone from an access list or role takes effect on their next API call (principle 2).
- Tickets: 5 wrong or reused tickets from one IP within a minute → 1-minute block for that IP.
- `[http] public_url` must be `https://`. Without it apps are disabled and the summary keeps a
  text fallback (`/tasks all`, numbered pages); `doctor` says how to enable them.

### 2.6 Telegram and later: platform-signed sign-in

Telegram signs who opened a button: `web_app` buttons (private chats) deliver `initData` signed with
the bot token, `login_url` buttons (groups) deliver Telegram Login data. okbot verifies the HMAC and
mints the session directly: one press, no ticket. This is an optimization of §2.3 on one platform,
not a different model: the session that results is the same.

---

## 3. What an app is

### 3.1 On disk

```
<bot>/apps/
├── tasks/            # a built-in app is not copied here; it is served from the binary
└── shopping-list/    # a registry or local app
    ├── app.toml
    ├── index.html
    ├── app.js
    ├── style.css
    └── locales/{en,vi}.json
```

```toml
# app.toml
name = "shopping-list"            # [a-z0-9-], unique per bot
title = { en = "Shopping list", vi = "Danh sách mua sắm" }
version = "0.3.0"
api = 1                            # App API major version it was written for
okbot = ">=0.15"                   # optional lower bound, for new App API methods
description = "Shared shopping list for a family room"
icon = "icon.svg"
license = "MIT"

# What the app may do through the App API. Shown at install, granted by the operator.
permissions = ["me", "tasks:read", "tasks:write"]

# Views that chat can link to (§4). Anything else in the app is reachable only from inside it.
[[view]]
path = "/open"
title = { en = "Open items", vi = "Việc chưa xong" }
params = ["tag"]                   # allowed query params; values are validated as short text
```

Rules checked at install and at start-up: unknown keys refused; files limited to an allow-list of
types (`html js mjs css json svg png webp woff2 txt`) and sizes (2 MB per file, 10 MB per app); no
symlinks; `index.html` present; every permission known.

### 3.2 Sandbox

The shell page (`/apps/`) is okbot's code. It renders the frame:

```html
<iframe sandbox="allow-scripts allow-forms" src="/apps/frame/shopping-list/index.html"></iframe>
```

- No `allow-same-origin`: the frame has an opaque origin, so it cannot read the session cookie,
  call the App API itself, read other apps or touch the shell's DOM.
- Frame responses carry `Content-Security-Policy: default-src 'none'; script-src 'self';
  style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'none'; form-action 'none';
  frame-ancestors <public_url>`, so nothing can be fetched or sent outside the bot, and the frame
  cannot be embedded elsewhere.
- No `allow-top-navigation` and no `allow-popups`: an app opens links through `okbot.openLink(url)`,
  which the shell shows as "Leave okbot for <host>?".
- The shell page itself: strict CSP, `X-Frame-Options: DENY`, no inline script.

The bridge is a small script the shell serves (`/apps/sdk/v1.js`):

```js
import { okbot } from "/apps/sdk/v1.js";
const me = await okbot.me();                       // { name, platform, context, locale, theme }
const { items, next } = await okbot.call("tasks.list", { status: "open", limit: 50 });
await okbot.call("tasks.done", { id: items[0].id });  // shell may ask for confirmation
okbot.on("context", ctx => reload());              // the person switched room/direct
```

### 3.3 App API v1

JSON over `POST /apps/api/v1/<method>`, reached only through the shell. Methods map to functions
that already exist behind chat commands and the bot's MCP tools; the API adds no new data paths.

| Permission | Methods | Backed by |
|---|---|---|
| `me` | `me` | session identity, locale, theme |
| `tasks:read` | `tasks.list` (filters, cursor), `tasks.get` | `TaskStore` with the requester's Scope |
| `tasks:write` | `tasks.add`, `tasks.update`, `tasks.done`, `tasks.snooze`, `tasks.pause`, `tasks.resume` | same rules as `task_*` tools; routines still need owner/admin and start paused when not created by a person (ADR 0006) |
| `notes:read` / `notes:write` | `notes.list`, `notes.get` / `notes.add` | notes writer (§5.9 of design) |
| `memory:read` / `memory:write` | `memory.list` / `memory.forget`, `memory.update` | memory writer (§5.4) |
| `knowledge:read` | `kb.search`, `kb.get` | okbase with the requester's Scope |
| `schedule:read` | `schedule.list(from, to)` | reminders, routines, automation schedules |
| `automations:read` | `automations.list`, `automations.runs` | `automation_runs`; admins only |
| `connections:read` | `connections.list` | sealed credential metadata, never tokens |
| `mcp:<server>/<tool>` | `mcp.call` | the gateway (ADR 0011): same `roles`, `rooms`, `deny_tools`, person credentials; `confirm` becomes a shell dialog that the person clicks, which is a real approval |

Errors are typed (`forbidden`, `not_found`, `invalid`, `conflict`, `rate_limited`) and never echo
server internals. Responses are paginated with opaque cursors. v1 only grows by adding methods and
optional fields; anything else is v2, served beside v1.

### 3.4 Per-bot configuration

```toml
[apps]
enabled = true                       # needs [http] public_url (https)
registry = "https://raw.githubusercontent.com/tidusvn05/okbot-apps/main/index.json"
session_idle = "12h"                 # default "30d" for profile = "personal"
session_max = "7d"                   # default "30d" for profile = "personal"

[apps.automations]                   # per-app rules, like [[mcp]]
roles = ["admins"]

[apps.shopping-list]
rooms = ["zalo:family/123456"]       # only from these rooms (direct chats always for personal apps)
permissions = ["me", "tasks:read"]   # optional: grant less than the manifest asks for
```

Apps are data in the bot directory, so `okbot app add` needs no restart: the running bot watches
`apps/` like the bundle and serves the new app within seconds.

---

## 4. Chat entry points

### 4.1 Summary first

Commands that list things get a fixed shape: a header with the total, the first N items (default
10), and an app button when the app is installed and enabled for that room.

```
📋 Tasks — 10 of 34 open
1. File the report — Fri 17:00
2. Call the bank — today 15:00 (reminder)
…
[Open Tasks]   [Next 10]
```

`[Next 10]` is an ordinary command button (ADR 0017) for platforms or rooms where apps are off, so
nothing is lost without a public URL.

### 4.2 Per platform

`ChannelCaps.ui` gains `verified_clicks: bool` (the platform reports who pressed) and
`private_reply: bool` (it can answer only the presser).

| Platform | "Open" | Ticket delivered |
|---|---|---|
| Discord | callback button | ephemeral follow-up with a link button |
| Slack | callback button | `chat.postEphemeral` with a link button |
| Google Chat | card button | private message (`privateMessageViewer`) in spaces; normal reply in DMs |
| Telegram | `web_app` button (private chat), `login_url` button (groups) | none needed: signed `initData` / login data (§2.6) |
| Zalo | link in the text | direct chat only |
| Console | URL printed | the console is the owner, `direct` |

### 4.3 From views and the agent

The view format (ADR 0015, 0017) gains one button kind:

```json
{ "label": "Open Tasks", "app": "tasks", "view": "/open", "params": { "tag": "work" } }
```

- A `[[command]]` result, a notification and the agent's `show` may all use it. The agent names an
  installed app and a declared view; it never sees or writes a URL or a ticket. The runtime checks
  the app is enabled for the room and the params against the manifest, and refuses the view
  otherwise (the agent can fix it, as with other `show` errors).
- Like every view button, pressing it mints a ticket **for the presser**, so a button the requester
  received in a group works for each member with their own rights.
- Notifications (`POST /notify`) may use it; the route's token holder cannot gain anything from it,
  because the ticket belongs to whoever presses.

---

## 5. Built-in apps

Ordered by value; each reuses an existing command or tool, so behaviour stays identical in chat and
in the app.

| App | Shows | Actions | Who |
|---|---|---|---|
| **tasks** | all to-dos, reminders, routines; filters (kind, tag, due, owner folder: me / this room) | done, snooze, edit, pause/resume routine, add | everyone (own and room tasks) |
| **schedule** | calendar and agenda of reminders, routine firings, automation schedules; next 30 days | jump to the task; snooze | everyone; automation rows for admins |
| **memory** | what the bot remembers about me / this room / global | forget, edit | everyone; global edits for admins (D2) |
| **notes** | journal by day, search | add a note | everyone |
| **automations** | each `[[automation]]`: trigger, last runs, items, errors, runs today vs. cap | none (automations come from config, ADR 0013) | admins |
| **connections** | my connected accounts per `[[mcp]]` server, expiry | disconnect, `/connect` instructions | everyone, direct context only |
| **knowledge** | browse and search the bundle within the Scope, read documents | none in v1 | everyone |

`okbot` admin pages (usage, limits, peers) are a later candidate; they are not apps for chat users
and can reuse the same shell.

---

## 6. Registry (the "marketplace")

### 6.1 The `okbot-apps` repository

Why a separate repository rather than `apps/` inside okbot (the usual practice of Raycast,
Obsidian and Homebrew taps):

- reviews of third-party code do not mix with core changes, and app authors do not need rights
  on the okbot repository; CODEOWNERS per app folder lets an author maintain their own app;
- the signing workflow and its key live in a repository with nothing else to release, so a
  compromise of okbot's release workflow does not sign apps, and the other way round;
- app versions, issues and CI (manifest lint, sandbox checks, screenshots) move at their own pace;
- cloning or vendoring okbot does not pull every community app.

Built-in apps stay in `crates/okbot-apps/builtin/`, because they ship and are tested with the binary.
Moving the registry later is one URL in `[apps] registry`.

```
okbot-apps/
├── index.json          # generated by CI, signed
├── index.json.sig      # Ed25519 over index.json
├── CODEOWNERS          # an author may own their app's folder
└── apps/shopping-list/
    ├── app.toml
    └── …
```

```json
{
  "format": 1,
  "generated": "2026-12-01T10:00:00Z",
  "apps": [{
    "name": "shopping-list",
    "version": "0.3.0",
    "api": 1,
    "okbot": ">=0.15",
    "commit": "9f2c…",
    "permissions": ["me", "tasks:read", "tasks:write"],
    "files": { "app.toml": "sha256-…", "index.html": "sha256-…", "app.js": "sha256-…" }
  }]
}
```

- A pull request adds or changes an app folder; review covers its permissions and code.
  CI lints manifests, runs the same checks as install, and on merge regenerates `index.json`
  pinned to the merge commit and signs it.
- **Signing key.** One Ed25519 key, generated offline by the maintainer, with an offline backup.
  The private key lives only in a GitHub Actions *environment* secret of `okbot-apps` that needs a
  maintainer's approval for each run, so a merged pull request alone cannot sign. The binary embeds
  two public keys, the current one and the next, so the key can be rotated without breaking
  installed bots. A version found bad is marked `"yanked": "<reason>"` in the index:
  `okbot app update` and `okbot doctor` report installed yanked apps, and `[apps] refuse_yanked =
  true` (default) stops serving them on the next index fetch.
- `okbot app search|info|add|update|remove` fetch `index.json` and its signature over HTTPS
  (rustls, ADR 0004), verify against the public key embedded in the binary (with room for a second
  key for rotation), then fetch each file from `raw.githubusercontent.com/tidusvn05/okbot-apps/<commit>/apps/<name>/…`
  and verify its hash before writing to `apps/<name>/` atomically (temp dir + rename).
- An app whose `api` the binary does not serve, or whose `okbot` bound is not met, is listed but
  not installable ("needs okbot 0.16").
- `add` and `update` print the permissions (and, on update, what changed) and ask for
  confirmation; `--yes` for scripts. A permission added by an update stays ungranted until accepted.
- No automatic updates: a compromised repository should not reach running bots on its own.
- `[apps] registry` can point at another signed index (a company's own), with its key in
  `[apps] registry_keys`. That is the answer for apps that should not go through okbot's review.

### 6.2 Local apps

```sh
okbot app add ./my-app                       # copy a directory
okbot app add --git https://git.example.com/x/my-app --rev 4b1e…   # a pinned commit
okbot app dev ./my-app                       # serve from the directory, reload on change
```

They are marked *unverified* in `okbot app list` and in the shell header. The sandbox and the
permission grant are the same as for registry apps; the trust difference is only who reviewed it.

---

## 7. Architecture

### 7.1 Crates

| Crate | Adds |
|---|---|
| `okbot-core` | `AppName`, `AppPermission`, `AppManifest` (parsed types), `AppRef` view button kind; `AppSessionStore` port (tickets and sessions); `ChannelCaps.ui.verified_clicks`, `private_reply` |
| `okbot-store` | `app_tickets`, `app_sessions` tables (schema version bump), hashes only |
| **`okbot-apps`** (new) | manifest validation, built-in apps (`include_dir`), installer and registry client (signature and hash checks), static serving with CSP, the shell and SDK, App API router (axum) written against ports: `TaskStore`, memory and notes writers, knowledge, automation run store |
| `okbot-runtime` | ticket minting on presses, `app` view buttons, summary-with-button for list commands, `/app` and `/app logout` commands, the access checks the API calls into |
| `okbot-channel-*` | private replies to a presser; Telegram `web_app` / `login_url` and init data verification |
| `okbot` (cli) | `okbot app …` commands; merges the apps router into the HTTP listener; wiring |

`okbot-apps` depends on `okbot-core` ports only, never on a channel or on `okbot-runtime`;
`scripts/check-deps.py` gets its rule. The runtime asks for app checks through a small trait
(`AppDirectory`: is app X enabled for room R, does view V exist) implemented in `okbot-apps` and
injected by the binary.

### 7.2 HTTP routes (behind `[http] listen`, under `public_url`)

```
GET  /apps/                          shell page
GET  /apps/sdk/v1.js                 bridge
GET  /apps/frame/<app>/<file>        app files (CSP, nosniff, hashed ETag)
POST /apps/api/session               ticket → session
POST /apps/api/telegram              Telegram initData → session
POST /apps/api/logout
POST /apps/api/v1/<method>           App API (cookie + CSRF header + X-Okbot-App)
```

### 7.3 Observability

Spans per API call (`app`, `method`, person, context), audit rows for writes (`source = app:<name>`),
metrics: sessions opened, tickets refused, API errors. Never log tickets, session ids, cookies or
request bodies.

---

## 8. Phases

| Phase | Scope | Release |
|---|---|---|
| **P32 — Sessions and shell** | ADR 0019; tickets, sessions, shell, sandbox, SDK; App API `me`, `tasks.*`; built-in **tasks**; `/tasks` summary + Open button on Discord and console; link fallback on Zalo; `[Next 10]` fallback | 0.14 |
| **P33 — All platforms** | private replies on Slack and Google Chat; Telegram `web_app` and `login_url`; `/app`, `/app logout`, `/app sessions`; `okbot app sessions` | 0.14 |
| **P34 — More built-in apps** | schedule, memory, notes, automations, connections; `app` button in views (commands, notifications, agent `show`) | 0.15 |
| **P35 — Registry** | ADR 0020; `okbot-apps` repository, CI index and signing, `okbot app search/add/update/remove`, permission grants and diffs | 0.15 |
| **P36 — Local apps and MCP** | `okbot app add <dir> / --git`, `okbot app dev`; `knowledge:read`; `mcp.call` with shell confirmation dialogs; developer guide in `docs/extending/` and the `okbot-guide` pack | 0.16 |

Each phase updates the `okbot-guide` pack (settings, commands) as AGENTS.md requires.

Tests: `insta` snapshots of the summary messages and of every App API JSON contract; ticket and
session tests with a fake `Clock` (expiry, reuse, wrong context, revoked access between calls);
CSP headers asserted per route; a registry test with a test key (bad signature, bad hash, wrong
`api`); `check-deps.py` rule for `okbot-apps`.

---

## 9. Risks and later options

| Risk | Mitigation |
|---|---|
| A reviewed registry app turns malicious in an update | explicit updates with diffs, pinned commits, sandbox without network, server-side permission checks |
| Ticket stolen from a chat screenshot or a shared screen | 5-minute single use; fragment-only; the session banner shows whose account it is |
| Operators without a domain | apps disabled, text fallbacks stay; `doctor` explains Caddy / Cloudflare Tunnel (already documented for Google Chat) |
| Browser sandbox quirks (opaque origin and `'self'` in CSP, Safari) | spike in P32 before the SDK is frozen |
| Front-end without a build step gets unwieldy | keep built-in apps small; a shared component file in the SDK |

Later, not in this proposal: declarative apps (a page of ADR 0017 views bound to API queries, no
JS at all); apps with server code in WASM; Discord Activities and Zalo Mini App integration; an
admin console on the same shell.

## 10. Resolved questions (2026-11-26)

1. Registry location: a separate `okbot-apps` repository (A8, §6.1).
2. Signing: one maintainer-held key in a protected environment, three trust tiers (A9, §6.1).
3. Remember this device: 30-day sessions for personal bots (A10).
4. Zalo: a link in the direct chat first; a Zalo Mini App later (A11).

## 11. Implementation notes (2026-11-26)

What the code does where it differs from the sections above:

- **Store.** Tickets and sessions are `ConversationStore` methods (`put_app_ticket`,
  `take_app_ticket`, `put_app_session`, …) like views, not a separate port.
- **Capabilities.** Only `UiCaps.private_reply` was added. Every platform with buttons reports
  the presser, so `verified_clicks` would always equal `buttons`. Discord, Slack, Google Chat and
  Telegram set it.
- **Telegram** answers a press with the link in the presser's direct chat and an alert that says
  so. If the presser never started the bot, the alert asks them to press Start. `web_app` and
  `login_url` are left for later.
- **`[Next 10]`** is not a button: built-in commands cannot be view buttons (ADR 0017). The
  summary says `/tasks all` instead.
- **Contexts** live in the session (up to 8). The shell sends the chosen one in
  `x-okbot-context`, and each call checks it again.
- **The agent** names apps in `show` through `apps: [{app, view, params, label}]` (at most 2),
  and is told the apps it may link in the `<interactive>` section. Programs write
  `{"label", "app", "view", "params"}` among their view's `buttons`.
- **`knowledge:read`** and the knowledge app shipped with P34. They needed only
  `KnowledgeProvider::documents` and `document`, which the notes app uses too.
- **`mcp.call`** answers `{"confirm": "..."}` for a guarded tool. The shell removes any
  `confirmed` an app sends, asks in its own dialog and calls again.
- **Signing key.** `OFFICIAL_KEYS` is empty until the maintainer runs `okbot app keygen`.
  `templates/okbot-apps/` holds the registry repository's README, CODEOWNERS, CI workflow and a
  sample app.
- **Browsers.** `scripts/e2e-apps.py` opens a launch link from `okbot chat` in Playwright: the
  sign-in from the fragment, every built-in app in the sandboxed frame without a CSP or script
  error, a write through the SDK bridge, the shell's confirm dialog and a used ticket being
  refused. CI (`apps-browser`) runs it in Chromium, Firefox and WebKit, and keeps desktop and
  phone screenshots. Mobile in-app browsers (Zalo, Telegram, Discord) are still checked by hand.
- **Audit.** Every write through the App API is a `tool_calls` row: server `bot` (a reserved
  `[[mcp]]` name), tool the method, command `app:<name>`. `mcp.call` records the real server.
