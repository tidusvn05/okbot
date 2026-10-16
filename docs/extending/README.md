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

Every port has a conformance check in `okbot-testkit` where its contract can be tested generically:

- `conformance::channel` / `conformance::channel_stops` for `Channel`;
- `conformance::conversation_store` for `ConversationStore`.

`scripts/check-deps.py` enforces the dependency rules; `scripts/verify.sh` runs every gate CI runs.
