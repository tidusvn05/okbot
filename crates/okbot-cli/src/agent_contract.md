# okbot — contract for coding agents

okbot runs chat bots (Zalo, Google Chat, Discord, Slack, console) that answer with an agent CLI (Claude Code or Codex) and an
okbase knowledge bundle. One bot = one directory with `okbot.toml`, `.env` (secrets), `kb/` and
`state/`. Several bots can live on one machine; `okbot list` shows them.

## Set up a bot for a user

1. `okbot new <name> [dir] --json` (or `okbot init --json` inside the directory).
2. Exit code 3 means an answer is missing. The JSON has `error.flag` and `error.message`: ask the
   user the message verbatim, then re-run with that flag. Never invent secrets.
   - `--agent claude,codex`: which agent CLIs, in fallback order (only logged-in ones).
   - `--platform zalo|telegram|gchat|discord|slack|none`.
   - `--zalo-token-env NAME`: a variable that already holds the Zalo bot token. Otherwise the
     user must run the wizard in a terminal to paste the token (it goes to `.env`, never argv).
   - `--telegram-token-env NAME`, `--discord-token-env NAME`, `--slack-app-token-env NAME` and
     `--slack-bot-token-env NAME` work the same way for those platforms.
   - `--delivery polling|webhook` (Zalo); webhook also needs `--public-url https://…`. Telegram
     starts with polling; webhook is set in `okbot.toml` (`docs/platforms/telegram.md`).
3. `okbot doctor <name> --json`: exit 0 when ready, 4 when checks failed (each failed check has
   a `detail` telling what to fix).
4. `okbot run <name>` runs the bot in the foreground.

## Add or remove platforms on an existing bot

- `okbot channel list [--bot NAME]`: channels and whether their secrets are set.
- `okbot channel add <zalo|gchat|discord|slack> [--bot NAME] [--instance NAME] -y` with the same
  flags as `okbot new` (`--discord-token-env`, `--slack-app-token-env`, …). Exit code 3 names a
  missing flag, as above. The edit is validated and rolled back when invalid.
- `okbot channel remove <platform:instance> [--purge-secrets]`.
- Restart the bot afterwards (`okbot restart NAME`).

## Manage the agent behind an existing bot

- `okbot agent detect [--bot NAME_OR_PATH] --json`: find known CLIs and check version/login without
  calling a model. Outside a bot directory, without `--bot`, checks the current user's environment.
- `okbot agent status [--bot NAME_OR_PATH] --json`: configured fallback order, model, effort,
  executable, login and tool-policy readiness. Exit 4 means findings; unknown login is not proof
  that a provider can run. Model/effort null means the CLI default, not a detected model name.
- `okbot agent use codex`: promote Codex, keeping other providers as fallbacks. `agent chain
  codex,claude` replaces the order and preserves inactive providers' overrides.
- `okbot agent set [PROVIDER] --model MODEL_ID --effort high`: defaults to the primary provider.
  Effort requires a configured or explicit model and is supported for Claude/Codex. `--effort`
  overrides a model suffix; changing model without an effort keeps the previous effort.
- Clear overrides with `--default-model` (also clears effort), `--default-effort` or `--default-cli`.
  `--cli PATH` records an absolute executable path. Every edit supports `--print` and `--json`.
- Edits save configuration; apply with `okbot restart` or the edit's `--restart`. A failed restart
  leaves the saved configuration in place and returns `saved: true`, `pending_restart: true` and
  `error.code: restart_failed`. Foreground bots need their supervisor restarted manually.
- `okbot agent test [PROVIDER] --json` intentionally spends quota on one small isolated request,
  without fallback or access to bot knowledge. Run it only when the user requests a live test.
  Exit 4 means the probe failed. Detect/status never run this probe automatically.

## Read retained history

`okbot history list|show ID|search TEXT|runs [--bot NAME_OR_PATH] --json` reads the existing SQLite
history without creating or migrating it. `show` includes messages before the latest `/reset` and
marks them `before_reset`; normal agent context still excludes those messages. Missing history
returns an empty `items` array. Records removed by retention are no longer available.

