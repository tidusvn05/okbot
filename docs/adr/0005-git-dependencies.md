# 0005 — agent-core and okbase as git dependencies

Status: Accepted (2026-10-12)

## Context
okbot builds on [agent-core](https://github.com/tidusvn05/agent-core) and
[okbase](https://github.com/tidusvn05/okbase). Neither is on crates.io yet, and the crate name
`agent-core` on crates.io belongs to an unrelated project.

## Decision
- Depend on both by git **tag** in `[workspace.dependencies]`, or, while an upstream pull request
  okbot needs is under review, by its exact commit (`rev`), never by a moving branch. `deny.toml`
  allows exactly these two git sources. Every git dependency also states `version`, which keeps
  cargo-deny's wildcard check meaningful.
- Distribute okbot as release binaries and a container image until both dependencies are published.
- Ask agent-core to choose a publishable crate name before okbot 1.0 ([agent-core#6](https://github.com/tidusvn05/agent-core/issues/6)).

## Amendment (2026-10-13)
The Zalo Bot API client moved out of `okbot-channel-zalo` into the standalone, unofficial
[`zalo-bot`](https://github.com/tidusvn05/zalo-bot) crate. It was pinned by commit until `zalo-bot` 0.1.0
reached crates.io; okbot now depends on the registry version.
okbot uses it with `default-features = false` and installs the ring provider, so no second crypto
provider is linked.

## Amendment (2026-10-13, agent-call)
agent-core was renamed to [agent-call](https://github.com/tidusvn05/agent-call) and published to
crates.io (0.4.1); okbot depends on the registry version and `deny.toml` no longer allows its git
source. okbase remains a git dependency.

## Amendment (2026-10-14, okbase)
okbase 0.2.3 (with `okbase-mcp`) is on crates.io; okbot depends on the registry versions and
`deny.toml` allows no git source. This decision is now fully superseded: every dependency comes from
crates.io.

## Consequences
okbot crates cannot be published to crates.io until the dependencies are; third-party adapters
depend on `okbot-core` by git tag in the meantime.
