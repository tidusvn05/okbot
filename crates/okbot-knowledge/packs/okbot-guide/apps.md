---
type: Guide
title: Apps
description: Web pages the bot serves behind chat summaries (tasks and more), opened with a button and signed in as the person who pressed it.
tags: [onboarding]
lang: en
status: stable
updated: 2026-11-27
aliases: [apps, app, miniapp, mini app, ứng dụng, xem thêm, see more, /app, Open Tasks, web page, trang web, đăng nhập, sign in, session]
generated: {by: okbot-guide-pack, at: "2026-11-26"}
---

# Apps

A chat message is a poor place for a long list. `/tasks` shows the ten most urgent tasks and an
**Open Tasks** button. The button opens the **Tasks app**, a page the bot serves itself. The page
is already signed in as the person who pressed the button, and it shows only what that person may
see from the chat they pressed it in. Apps never run the agent, so they cost no agent quota.

Built-in apps:

| App | What it shows | Chat |
| --- | --- | --- |
| `tasks` | every to-do, reminder and routine: search, filter by kind, by place (only mine, this room) or by who made it, sort by what comes next; add, edit (title, notes, due, reminder time, repeat), done, snooze (1 h, 3 h, tomorrow 9:00), pause, resume, cancel | `/tasks`, `/app tasks` |
| `schedule` | reminders and routine runs of the next 7 or 30 days, repeats included, as an agenda or a calendar; add a reminder (once, daily, weekdays, weekly, monthly), change its time, snooze, pause; scheduled automations for administrators | `/app schedule` |
| `memory` | what the bot remembers about you, this room and everyone; forget a note | `/app memory` |
| `notes` | the journal by day: search, read, add a line | `/app notes` |
| `knowledge` | the documents the bot answers from, as far as you may see them | `/app knowledge` |
| `automations` | each `[[automation]]`, its trigger and latest runs and errors (administrators only) | `/app automations` |
| `connections` | your connected accounts (`/connect`), never their tokens; from a direct chat only | `/app connections` |
| `artifacts` | pages, documents, tables and diagrams the assistant made (see Artifacts below): view, versions, download, rename, delete | the Open button under an answer, `/app artifacts [id]` |

An app built on something the bot does not keep (no knowledge base, no tasks) is not offered.

`/app` lists the apps a person may open here. `/app <name>` opens one, and `/app <name> <view>`
opens one of its pages (`/app tasks all`).

## What they need

Apps need the bot's public HTTPS address, the one OAuth callbacks and Google Chat use:

```toml
[http]
listen = "127.0.0.1:8787"
public_url = "https://bot.example.com"   # a reverse proxy forwards it to listen
```

Forward the whole address to `listen`, or at least `/apps/`. Without `public_url` the buttons are
not shown, and `/tasks all` still lists everything in chat. `okbot chat` opens apps on this
machine's loopback port, so you can try them without a public address.

## How a press signs someone in

1. The person presses **Open Tasks**. The platform tells the bot who pressed it.
2. The bot checks that person as if they had written a message there: blocked or allowed
   accounts, rooms, the owner of a personal bot, and the app's own `roles` and `rooms`.
3. The bot makes a link for that person only, valid **once** and for **5 minutes**, and shows it
   only to them:

| Platform | What the presser sees |
| --- | --- |
| Discord | a private ("only you can see this") reply with an **Open ↗** button |
| Slack | an ephemeral message ("only visible to you") with an **Open ↗** button |
| Google Chat | a private message in the space (only the presser sees it); a normal reply in a direct chat |
| Telegram | the link in the presser's direct chat with the bot, and a notice saying so. Someone who never started a chat with the bot is asked to press Start first |
| Zalo, and platforms without private replies | the link in a direct chat only; in a group the bot says to send `/app tasks` in a direct chat |
| Console (`okbot chat`) | the link, on the loopback port |

4. The page swaps the link for a session cookie and opens the app. Later presses in the same
   browser reuse the session.

Everyone in a group can press the same button, and each person gets their own link with their own
rights. A link that is forwarded or seen on a screen stops working after one use or 5 minutes.
Chat platforms that preview links never see the secret part of the link: browsers do not send the
part after `#` to any server.

**Personal data needs a direct chat.** Opened from a group, the app shows the room's tasks and
adds tasks to the room. Opened from a direct chat, it also shows the person's own tasks. When
someone has opened it from both, the page has a switcher.

## Buttons in other messages

A `[[command]]` view, a notification and the agent's answers can carry an app button too. A
program writes it among its view's `buttons`, with `app` (and optionally `view` and `params`)
in place of `command`:

```json
{"text": "3 orders wait", "buttons": [{"label": "Open the board", "app": "tasks", "view": "/open"}]}
```

The app and its view must be installed here. Otherwise the view is refused (a notification gets
a 400 that names the apps), and without a public address there are no app buttons at all. With
`reply_mode = "interactive"` the agent is told which apps it may link and when each helps (the
`use_when` of its manifest), and when a list is too long for chat it summarises it and adds the
button instead. A long task or memory listing reminds it at that moment. With more than 8 apps,
the agent looks the others up with the `apps` tool. The agent names an app and a view, and
never writes a link: every presser gets their own link with their own rights.

## Sessions

A session ends after **12 hours unused** or **7 days** in all. On personal bots it lasts
**30 days**, since only the owner can sign in.

```toml
[apps]
enabled = true            # false: no apps and no Open buttons
session_idle = "12h"      # 30m, 12h, 7d; at most 30d
session_max = "7d"
```

A session stores who signed in and from which chat, never what they may do. Every call the page
makes is checked again against the lists as they are now. Remove someone from `allow_users`, block
them or take a role away, and their next click fails without waiting for the session to end.