All history reads accept `--channel PLATFORM:INSTANCE`, `--since RFC3339`, `--until RFC3339`,
`--limit 1..1000` (default 50) and `--offset` (default 0). The lower time bound is inclusive; the
upper bound is exclusive. `search` and `runs` also accept `--conversation ID`; `runs --errors`
selects failed/cancelled turns. Search uses literal Unicode lowercase matching, not regex or SQL.
Pages select from the newest end; `show` presents its selected page oldest first. JSON timestamps
are UTC. Each run is the existing turn-level audit record; unknown model/usage stay null.

## Personal assistant

If the bot is only for the user, create it with `--profile personal` (or `okbot profile personal`
later). It then answers only paired accounts, which share one memory on every platform. Run
`okbot pair <bot>` and tell the user to send the printed `/pair <code>` line to the bot in a direct
message on each platform within 10 minutes. `okbot pair <bot> --list` shows paired accounts.

## Tasks and reminders

Users ask the bot in chat ("remind me at 8 tomorrow…"); `/tasks` and `/done <n>` work in chat. From
the terminal: `okbot task list [BOT] [--all]`, `okbot task add "<title>" --bot BOT [--remind
"YYYY-MM-DD HH:MM"] [--due …] [--repeat "daily 08:00"] [--to discord:main] [--person ID]`,
`okbot task done|rm <id> --bot BOT`. Times are local to `[bot] timezone`; set it for correct
reminders. Tasks are files in `kb/tasks/`.

## One person on several platforms (team bots)

Personal memory is per account unless the operator links accounts:
`okbot people link <person> <platform:instance/user-id>…` (each account's id comes from `/whoami`
in that chat). Linked accounts share `kb/memory/people/<person>/`. Also `people list|unlink|remove`.
Only link accounts the user confirms are theirs: linked accounts see each other's notes.

## Your own MCP servers and chat commands

To give the bot the user's own tools, add an `[[mcp]]` server to `okbot.toml`: `name`, then either
`url` and `token_env` (a variable in `.env`; `auth = "signed"` gives the server who asked and where,
and lets it show buttons and forms) or `command = ["npx", "-y", "…"]` with `env = ["VAR"]` for a
local stdio server. The agent reaches it through okbot's gateway, which applies the server's rules:
`roles` and `rooms` (who and where), `tools` / `deny_tools` (patterns with `*`),
`[[mcp.restrict]] tools = […] roles = […]`, and `confirm = "destructive"` (the person approves each
risky call with a button or `/approve <code>`). Roles are defined in `[roles] ops = ["discord:main/1",
"person:lan"]`; `admins` and `everyone` are built in. `okbot role grant ops <account> --for 2h` gives
one for a while, `okbot audit` lists the tool calls, `[limits] turns_per_day` caps each person.
`[[command]]` turns one tool into a chat command that runs without the agent (`name`, `mcp`, `tool`,
`args = ["company", "since?"]`, `with = {…}` fixed arguments, `roles`). `okbot doctor` checks the
config; restart the bot afterwards. Details: `docs/extending/README.md`. Only add servers the user
runs or trusts, and never one that offers a shell or file access; prefer `tools = [...]` with
the tools the bot needs.

To let the user's programs post to chats (alerts, CI, deploys), add a `[[notify]]` target: `name`,
`token_env` (a new variable in `.env`, never a bearer `[[mcp]]` token) and `to` (`room:<key>` from
`/whoami` as an admin, `user:<platform>:<instance>/<id>`, `person:<id>`, `admins`). It needs
`[http] listen`. Callers `POST /notify/<name>` with `Authorization: Bearer <token>` and
`{"text": "…", "id": "…"}`, or run `okbot send <name> "text"` on the bot's machine. Details:
`docs/notify.md`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | error (message on stderr, or `error.message` with `--json`) |
| 3 | the user must provide or decide something (`error.flag`) |
| 4 | findings (`doctor` checks or `kb lint` errors) |

## Rules

- Do not put tokens in `okbot.toml` or on the command line; use `.env` or the environment.
- Do not set `allow_unrestricted_agents = true` unless the user explicitly asks: it lets agents
  use a shell.
- Knowledge lives in `kb/` as okbase (OKF) markdown; `okbot kb lint` checks it.
