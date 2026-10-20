# Adding a chat platform

This guide adds a native Rust channel. For an adapter in another language, see the
[bridge protocol](../bridge.md) instead: it needs no change to okbot.

## 1. Start from the template

```sh
cp -r templates/channel crates/okbot-channel-<name>
```

Then in the copy:

- `Cargo.toml`: rename the package to `okbot-channel-<name>`, drop `publish = false`, add the platform's
  dependencies through `[workspace.dependencies]` (HTTP: `reqwest` with rustls; WebSocket:
  `tokio-websockets`). Add the crate to `[workspace.dependencies]` in the root `Cargo.toml`.
- `src/lib.rs`: rename `ExampleChannel`; set the platform name used in channel ids (`<name>:<instance>`).
- `src/client.rs`: replace the in-memory stand-in with the platform client. Keep okbot types out of it.

A channel crate may depend on `okbot-core` and `okbot-render` only (`scripts/check-deps.py`).

## 2. Implement `Channel`

| Method | Contract |
|---|---|
| `id()` | `ChannelId::new("<name>", instance)` |
| `caps()` | Real limits: `max_message_len` and `length_unit` (UTF-16 for JavaScript-based APIs), `dialect`, `threads`, `edit`, `typing` |
| `run(sink, shutdown)` | Connect, turn platform events into `InboundEvent`s, `sink.deliver(event)`. Reconnect with backoff on transient errors; return `Err(ChannelError::Auth)` for rejected credentials (no retry); return `Ok(())` soon after `shutdown` is cancelled or when `deliver` fails |
| `send(to, message)` | Post `message.text` (already rendered and split) to `to.conversation`; return the platform message id |
| `edit` | Only when `caps.edit`; the default returns `Unsupported` |
| `typing` | Best effort; the default does nothing |
| `open_direct(user)` | Only when the platform lets a bot start a direct chat with someone who never wrote to it: return the same `ConversationKey` that person's direct messages arrive with. The default returns `Unsupported`, and the runtime then uses only direct chats it has seen ([notifications](../notify.md)) |
| `webhook()` | For push delivery: an `Arc<dyn WebhookHandler>` mounted at `/<name>/<instance>` on `[http].listen`. Verify the platform's signature there |

Normalizing events (`InboundEvent`):

- `event_id`: the platform's unique id; the runtime drops duplicates, so redeliveries are safe.
- `author.is_bot = true` for bots, including the bot's own messages.
- `text`: strip the bot's mention, write other mentions as `@name`.
- `mentions_bot` and `is_direct` decide when the bot answers (`reply_on`).
- `restricted = true` when the platform withheld content.

Errors: map platform failures to `ChannelError::{Auth, RateLimited, Transient, Failed}`; the runtime
retries `Transient` and `RateLimited`. Never put tokens in error messages; wrap them in `Secret`.

## 3. Formatting

Replies are rendered by `okbot-render` from CommonMark into `caps.dialect`. If the platform's formatting
differs from every existing dialect, add a `MarkdownDialect` variant in `okbot-core` and its renderer in
`okbot-render`, with snapshot tests (`insta`) for headings, lists, code, links and tables.

The input is read the way chat apps show text ([ADR 0009](../adr/0009-chat-text-layout.md)): every line break
is kept, a line after a list starts a new paragraph, and a `-# ` line is a note (small print: Discord subtext,
emphasis elsewhere). A new dialect writes notes in its own small or quiet form.

## 4. Test

- Unit tests for event conversion, with recorded platform payloads in `tests/fixtures/`.
- A fake platform server (see `okbot-channel-zalo/tests/fake_api.rs`, `okbot-channel-slack/src/tests.rs`)
  so `run`, `send` and `edit` are tested over the real protocol.
- The conformance checks, against the fake server:

  ```rust
  okbot_testkit::conformance::channel(channel.as_ref(), &target).await;
  okbot_testkit::conformance::channel_stops(channel, sink, CancellationToken::new()).await;
  ```

- Optional real-platform tests marked `#[ignore]`, reading tokens from the environment.

## 5. Register in `okbot`

1. `crates/okbot-cli/Cargo.toml`: an optional dependency and a feature `<name>`, added to `default`.
2. `okbot-config`: a `[channels.<name>.<instance>]` section (`schema.rs`), secret variable names in
   `secret_names()`, validation in `Config::validate`, and the name in `BUILT_IN_PLATFORMS`.
3. `crates/okbot-cli/src/bot.rs`: build the channel from its section (a `<name>_channels` function and a
   stub when the feature is off).
4. `okbot doctor`: check the credentials with a cheap API call (`check_<name>`).
5. `okbot init`: a wizard step and non-interactive flags; write secrets to `.env` with `upsert_env`.

## 6. Document

- `docs/platforms/<name>.md`: creating the app or bot on the platform, permissions, delivery modes,
  limits, a full config example.
- README platform table, `okbot help --agent`, CHANGELOG entry (`feat(<name>): …`).
