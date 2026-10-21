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
| Your own tools for the agent | an MCP server (streamable HTTP, or a stdio program) | `[[mcp]]` in `okbot.toml` | [below](#your-own-mcp-server) |
| Tools that act on each person's own account | a signed MCP server keeping per-person OAuth tokens | `[[mcp]] auth = "signed"` | [accounts.md](accounts.md) |
| A chat command for one of your tools | nothing: config only | `[[command]]` in `okbot.toml` | [below](#commands-for-your-tools) |
| A platform's command menu | `Channel::set_commands` | channel adapter | [below](#commands-for-your-tools) |
| Notifications from your programs | nothing: config only | `[[notify]]` in `okbot.toml` | [../notify.md](../notify.md) |
| Bot-started direct chats | `Channel::open_direct` | channel adapter | [channel.md](channel.md) |

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
auth = "signed"                       # bearer (default): the secret itself | signed: a run identity
```

- Only agents that take per-run MCP servers (Claude, Codex) get it; it works with `tool_policy = "mcp-only"`, which
  is the setting to use for a bot whose job is your tools.
- **Through a gateway** ([ADR 0011](../adr/0011-mcp-gateway-and-roles.md)). The agent talks to okbot on a random
  loopback port with a token that ends with its turn; okbot forwards to your server with the secret (or the signed
  run token), so the agent never holds either. Every rule below is applied there, and every call is recorded.
- The secret is kept out of logs and out of every other run's environment.
- Never add a server that offers a shell or file access: the agent would get them, which okbot otherwise never
  allows. Use `https://` for a server on another host.

### Who, where and which tools

```toml
[roles]                               # names for groups of people
ops = ["discord:main/1234", "person:lan"]   # accounts as /whoami shows them, or [people] ids
support = []                          # filled by temporary grants only

[[mcp]]
name = "ops"
url = "http://127.0.0.1:8790/mcp"
token_env = "OPS_MCP_TOKEN"
roles = ["ops", "support"]            # who may use the server (default: everyone the bot answers)
rooms = ["discord:main/987654"]       # where it is offered (default: everywhere)
tools = ["status", "logs_*", "deploy*"]   # offered tools, `*` matches anything (default: all)
deny_tools = ["logs_purge"]           # never offered; wins over `tools`
confirm = "destructive"               # never (default) | destructive | always
confirm_by = "requester"              # requester (default) | admins

[[mcp.restrict]]                      # these tools only for these roles
tools = ["deploy*"]
roles = ["ops"]
```

- `admins` (the `[bot] admins`, and the owner of a personal bot) and `everyone` are built-in roles;
  `admins_only = true` is short for `roles = ["admins"]`.
- A tool a person may not call is left out of `tools/list`, so the agent does not see it, and a call to it is
  refused with a tool error ("not available to this person here").
- **Confirmation.** With `confirm = "destructive"`, a call of a tool that the server does not annotate as
  `readOnlyHint` (or as `destructiveHint: false`) waits: the conversation shows the tool and its arguments with
  **Allow** and **Deny**, and only the requester (or, with `confirm_by = "admins"`, an administrator in that
  chat) can answer. Platforms with buttons (Discord, Telegram, Slack, Google Chat) show buttons; elsewhere the
  message ends with `/approve <code>` and `/deny <code>`, which are answered at once even while the turn waits.
  No answer before the turn's timeout is a refusal, and the agent is told not to retry. `always` asks for every
  call. The arguments are shown in full; a call whose arguments do not fit one message (about 1500
  characters) is declined without asking. Annotations are hints from your server; the permissions are only
  what `okbot.toml` and grants say.
- Only tools pass the gateway: `resources/*`, `prompts/*` and other methods are answered "not offered through
  okbot", since they would bypass the tool rules.
- **Temporary roles.** `okbot role grant ops discord:main/1234 --for 2h` (or `/grant ops discord:main/1234 2h`
  by an administrator in chat) adds a role from `[roles]` until it expires; `okbot role revoke`, `/revoke`,
  `okbot role list` and `/roles` manage them. `admins` and `everyone` cannot be granted, and a grant lasts at
  most 30 days.
- **Audit.** `okbot audit [--since 24h] [--person …] [--json]` lists every tool call through the gateway and every
  `[[command]]` run: who, where, which tool, allowed, refused or declined, and how long it took. Arguments and
  results are not stored (they may hold secrets).

### Limits per person

```toml
[limits]                              # over a rolling 24 hours; administrators are exempt
turns_per_day = 50                    # 0 (default): no limit
tokens_per_day = 2000000              # input + output tokens as the agent CLIs report them
```

A person over a limit gets a fixed reply and no agent turn. Linked accounts (`[people]`) share one limit.

### Local servers (stdio)

Many MCP servers are programs that speak over stdin and stdout. okbot runs one for each agent session,
behind the same gateway and rules:

```toml
[[mcp]]
name = "github"
command = ["npx", "-y", "@modelcontextprotocol/server-github"]
env = ["GITHUB_PERSONAL_ACCESS_TOKEN"]    # passed from the environment or .env, by name
tools = ["search_*", "get_*", "list_*"]   # read-only tools only
```

The process gets an empty environment (only `PATH`, `HOME` set to its own empty directory under
`state/work/mcp/`, and the variables in `env`), runs as okbot's user, and is stopped (input closed, then killed)
when the session or the turn ends. It has no per-person identity: for that, run an HTTP server with
`auth = "signed"`. A server that reads or writes files can still reach whatever okbot's user can, so prefer
`tools = [...]`, and run okbot as a user of its own.

### Acting on each person's own account

A signed server knows who asked, so it can keep one OAuth token per person and call the service as them, never
through a shared account and never showing a credential to the agent. See [accounts.md](accounts.md) and the
runnable [`examples/mcp/accounts-server.mjs`](../../examples/mcp/accounts-server.mjs).

### Signed run identity (`auth = "signed"`)

The agent presents `Authorization: Bearer okbot1.<claims>.<signature>` instead of the secret
([ADR 0007](../adr/0007-signed-identity-and-prompts.md)). Verify it on every request:

1. split at the last `.`: `signed` and `signature`;
2. `signature == base64url(HMAC-SHA256(secret, signed))` (no padding; compare in constant time);
3. `signed` starts with `okbot1.`; the rest is base64url JSON:
   `{"bot", "mcp", "user": "discord:main/1234", "name", "conversation": {...}, "admin", "direct",
   "interact": "http://127.0.0.1:…/interact", "command", "locale", "exp", "jti"}`; reject when `exp` (Unix seconds) has
   passed. `command` is present only for a [`[[command]]`](#commands-for-your-tools) run; `locale` (`en-US`, `ja`)
   only when the platform reports the person's language (Discord slash commands and buttons, Telegram), so a
   server can answer in it.

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

→ `{"prompt": "<id>"}`. On platforms without buttons the prompt is shown as text with `/answer <id> <n>` for
each button; a prompt with a form needs Discord (`501` elsewhere). The callback must be on the server's own origin.
Only a token okbot issued for a turn or command, until it expires, is accepted (`401` otherwise): the server holds
the secret, but cannot sign claims of its own to reach another conversation.
When someone allowed presses a button (and fills in its form), okbot POSTs

```json
{"prompt": "<id>", "button": "ok", "values": {"otp": "123456"},
 "user": {"id": "discord:main/1234", "name": "Lan", "admin": true}, "conversation": {...}}
```

with `x-okbot-timestamp` and `x-okbot-signature: base64url(HMAC-SHA256(secret, "<timestamp>.<body>"))`. Answer
`{"text": "Deployed ✅"}` to replace the buttons, or add `"keep_open": true` to let them try again (a wrong code).
Values never reach the agent or the history: use forms for API keys and one-time codes. Discord shows buttons and
forms; Telegram, Slack and Google Chat show buttons; other platforms show text. Limits follow Discord (5 buttons,
5 fields).

## Commands for your tools

`[[command]]` turns one tool of an `[[mcp]]` server into a chat command that okbot runs itself, without the agent
([ADR 0008](../adr/0008-commands-for-mcp-tools.md)): it answers when no agent is available (quota, login, outage),
at once, and the same way every time. Use it for the everyday operations; leave questions to the agent.

```toml
[[command]]
name = "logs"                              # /logs — a-z, 0-9, _ (what every platform accepts)
description = "Recent errors of a service" # /help and command menus
descriptions = { ja = "サービスの最近のエラー" } # by the person's app language (menus, /help)
mcp = "ops"                                # an [[mcp]] server
tool = "company_logs"
args = ["company", "service", "since?"]    # positional; `x?` optional, `x...` the rest of the line
with = { level = "error" }                 # always passed; chat cannot change it
admins_only = false                        # implied when the server is admins_only
roles = ["ops"]                            # who may run it (default: whoever may call the tool)
```

`/logs freach channel-broker 24h` calls `company_logs {"company": "freach", "service": "channel-broker",
"since": "24h", "level": "error"}`. Any argument the tool declares, except those fixed by `with`, can be set
as `name=value` (`/logs freach agent-sales limit=20`), values take the type the tool's input schema declares (`integer`,
`number`, `boolean`, `array` as `a,b`), and `"…"` keeps spaces in one value. Missing or malformed arguments
get the usage line, without calling the tool. `/help` lists the commands the person may run. A command obeys the
same server rules as the agent (roles, rooms, tool lists), is recorded by `okbot audit`, and is not confirmed: the
person typed it.

- **Same identity, same checks.** okbot opens an MCP session with the token an agent turn would get (signed:
  with `"command": "<name>"` in the claims), lists the tools and calls the one named. A tool the server does not
  offer to this person reads as "not available to you". Prompts work as in agent turns.
- **Readable output.** The text blocks of the result are sent as Markdown, as is; an error result gets ⚠️. A
  server that also serves agents can check the `command` claim and answer in sentences instead of JSON. A tool
  that only shows a prompt may return no text: okbot then sends nothing. Lay text out for chat
  ([ADR 0009](../adr/0009-chat-text-layout.md)): the conclusion on the first line, one fact per line, `-# ` for
  small print; every line break is shown.
- **Not in the history.** Commands and their results are not part of the conversation the agent sees.
- **Everywhere.** Commands are text messages starting with `/name` (`/name@bot` too), so every platform has
  them. Where the client claims `/` for itself (Slack, Google Chat), mention the bot first:
  `@bot /logs freach …`. With at least one `[[command]]`, okbot also fills the platform's command menu with them
  and `/help`, `/reset`, `/stop`, `/whoami` (`Channel::set_commands`): Discord registers global slash commands
  (a chosen command is acknowledged and then handled as the text `/name value…`), Telegram sets its `/` menu.
  Adapters without a menu ignore the call.
