# Changelog

All notable changes to okbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and okbot uses
[Semantic Versioning](https://semver.org/) (see docs/releasing.md). This file is generated from commit messages by git-cliff.

## [0.10.1] - 2026-10-25

### Features

- **teams:** manifests valid 365 days by default; okbot team renew (#49) (5d49c14)

### Bug fixes

- **install:** say why a download failed (#47) (c716d8b)
- **peers:** relay from okbot chat; okbot start for team-only bots (#48) (a8c7ff4)
- **agent:** find agent CLIs at each start instead of pinning their path (#50) (f4f0d48)

### Other

- reopen development as 0.11.0-dev; publish up to a new crate (#46) (8bf832a)

## [0.10.0] - 2026-10-24

### Features

- **interact:** a callback reply can show the prompt's next step (b455ef6)
- OAuth for MCP servers and built-in automations (#39) (8c3831d)
- **kb:** knowledge packs and the okbot-guide pack (#40) (4150dfd)
- **peers:** teams of bots that delegate work to each other (#44) (f1a58a1)

### Documentation

- **guide:** list only reviewed MCP servers; SSH and infrastructure page (#42) (cf1579f)

### Dependencies

- **deps:** bump agent-call and okbase; run the msrv CI job on 1.89 (#43) (0f462b7)

### Other

- reopen development as 0.10.0-dev; v0.9.0 release results (#38) (591045e)

## [0.9.0] - 2026-10-21

### Features

- **console:** simulate users, rooms and platforms; scripted agent for tests (#35) (99ef81d)

### Other

- reopen development as 0.9.0-dev; v0.8.0 release results (#34) (f3b051e)

## [0.8.0] - 2026-10-21

### Features

- MCP gateway with roles, rooms, tool lists, approvals, audit, limits and stdio servers (ADR 0011) (#31) (03842bf)
- **cli:** okbot agent and okbot history for operators (#32) (8e1bfa1)

### Other

- reopen development as 0.8.0-dev (#30) (3ee13df)

## [0.7.0] - 2026-10-20

### Features

- [[notify]] notifications from other programs (ADR 0010) (#28) (d09fb6e)

### Documentation

- [[mcp]] and [[command]] in the README and help-agent; v0.6.0 checklist results (#26) (902bd82)

### Dependencies

- **deps:** okbase 0.2.4 (741011b)

### Other

- reopen development as 0.7.0-dev (#25) (66c9e03)

## [0.6.0] - 2026-10-17

### Features

- **config:** [[mcp]] for your own MCP servers (3ca4955)
- signed run identity and prompts for [[mcp]] servers (ADR 0007) (cc03766)
- [[command]] chat commands for MCP tools, with platform menus (ADR 0008) (6a86309)
- **cli:** okbot chat also runs the bot's [[command]]s (1d5f50f)
- the author's locale from platforms that report it, in signed run tokens (138eb0a)
- [[command]] descriptions in several languages (4d6229d)
- **render:** chat line breaks, -# notes and a reply layout convention (481c351)
- **commands:** keep a server's own icon on error results (853dd84)

### Bug fixes

- **cli:** refuse an empty token in the setup wizard (a9f7d2c)
- **discord:** answer when the bot's own role is mentioned (4f1af1a)
- **core:** allow form field ids up to 100 characters (7cce51a)
- **runtime:** name=value before a rest argument; a quoted word is always a value (7900749)
- **discord:** prompts never show link previews (3014ab8)
- **mcp:** fixed `with` arguments, no URLs in errors, only issued tokens on /interact (#23) (f6d1f97)

### Other

- reopen development as 0.6.0-dev (#22) (a60ce83)
- **deny:** ignore zip extract() advisories that okbot cannot reach (3d180d1)

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


