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
token_env = "OPS_MCP_TOKEN"           # shared secret, from the environment or .env
admins_only = true                    # offer it only in turns of [bot] admins (default false)
auth = "signed"                       # bearer (default): the secret itself | signed: a run identity
```

- Only agents that take per-run MCP servers (Claude, Codex) get it; it works with `tool_policy = "mcp-only"`, which
  is the setting to use for a bot whose job is your tools.
- The secret is kept out of logs and out of every other run's environment.
- Every tool of the server is allowed. Keep dangerous operations behind a prompt (below) in the server.

### Signed run identity (`auth = "signed"`)

The agent presents `Authorization: Bearer okbot1.<claims>.<signature>` instead of the secret
([ADR 0007](../adr/0007-signed-identity-and-prompts.md)). Verify it on every request:

1. split at the last `.`: `signed` and `signature`;
2. `signature == base64url(HMAC-SHA256(secret, signed))` (no padding; compare in constant time);
3. `signed` starts with `okbot1.`; the rest is base64url JSON:
   `{"bot", "mcp", "user": "discord:main/1234", "name", "conversation": {...}, "admin", "direct",
   "interact": "http://127.0.0.1:…/interact", "exp", "jti"}`; reject when `exp` (Unix seconds) has passed.

Authorize by `user` and `admin` — the agent cannot change them.

### Prompts: ask the person, not the agent

With the same token, the server can show buttons in the conversation and receive the answer directly:

```http
POST <claims.interact>
Authorization: Bearer <the run token>

{"text": "Deploy **v2** to production?",
 "buttons": [
   {"id": "ok", "label": "Deploy", "style": "danger",
    "form": {"title": "Confirm", "fields": [{"id": "otp", "label": "One-time code", "min_len": 6, "max_len": 6}]}},
   {"id": "cancel", "label": "Cancel", "style": "secondary"}],
 "answer_by": "admins",
 "expires_in": 900,
 "callback": "http://127.0.0.1:8790/answers"}
```

→ `{"prompt": "<id>"}` (`501` on platforms without buttons). The callback must be on the server's own origin.
When someone allowed presses a button (and fills in its form), okbot POSTs

```json
{"prompt": "<id>", "button": "ok", "values": {"otp": "123456"},
 "user": {"id": "discord:main/1234", "name": "Lan", "admin": true}, "conversation": {...}}
```

with `x-okbot-timestamp` and `x-okbot-signature: base64url(HMAC-SHA256(secret, "<timestamp>.<body>"))`. Answer
`{"text": "Deployed ✅"}` to replace the buttons, or add `"keep_open": true` to let them try again (a wrong code).
Values never reach the agent or the history: use forms for API keys and one-time codes. Discord supports prompts
today; buttons, styles and limits follow Discord (5 buttons, 5 fields).
