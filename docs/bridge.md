# Bridge protocol (version 1)

A **bridge adapter** connects okbot to a chat platform from a separate program, written in any
language. Use it when a platform's best SDK is in Node, Python or Go, or when you want to ship an
adapter without changing okbot. Native Rust channels (`docs/extending/channel.md`) remain the choice
for platforms okbot ships with.

okbot starts the adapter as a child process and talks to it over **stdin/stdout**, using
**JSON-RPC 2.0, one JSON object per line** (UTF-8, `\n`-terminated, no embedded newlines). Anything the
adapter writes to **stderr** goes to okbot's log, line by line.

The reference adapter is [`examples/bridge/echo-channel.mjs`](../examples/bridge/echo-channel.mjs)
(Node, no dependencies). Copy it and replace the parts marked `PLATFORM`.

## Configuration

```toml
[channels.bridge.main]               # instance name: the channel id is `<platform>:main`
platform = "matrix"                  # lowercase a-z 0-9 - _; not a built-in platform name
command = "okbot-channel-matrix"     # default: okbot-channel-<platform> on PATH;
                                     # a path with `/` is relative to the bot directory
args = []
env = ["MATRIX_TOKEN"]               # passed to the adapter, from .env or okbot's environment
max_message_len = 4096               # okbot splits longer replies (counted in chars)
dialect = "common-mark"              # common-mark | plain | slack-mrkdwn | discord-md | g-chat-text | zalo-md | telegram-html
threads = false
edit = true                          # the adapter implements `edit` (enables "thinking…" + progress)
typing = false                       # the adapter implements `typing`
reply_on = ["dm", "mention"]         # same access options as the built-in channels
allow_users = []
blocked_users = []

[channels.bridge.main.settings]      # free-form, sent to the adapter in `initialize`
poll_timeout = 30
```

The adapter inherits okbot's environment plus the variables listed in `env`; the secret variables of
other channels are removed. Never put secrets in `settings`: `okbot.toml` is not a secret file.

The capabilities are configuration, not negotiated: the runtime needs them before the adapter runs.

## Lifecycle

1. okbot starts the process and sends `initialize`.
2. The adapter connects to its platform, answers `initialize`, then sends `inbound` notifications.
3. okbot calls `send`, `edit` and `typing` while it answers.
4. On shutdown okbot sends `shutdown` and waits up to 5 seconds; the adapter should exit by itself.
   Afterwards the process is killed.
5. When the adapter exits unexpectedly or `initialize` fails, okbot restarts it after 2, 4, 8 … up to
   64 seconds.

## Messages from okbot (requests)

Every request has an `id`; answer each with exactly one response carrying the same `id`, either
`{"jsonrpc":"2.0","id":…,"result":{…}}` or
`{"jsonrpc":"2.0","id":…,"error":{"code":-32000,"message":"…"}}`. Answer within 60 seconds. Requests
may overlap: answer them in any order.

### `initialize`

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "protocol":1, "channel":"matrix:main", "settings":{"poll_timeout":30}}}
```

Result: `{"protocol": 1, "name": "<adapter name and version>"}`. A different `protocol` stops the
adapter with an error. Report errors that make retrying pointless (a rejected token) as an error
response with a clear message.

### `send`

```json
{"jsonrpc":"2.0","id":2,"method":"send","params":{
  "target":{"conversation":{"channel":"matrix:main","space":null,"room":"!abc:example.org","thread":null},
            "in_reply_to":"987"},
  "text":"Already rendered in the configured dialect, at most max_message_len long"}}
```

Result: `{"message_id": "<platform message id>"}` (a string). okbot uses it for `edit`.

### `edit` (only when `edit = true`)

```json
{"jsonrpc":"2.0","id":3,"method":"edit","params":{
  "target":{…as in send…}, "message_id":"555", "text":"The new text"}}
```

Result: `{}`.

### `typing` (only when `typing = true`)

`{"target": {…}}` → result `{}`. Best effort: errors are logged, never shown to users.

### `shutdown`

`{}` → result `{}`, then exit.

## Messages from the adapter (notifications)

Notifications have no `id`; okbot does not answer them.

### `inbound`

A message for the bot:

```json
{"jsonrpc":"2.0","method":"inbound","params":{"event":{
  "event_id":"upd-1001",
  "conversation":{"channel":"matrix:main","space":null,"room":"!abc:example.org","thread":null},
  "author":{"id":"42","display_name":"An","is_bot":false},
  "text":"Hello bot",
  "attachments":[{"kind":"image","url":"https://…","caption":null}],
  "mentions_bot":false,
  "is_direct":true,
  "restricted":false,
  "message_id":"987",
  "sent_at":"2026-10-13T08:00:00Z"}}}
```

| Field | Meaning |
|---|---|
| `event_id` | Unique per platform event; okbot drops duplicates for 24 hours, so redelivery is safe |
| `conversation.channel` | Must equal the `channel` from `initialize`; other events are dropped |
| `conversation.room` | The chat, channel or DM id; `space` and `thread` when the platform has them |
| `author.is_bot` | okbot ignores bots, including itself: set it for the bot's own messages |
| `text` | Remove mentions of the bot; write other mentions as `@name` |
| `attachments` | `{"kind":"image","url","caption"}`, `{"kind":"audio","url"}`, `{"kind":"sticker","id"}`, `{"kind":"file","url","name"}` |
| `mentions_bot` / `is_direct` | Drive `reply_on`: in groups okbot only answers mentions unless configured otherwise |
| `restricted` | The platform withheld the content; okbot answers with a fixed notice |
| `message_id` | Lets replies quote or thread under the message (`target.in_reply_to`) |
| `sent_at` | RFC 3339 timestamp |

Access rules (`allow_users`, `blocked_users`, rooms), deduplication, history and rendering are
okbot's job; adapters forward every message.

### `log`

`{"message": "…"}`: written to okbot's log at info level. Plain stderr lines work too.

## Security

- The adapter runs with okbot's user and permissions; review third-party adapters like any program.
- Webhook-style platforms: the adapter runs its own HTTP listener and verifies the platform's
  signatures itself.
- Never print secrets to stderr: okbot redacts the values of the `env` variables it passes, but not
  values the adapter derives from them.

## Testing an adapter

- Run it by hand: type `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1,"channel":"demo:t","settings":{}}}`
  and check the answer.
- Rust tests: `okbot_channel_bridge::BridgeChannel` with `okbot_testkit::conformance::channel` runs the
  same checks as for native channels (see `crates/okbot-channel-bridge/src/tests.rs`).
- End to end: `okbot doctor` checks the command and variables; `okbot run` shows the adapter's log.

## Versioning

The protocol version changes only for incompatible changes. New optional fields and methods may be added
within version 1: ignore fields you do not know, and answer unknown methods with an error response
(`code: -32601`).
