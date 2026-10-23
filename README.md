# okbot

[![CI](https://github.com/tidusvn05/okbot/actions/workflows/ci.yml/badge.svg)](https://github.com/tidusvn05/okbot/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV 1.89](https://img.shields.io/badge/rustc-1.89%2B-orange.svg)](rust-toolchain.toml)

**A multi-platform chat assistant that thinks with the agent CLI you already use and knows what your
[okbase](https://github.com/tidusvn05/okbase) knowledge base knows.**

okbot connects Zalo, Telegram, Google Chat, Discord and Slack (and any other platform, through a small bridge
protocol) to Claude Code, Codex, OpenCode or Devin. Every bot has its own markdown knowledge base and
memory. The agent behind it can search the web and read that knowledge, but never run a shell or
touch your files.

> **Status: alpha (v0.6.0).** Every feature below is tested against fake platform APIs and real agent
> CLIs, and runs daily on real Zalo and Discord bots; Telegram, Google Chat and Slack have not been
> tried on real accounts yet. Expect breaking changes until v1.0.

## Features

- **Many platforms, one bot.** Zalo Bot and Telegram (polling or webhook), Google Chat (HTTP or
  Pub/Sub), Discord, Slack (Socket Mode) and a terminal console. Adapters in any language via the
  [bridge protocol](docs/bridge.md).
- **Agent CLIs as the brain.** Claude Code, Codex, OpenCode or Devin through
  [agent-call](https://github.com/tidusvn05/agent-call), using the CLI's own login: no API key. Fallback
  chains, session resume, live "thinking…" progress where the platform can edit messages.
- **Knowledge and memory in markdown.** One [okbase](https://github.com/tidusvn05/okbase) (OKF) bundle
  per bot: default knowledge, your documents, and memory the bot writes ("remember that…"), versioned
  with git. Answers cite their sources.
- **Private by construction.** Each run sees only what its user and room may see: one user can never
  read another user's memory, even through prompt injection.
- **Safe by default.** Web search and fetch through the CLI's built-in tools plus okbot's MCP tools;
  never a shell or file access. Rooms with sensitive knowledge can turn the web off (`mcp-only`).
- **Your own tools.** Connect your MCP servers (`[[mcp]]`, over HTTP or stdio): with signed run identity
  they know who asked and where, and can ask that person directly with buttons and forms (approvals,
  one-time codes) that never pass through the agent. `[[command]]` turns a tool into a chat command
  (`/logs acme 24h`) that answers without the agent, with platform command menus. See
  [docs/extending](docs/extending/README.md).
- **Notifications from your programs.** `[[notify]]` gives a monitor, CI job or service one HTTP call
  (`POST /notify/alerts`, or `okbot send alerts "…"`) to reach the rooms and people you list, on every platform,
  rendered for each, without an agent. See [docs/notify.md](docs/notify.md).
- **Gmail, Drive, Sheets, Notion and more.** Point `[[mcp]] auth = "oauth"` at a service's MCP server
  (`okbot mcp add notion | google-workspace | linear`); each person connects their own account with
  `/connect`, used only in their own chats, and okbot keeps the tokens sealed. See
  [docs/services.md](docs/services.md).
- **Automations.** `[[automation]]` polls a tool, runs on a schedule or takes a webhook, and acts on
  what is new: a templated message without an agent ("📧 new mail from…"), or an agent turn as you
  ("summarise it") that runs without web tools and asks before any change. See
  [docs/automations.md](docs/automations.md).
- **Permissions you can read.** The agent reaches your servers only through okbot's gateway: roles
  (`[roles]`, temporary grants with `/grant`), rooms and tool allow-lists decide who may call what,
  risky calls wait for the person's **Allow**, every call is in `okbot audit`, and `[limits]` caps each
  person's daily turns and tokens.
- **Personal or team.** A `personal` bot answers only you, on every platform you pair with
  `okbot pair`, with one shared memory; a `team` bot keeps one memory per person.
- **Many bots per machine.** A bot is a directory; `okbot` creates, checks, runs and supervises them
  (systemd), with a machine-wide concurrency limit.
- **Operable.** `okbot doctor`, `/healthz`, Prometheus `/metrics`, structured logs with secrets redacted.

## How it works

```
 Zalo   Telegram   Google Chat   Discord   Slack   bridge adapters
   └────────┴────────────┼──────────┴────────┴────────────┘
                  channel adapters ── per-platform rendering (Markdown dialects, splitting)
                         │
                  okbot runtime ── access rules, dedupe, per-conversation queue, history (SQLite)
                         │
           ┌─────────────┴──────────────┐
   agent CLI (agent-call)          okbase bundle (kb/)
   claude · codex · …      ◄────── MCP: kb search/read, memory, attachment_view
   web tools, no shell             scoped to the current user and room
```

Read [docs/design.md](docs/design.md) for the architecture and the reasons behind it.

## Quick start

Requirements: Linux or macOS, and at least one agent CLI installed and logged in
(`claude`, `codex`, `opencode` or `devin`).

```sh
# a prebuilt binary (Linux, macOS) from the latest GitHub release, checksum verified
curl -fsSL https://raw.githubusercontent.com/tidusvn05/okbot/main/install.sh | sh
# or from crates.io (compiles, Rust 1.89+)
cargo install --locked okbot

okbot new shop          # wizard: picks the agent CLI, asks for the platform token, writes kb/
cd shop
okbot chat              # try the bot in the terminal
okbot doctor            # check agent, knowledge and platform credentials
okbot start shop        # run as a service; `okbot logs shop -f` to follow
```

Each release also ships a container image, `ghcr.io/tidusvn05/okbot` (see [docs/deploy.md](docs/deploy.md)).

## Manage agents and review history

Inside a bot's directory (or add `--bot NAME_OR_PATH`):

```sh
okbot agent detect                      # installed CLIs, versions and login; no model calls
okbot agent status                      # this bot's chain, model, effort and readiness
okbot agent use codex                   # make Codex primary; keep the other agents as fallbacks
okbot agent set --model MODEL_ID --effort high
okbot agent chain codex,claude          # replace the fallback order; keep provider settings
okbot restart                          # apply the saved configuration

okbot history list                      # conversation ids and recent activity
okbot history show 1                    # retained messages, including before /reset
okbot history search "invoice"          # literal Unicode-lowercase substring search
okbot history runs --errors --json      # failed/cancelled agent-turn audit records
```

Agent edits support `--print` for a validated preview and `--restart` to restart after saving.
`okbot agent test` intentionally makes one small model call and consumes quota; detection and status
do not. History reads the existing database without creating or migrating it. Records remain subject
to the bot's retention setting (90 days by default).
See [agents and history](docs/agents-and-history.md) for clearing overrides, filters and JSON output.
Coding agents can run `okbot help-agent` for the non-interactive setup contract.

A bot directory looks like this:

```
shop/
├── okbot.toml   # configuration (no secrets)
├── .env         # tokens, mode 0600, read only by okbot
├── kb/          # okbase bundle: persona, system guide, your knowledge, memory
└── state/       # SQLite history, agent work directories
```

## Platforms and agents

| Platform | Delivery | Guide |
|---|---|---|
| Zalo Bot | long polling or webhook | [docs/platforms/zalo.md](docs/platforms/zalo.md) |
| Telegram | long polling or webhook | [docs/platforms/telegram.md](docs/platforms/telegram.md) |
| Google Chat | HTTP endpoint or Pub/Sub | [docs/platforms/google-chat.md](docs/platforms/google-chat.md) |
| Discord | gateway | [docs/platforms/discord.md](docs/platforms/discord.md) |
| Slack | Socket Mode | [docs/platforms/slack.md](docs/platforms/slack.md) |
| Console (`okbot chat`) | terminal, imitates the others with `--like` | [docs/platforms/console.md](docs/platforms/console.md) |
| Anything else | bridge adapter (any language) | [docs/bridge.md](docs/bridge.md) |

| Agent CLI | Web tools | Tool lock-down |
|---|---|---|
| Claude Code | WebSearch, WebFetch | enforced (`--tools` allow-list) |
| Codex | web_search | enforced (features disabled, read-only sandbox) |
| OpenCode, Devin | built in | not enforceable: refused unless `--unsafe-tools` |

## Documentation

- [Design](docs/design.md) and [architecture decisions](docs/adr/)
- [Running on a VPS](docs/deploy.md): many bots per machine, services, reverse proxy
- [A personal assistant](docs/assistant.md): memory, reminders, routines, notes and saved documents
- [Extending okbot](docs/extending/README.md): new platforms in [Rust](docs/extending/channel.md) or
  [any language](docs/bridge.md), agent backends, stores
- [Implementation plan](docs/implementation-plan.md): phases, releases, engineering standards
- [Releasing](docs/releasing.md)

## Contributing

Contributions are welcome: bug reports, platform adapters, docs. Read
[CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md), and run
`scripts/verify.sh` before opening a pull request. Report security issues privately as described in
[SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in okbot
by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
