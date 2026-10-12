# 0005 — agent-core and okbase as git dependencies

Status: Accepted (2026-10-12)

## Context
okbot builds on [agent-core](https://github.com/tidusvn05/agent-core) and
[okbase](https://github.com/tidusvn05/okbase). Neither is on crates.io yet, and the crate name
`agent-core` on crates.io belongs to an unrelated project.

## Decision
- Depend on both by git **tag** (never a branch) in `[workspace.dependencies]`; `deny.toml` allows
  exactly these two git sources.
- Distribute okbot as release binaries and a container image until both dependencies are published.
- Ask agent-core to choose a publishable crate name before okbot 1.0 ([agent-core#6](https://github.com/tidusvn05/agent-core/issues/6)).

## Consequences
okbot crates cannot be published to crates.io until the dependencies are; third-party adapters
depend on `okbot-core` by git tag in the meantime.
