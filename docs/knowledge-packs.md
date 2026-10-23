# Knowledge packs

A knowledge pack is a ready-made folder of knowledge that ships inside the `okbot` binary and is
copied into a bot's bundle on request:

```sh
okbot kb pack list
okbot kb pack add okbot-guide            # into kb/knowledge/okbot-guide/, linked from knowledge/index.md
```

A pack owns its folder. Running `pack add` again after an upgrade replaces the pack's files with the
ones of the new version, so the pack always describes the binary that installed it; other files are
never touched. The bot re-reads the files within seconds, and `pack add` re-indexes at once.

## `okbot-guide`

How to set up and run okbot and the services around it, written for operators: installing,
personal and team bots, every chat platform, agent CLIs, permissions, the knowledge base, connecting
services through MCP (Google Workspace and a catalog of popular servers, Rust first), your own tools
and commands, automations, notifications, deployment, security and troubleshooting. Each document
carries Vietnamese and English `aliases`, so questions in either language find it.

### A helper bot for okbot itself

Install the pack into any bot, or make a bot whose job is helping people deploy okbot:

```sh
okbot new okbot-help --profile team --platform discord
okbot kb pack add okbot-guide --bot okbot-help
```

Then give it a persona in `kb/persona/assistant.md`, for example:

```markdown
You help people deploy and run okbot and the services that go with it.

- Answer from the okbot guide in `knowledge/okbot-guide/`; cite the page you used.
- Answer in the language the person writes in. Keep okbot settings, commands and file names as
  they are.
- Give the exact `okbot.toml` block or command, then one line on why.
- For services, recommend an MCP server from the guide and show its `[[mcp]]` settings; prefer
  the vendor's hosted server, otherwise a maintained Rust one.
- Never ask for tokens or secrets in chat: they go in `.env`.
- When the guide does not cover something, say so and point to the okbot repository.
```

Use `tool_policy = "web"` so it can also check a service's current documentation.

## Maintaining a pack

Packs live in `crates/okbot-knowledge/packs/<name>/` and are registered in
`crates/okbot-knowledge/src/packs.rs`. A change to a setting, command or behaviour that the guide
describes updates the guide in the same change. `crates/okbot-cli/tests/guide_pack.rs` parses every
TOML example of the guide against the configuration schema and lints the pack at okbase L2, so a
renamed or removed setting fails the build. Tags must be in the default vocabulary (`bot`,
`onboarding`), because the pack is installed into existing bundles.
