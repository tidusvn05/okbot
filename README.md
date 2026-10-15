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

> **Status: alpha, unreleased.** Every feature below is implemented and tested against fake platform
> APIs and real agent CLIs; real-platform testing and the first release (v0.5.0) are in progress. Expect
> breaking changes until v1.0.

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
