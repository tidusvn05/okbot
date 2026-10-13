# okbot — contract for coding agents

okbot runs chat bots (Zalo, Google Chat, Discord, Slack, console) that answer with an agent CLI (Claude Code or Codex) and an
okbase knowledge bundle. One bot = one directory with `okbot.toml`, `.env` (secrets), `kb/` and
`state/`. Several bots can live on one machine; `okbot list` shows them.

## Set up a bot for a user

1. `okbot new <name> [dir] --json` (or `okbot init --json` inside the directory).
2. Exit code 3 means an answer is missing. The JSON has `error.flag` and `error.message`: ask the
   user the message verbatim, then re-run with that flag. Never invent secrets.
   - `--agent claude,codex`: which agent CLIs, in fallback order (only logged-in ones).
   - `--platform zalo|gchat|discord|slack|none`.
   - `--zalo-token-env NAME`: a variable that already holds the Zalo bot token. Otherwise the
     user must run the wizard in a terminal to paste the token (it goes to `.env`, never argv).
   - `--delivery polling|webhook`; webhook also needs `--public-url https://…`.
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

## One person on several platforms

Personal memory is per account unless the operator links accounts:
`okbot people link <person> <platform:instance/user-id>…` (each account's id comes from `/whoami`
in that chat). Linked accounts share `kb/memory/people/<person>/`. Also `people list|unlink|remove`.
Only link accounts the user confirms are theirs: linked accounts see each other's notes.

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
