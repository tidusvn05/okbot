# Extending okbot

okbot is built from small crates that meet at the ports in `okbot-core` (`docs/adr/0001`). Adapters never
depend on each other; `okbot` is the only place that turns configuration into objects.

| To add | Implement | Register | Guide |
|---|---|---|---|
| A chat platform (Rust) | `Channel` in a new `okbot-channel-<name>` crate | feature + config section + wizard step in `okbot`, `docs/platforms/<name>.md` | [channel.md](channel.md) |
| A chat platform (any language) | the bridge protocol | `[channels.bridge.<instance>]` | [../bridge.md](../bridge.md) |
| A Markdown dialect | a `MarkdownDialect` variant + renderer in `okbot-render` | used through `ChannelCaps::dialect` | [channel.md](channel.md#formatting) |
| An agent backend | `AgentBackend` | `okbot-agent` chain builder | rustdoc of `okbot_core::AgentBackend` |
| A store | `ConversationStore` | `okbot` | `okbot_testkit::conformance::conversation_store` |
| A chat command | `okbot-runtime/src/command.rs` | command table | — |
| A bot tool for the agent | `okbot-knowledge/src/bot_mcp.rs` | gated by `ToolPolicy` | — |
| Your own tools for the agent | an MCP server (streamable HTTP, bearer token) | `[[mcp]]` in `okbot.toml` | [below](#your-own-mcp-server) |

Every port has a conformance check in `okbot-testkit` where its contract can be tested generically:

- `conformance::channel` / `conformance::channel_stops` for `Channel`;
- `conformance::conversation_store` for `ConversationStore`.

`scripts/check-deps.py` enforces the dependency rules; `scripts/verify.sh` runs every gate CI runs.

## Your own MCP server

The agent can call tools of MCP servers you run, next to okbot's own `kb` and `bot`. Declare each one:

```toml
[[mcp]]
name = "ops"                          # tools appear as mcp__ops__<tool>
url = "http://127.0.0.1:8790/mcp"     # streamable HTTP
token_env = "OPS_MCP_TOKEN"           # bearer token, from the environment or .env
admins_only = true                    # offer it only in turns of [bot] admins (default false)
```

- Only agents that take per-run MCP servers (Claude, Codex) get it; it works with `tool_policy = "mcp-only"`, which
  is the setting to use for a bot whose job is your tools.
- The token reaches the agent process only as that one variable; it is kept out of logs and out of every other run's
  environment. The server sees the agent, not the chat user: decisions that need a person (approvals, secrets)
  belong in the server's own channel, not in the agent's arguments.
- Every tool of the server is allowed. Keep dangerous operations behind confirmation in the server.