- `/app sessions` lists your signed-in browsers, and `/app logout` signs all of them out.
- `okbot app sessions` lists everyone's. `--revoke person:<id>` (or an account) signs one person
  out, and `--all` signs out everyone.

## Limiting an app

```toml
[apps.tasks]
roles = ["ops"]                          # only these roles (admins and everyone are built in)
rooms = ["discord:main/123456789012345"] # only from these rooms; direct chats always work
permissions = ["me", "tasks:read"]       # read only: grant less than the app asks for
# disabled = true                        # turn it off
```

`okbot app list` shows every app with its version, where it comes from and what it may do.
`okbot app info tasks` describes one app and the pages chat can open.

## What an app can and cannot do

An app's page runs in a sandbox inside okbot's own page. It has no cookies and cannot reach the
network, and it cannot change the page around it. Everything it reads or changes goes through
okbot, which checks the app's permissions and the person's rights on every call:

| Permission | Allows |
| --- | --- |
| `me` | who is signed in and from where |
| `tasks:read` | listing and reading tasks |
| `tasks:write` | adding, changing, finishing, snoozing, pausing and cancelling tasks |
| `schedule:read` | reminders, routine runs and (administrators) automation times |
| `memory:read`, `memory:write` | listing memory, forgetting a note |
| `notes:read`, `notes:write` | reading the journal, adding a line |
| `knowledge:read` | searching and reading the knowledge base |
| `automations:read` | automations and their runs (administrators) |
| `connections:read` | which accounts are connected (never tokens) |
| `artifacts:read`, `artifacts:write` | listing, opening and downloading artifacts; renaming and deleting them |
| `mcp:<server>/<tool>` | calling that `[[mcp]]` tool as the person, under the server's rules; `confirm` asks the person in okbot's own dialog |

## More apps

Apps are added by the operator, from the terminal. Chat users and the agent cannot add one.

```sh
okbot app search                 # the okbot-apps registry, its index checked against its signature
okbot app add shopping-list      # shows what it may do and asks first (--yes for scripts)
okbot app update                 # newer versions; new permissions are asked for; yanked ones stop being served
okbot app update --allow-downgrade   # also install an older version the registry now offers
okbot app remove shopping-list
okbot app add ./my-app           # your own folder: marked unverified
okbot app add --git https://git.example.com/me/apps --rev 4b1e2c9 --path my-app
okbot app dev ./my-app           # while writing one: copied in again on every change
```

The running bot picks the change up within seconds, with no restart. Registry apps are reviewed in
the `okbot-apps` repository. Their index pins every file to a commit with its SHA-256 and is
signed with Ed25519, and okbot checks both before writing anything. Nothing updates on its own.

```toml
[apps]
registry = "https://apps.example.com/index.json"   # another signed index (default: okbot's)
registry_keys = ["ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="]
refuse_yanked = true            # stop serving versions the registry yanked (default)
```

Writing an app: `docs/extending/apps.md` in the okbot repository.

Routines run the agent unattended, so apps cannot create them or change their prompt. Only an
administrator can start one, from the app as from `/resume`.

Every change an app makes (adding or finishing a task, a note, forgetting a memory, an `mcp.call`)
is in `okbot audit`, with the app as the source (`app:tasks`).

## Artifacts

When an answer is a page, a document, a table or a diagram worth keeping, the agent saves it as an
**artifact** instead of pasting it: HTML, Markdown, SVG, Mermaid, CSV, JSON or text. Asking for a
change ("make the table wider") saves a new version of the same artifact. Artifacts are files in
the knowledge bundle (`kb/artifacts/`), kept with the same rules as tasks: personal ones only from
a direct chat, the others shared with the chat they were made in. Only their titles and summaries
are searched, never their content.

The answer that saved one gets an **Open** button for it. It opens the built-in **Artifacts** app
(`/app artifacts`, or `/app artifacts a-7k2m9q4x2c` for one), which lists them with search and
filters and shows each one: Markdown, tables (CSV, sortable) and JSON in the app; HTML pages, SVG and
Mermaid diagrams in a viewer of their own. The viewer has no network and no access to the app or
the session, and a page that tries to open another page is closed. A banner says the page was
made by the assistant: never type a password into one. From the app people also switch between
versions, download, rename and delete.

```toml
[artifacts]
enabled = true          # false: the agent cannot save artifacts
max_bytes = "1MB"       # one version
quota_person = "50MB"   # one person's, or one chat's
quota_bot = "1GB"       # the whole bot
keep_versions = 10      # older versions' files are removed
scripts = true          # HTML artifacts may run their own scripts, with no network
```

## Troubleshooting

- **No Open button**: check `[http] public_url`, that `[apps] enabled` is not false, and
  `okbot app list`. Platforms without private replies show the button only as a hint to use a
  direct chat.
- **"This link has expired or was already used"**: press the button again or send `/app tasks`.
  Each link works once, within 5 minutes.
- **The page says you are signed out**: the session ended or was revoked. Open it from the chat again.
- **A group is missing from the place switcher**: a group stays open in the browser for 12 hours
  after the last press there. Press an app button in that group again.
- **"Sign in as … ?"**: the link belongs to another account than the one signed in on this browser.
  Choose *Stay* unless you meant to switch: someone may have sent you their link.
- **"… tried to open another page and was closed"**: the app navigated its frame, which okbot does
  not allow. Report it to whoever made the app.
- **"this index is older than one this bot already used"**: the registry served an older signed
  index than before, which may be a replay. Check the registry; deleting `apps/.okbot-index`
  accepts it.
