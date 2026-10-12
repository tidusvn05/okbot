# okbot

**A multi-platform chat assistant that thinks with the agent CLI you already use and knows what your
[okbase](https://github.com/tidusvn05/okbase) knowledge base knows.**

> Status: **pre-alpha**. Nothing here is usable yet; follow [`docs/implementation-plan.md`](docs/implementation-plan.md).

- **Many platforms, one bot:** Zalo Bot, Google Chat, Discord, Slack and a terminal console, each an
  adapter behind one `Channel` trait.
- **Agent CLIs as the brain:** Claude Code, Codex, OpenCode or Devin through
  [agent-core](https://github.com/tidusvn05/agent-core), with the CLI's own login: no API key needed.
- **Knowledge and memory in markdown:** an okbase (OKF) bundle per bot, read through scoped MCP tools,
  so one user never sees another user's memory.
- **Safe by default:** the agent gets web tools and okbot's tools, never a shell or your files.
- **Many bots per machine:** one directory per bot, managed by the `okbot` command.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/tidusvn05/okbot/main/install.sh | sh
okbot new shop          # wizard: agent CLI, Zalo token, default knowledge
okbot doctor shop
okbot start shop        # runs as a service; `okbot logs shop -f`
```

Coding agents: `okbot help-agent` prints the setup contract.

## Documentation

- [Design](docs/design.md): architecture and the reasons behind it
- [Implementation plan](docs/implementation-plan.md): phases, releases and engineering standards
- [Running on a VPS](docs/deploy.md): many bots per machine, services, reverse proxy
- Platforms: [Zalo Bot](docs/platforms/zalo.md)
- [Architecture decisions](docs/adr/)
- [Releasing](docs/releasing.md)

## License

MIT OR Apache-2.0, at your option.
