# Changelog

All notable changes to okbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and okbot uses
[Semantic Versioning](https://semver.org/) (see docs/releasing.md). This file is generated from commit messages by git-cliff.

## [0.5.0] - 2026-10-16

### Features

- **core:** domain model and ports (P1) (a5215fd)
- **render:** Markdown dialects and length-aware splitting (P2) (2faae4a)
- **store:** SQLite conversation store and testkit conformance suite (P3) (3d987a9)
- **agent:** CLI backends, fallback chain and CLI detection (P4) (5ef04ff)
- **runtime:** message pipeline, console channel and okbot chat (P5) (b98f901)
- **knowledge:** okbase bundle, scoped MCP tools and default knowledge (P6) (b5508a3)
- **agent:** enforce tool policies and use MCP and system prompts (P7) (1a1ad53)
- **zalo:** Zalo Bot channel with polling and webhook (P8b) (ce28526)
- **cli:** bot configuration, setup wizard, run, doctor and registry (P8a) (83faa0a)
- **ops:** services, machine-wide limits and packaging (P8c) (3b7a9dc)
- **memory:** personal, room and team notes with MCP tools and commands (P10) (eedfb81)
- **gchat:** Google Chat channel with Pub/Sub and HTTP delivery (P9) (19e9c57)
- **channels:** Discord and Slack (P11) (d5c00c9)
- **runtime:** session resume and live progress (P12) (6c3ef36)
- image attachments, metrics and fleet status (P13) (c22e2ec)
- bridge protocol, channel template and conformance suite (P14) (eb39430)
- link one person's accounts across platforms; okbot channel and people commands (#10) (c3d8b2c)
- personal profile with pairing codes (okbot profile, okbot pair) (#11) (8117173)
- **discord:** print the install link after okbot channel add; quiet Zalo's 400 on deleteWebhook (#13) (d306e22)
- tasks and reminders for personal assistants (P15–P16) (#16) (3bdb790)
- snooze, routines, notes and saving documents (P17–P19) (#18) (4500c4e)
- Telegram channel, security and privacy fixes for v0.5.0 (#19) (8343397)

### Bug fixes

- **cli:** build without channel features; check every feature in verify.sh (35ac624)
- never wait inside a turn; depend on agent-call 0.4.1 from crates.io (#14) (2f10916)

### Refactoring

- **zalo:** use the zalo-bot crate as the Zalo Bot API client (#9) (14ac7de)

### Documentation

- open-source README, issue and PR templates, dependabot (caba61e)

### Dependencies

- **deps:** Bump actions/download-artifact from 4 to 8 (#1) (c389c33)
- **deps:** Bump docker/login-action from 3 to 4 (#2) (86f3109)
- **deps:** Bump actions/upload-artifact from 4 to 7 (#3) (b548b28)
- **deps:** Bump actions/checkout from 5 to 7 (#4) (aa45150)
- **deps:** Bump the cargo group with 3 updates (#6) (2bff419)
- **deps:** reqwest 0.13, release actions to Node 24, dependabot limits (#7) (f936e83)
- **deps:** okbase v0.2.3 (#8) (04d45d5)
- **deps:** zalo-bot 0.1 from crates.io (#12) (4340d63)
- **deps:** agent-call 0.4.2 (Codex can call okbot's MCP tools) (#15) (b288301)

### Other

- repository foundation (P0) (79e4f52)
- add scripts/verify.sh running every quality gate (2c38dcb)


