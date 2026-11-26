# Writing an app

An app is a folder with an `app.toml` and static files. The bot serves it behind its chat
summaries and opens it signed in as the person who pressed its button (ADR 0019). Nothing in an
app runs on the bot. The page runs in a sandboxed frame in the person's browser, without
cookies or network access, and reaches data only through the **App API**, within the
permissions the operator granted and the person's own rights.

The [`hello-tasks`](../../templates/okbot-apps/apps/hello-tasks) sample is the smallest
complete app. The built-in apps in [`crates/okbot-apps/builtin/apps`](../../crates/okbot-apps/builtin/apps)
are larger examples.

## The folder

```
my-app/
├── app.toml        # the manifest
├── index.html      # the entry page
├── app.js
└── locales/vi.json # any other files
```

Allowed files: `html js mjs css json svg png webp woff2 txt md toml`, at most 2 MB each, 10 MB and
200 files in all, with no links and no hidden files. The folder name is the app name.

```toml
name = "my-app"                 # a-z, 0-9, -; starts with a letter
title = { en = "My app", vi = "App của tôi" }
version = "0.1.0"
api = 1                         # the App API major version
okbot = ">=0.14"                # optional: the lowest okbot it needs
description = { en = "One paragraph." }
license = "MIT"
permissions = ["me", "tasks:read"]

[[view]]                        # pages chat may open: `/app my-app open`
path = "/open"
title = { en = "Open items" }
params = ["kind"]               # query values chat (or a view button) may pass
```

## The page

```html
<link rel="stylesheet" href="../../sdk/ui.css">   <!-- optional: okbot's look -->
<script src="../../sdk/v1.js"></script>
<script src="app.js" defer></script>
```

The frame's policy allows scripts, styles, images and fonts **from the bot only**, with no
inline scripts or `style` attributes, no `fetch` and no form submission anywhere. Put everything
in files and build elements with `okbot.h` or the DOM.

### The SDK

| Call | What |
| --- | --- |
| `okbot.call(method, params)` | an App API method; resolves to its result, rejects with `error.code` and `message` |
| `okbot.me()` | `{name, platform, account, context: {label, direct}, locale, timezone, admin, permissions}` |
| `okbot.confirm(text)` | asks the person in okbot's own dialog; `true` or `false` |
| `okbot.openLink(url)` | opens an `https` link after the person agrees to leave |
| `okbot.setTitle(title)` | the tab title |
| `okbot.view` | `{path, params}` that chat opened |
| `okbot.lang` | `en` or `vi`, chosen by the shell |
| `okbot.words({en: {...}, vi: {...}})` | `t(key, vars)` in that language |
| `okbot.h(tag, props, ...children)` | builds an element; text is never HTML |
| `okbot.when(iso, timeZone)` | a local date and time |

### The App API

| Method | Permission | Notes |
| --- | --- | --- |
| `me` | `me` | |
| `tasks.list` `{status: active\|all\|done, kind, query, cursor, limit}` | `tasks:read` | paged: `{items, total, next}` |
| `tasks.get` `{id}` | `tasks:read` | |
| `tasks.add` `{title, notes, due, remind_at, repeat, kind: todo\|reminder}` | `tasks:write` | routines are made in chat |
| `tasks.update` `{id, title, notes, due, remind_at, repeat}` | `tasks:write` | `null` clears a time |
| `tasks.done`, `tasks.pause`, `tasks.resume`, `tasks.cancel` `{id}` | `tasks:write` | starting a routine needs an administrator |
| `tasks.snooze` `{id, until: 10m\|2h\|15:30\|date-time}` | `tasks:write` | |
| `schedule.list` `{days}` | `schedule:read` | firings, repeats expanded; automations for administrators |
| `memory.list` | `memory:read` | |
| `memory.forget` `{id}` | `memory:write` | |
| `notes.list` `{query}`, `notes.get` `{id}` | `notes:read` | |
| `notes.add` `{text}` | `notes:write` | |
| `knowledge.search` `{query}`, `knowledge.get` `{id}` | `knowledge:read` | within the person's scope |
| `automations.list` | `automations:read` | administrators only |
| `connections.list` | `connections:read` | from a direct chat only; never tokens |
| `mcp.call` `{server, tool, arguments}` | `mcp:<server>/<tool>` | see below |

Methods that change data are recorded in `okbot audit` as `bot.<method>` with the source
`app:<name>`.

Times are RFC 3339 with the bot's offset, or `YYYY-MM-DD HH:MM` in the bot's time zone.

Where the app was opened decides what it sees. Opened from a group, it sees that room's data and
shared data. Opened from a direct chat, it also sees the person's own data. `me().context.direct`
says which.

### Calling your own tools

`mcp.call` calls one tool of a `[[mcp]]` server as the signed-in person. The server's `roles`,
`rooms`, tool lists and the person's own credentials all apply, as for an agent or a
`[[command]]`. The call is audited as `app:<name>`. A tool that `confirm` guards first answers
`{"confirm": "<question>"}`. okbot's shell then asks the person in its own dialog and calls
again, and an app cannot skip that step.

```js
const out = await okbot.call("mcp.call", { server: "ops", tool: "status", arguments: { company: "freach" } });
// out.text, out.structured, out.is_error
```

## Trying it

```sh
okbot app dev ./my-app        # copies it into the bot again on every change
okbot chat                    # then /app my-app: opens on this machine
```

okbot's own apps are checked in real browser engines by `scripts/e2e-apps.py` (Playwright); its
steps are a starting point for testing an app of your own.

`okbot app add ./my-app` installs a copy, and `okbot app add --git <url> --rev <commit> --path apps/my-app`
installs from a pinned commit. Both are marked unverified.

## Publishing

Open a pull request that adds `apps/<name>/` to the
[okbot-apps](../../templates/okbot-apps/README.md) registry. After review and the signed index,
operators install it with `okbot app add <name>`.
