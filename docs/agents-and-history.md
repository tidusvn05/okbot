# Managing agents and reading retained history

These are local operator commands. Run them in the bot directory or pass `--bot NAME_OR_PATH`.
Each supports `--json`, including failures. They do not expose administrative history as chat or
MCP tools. Knowledge and memory continue to use their existing scoped interfaces.

## Agent lifecycle

`okbot agent detect` lists all four known providers with their executable, version and login state.
It can run before a bot exists. Inside a bot, or with `--bot`, it uses that bot's exact executable
overrides and separate `[agent].home` when checking login. Outside a bot it uses the current user's
login. Detection and `agent status` run version/auth-status commands only and consume no model quota.

`okbot agent status` shows the configured fallback order and readiness. It distinguishes a missing
CLI, logged-out CLI, unknown login, invalid model syntax and a provider blocked by tool policy.
Exit 4 indicates findings. A CLI's default model and effort are shown as defaults, not inferred;
their JSON fields are null. Providers with unknown login need an intentional live test to prove
they work.

```sh
okbot agent use codex --bot shop
okbot agent set codex --model MODEL_ID --effort high --bot shop --print
okbot agent set codex --model MODEL_ID --effort high --bot shop --restart
okbot agent chain codex,claude --bot shop
```

`use` promotes the provider and keeps the remaining providers as fallbacks. `chain` replaces the
whole order; the list must be nonempty, contain known providers and have no duplicates. Per-provider
overrides remain in `okbot.toml` when a provider leaves the active chain, so selecting it again
restores its settings. Normal tool-policy restrictions still apply; these commands never enable
unrestricted agents automatically.

`set` defaults to the primary provider when its argument is omitted. It accepts:

| Flag | Effect |
|---|---|
| `--model MODEL` | Set the model; keep the previous effort unless overridden |
| `--effort LEVEL` | Set effort for Claude/Codex; requires an explicit or previously configured model |
| `--cli PATH` | Check and store the exact executable as an absolute path (symlinks kept, so `~/.local/bin/codex` survives updates) |
| `--default-model` | Remove both model and effort overrides |
| `--default-effort` | Keep the model and remove its effort suffix |
| `--default-cli` | Remove the path override; the bot finds the CLI at each start |

Without `--cli`, nothing is stored: the bot finds the CLI at each start (`PATH`, then the usual
install directories), so a CLI that updates itself into a new versioned directory, or a bot moved to
another machine, keeps working. A pinned path that no longer exists falls back to the same search,
with a warning in the log and in `okbot doctor`.

Models keep the existing `model@effort` representation. A new suffix overrides the previous effort;
an explicit `--effort` overrides that suffix. Effort syntax is validated against the pinned
agent-call adapter: Claude supports `low`, `medium`, `high`, `xhigh`, `max`; Codex additionally
supports `ultra`. The installed CLI and selected model must also support the requested value.
OpenCode model ids use `provider/model[#variant]`; OpenCode and Devin have no separate effort flag.

Edits preserve comments and unrelated settings, validate before writing and support `--print` for
a preview. They take effect when the bot next starts. `--restart` explicitly applies an edit through
the bot's systemd service; otherwise run `okbot restart NAME`. A failed restart returns a nonzero
exit code while keeping the saved configuration: JSON includes `saved: true`, `pending_restart:
true`, `error.code: restart_failed` and `next`. Foreground/container deployments must restart their
own supervisor. `agent status` reports saved configuration, not a live process's in-memory settings.

`okbot agent test [PROVIDER]` intentionally makes one small live request (up to 60 seconds), using
the primary by default. It consumes the CLI's quota. The probe uses a temporary empty directory,
scrubs bot secrets, has no bot MCP servers, does not reuse a conversation, does not fall back and
does not write bot history. It requires an enforceable tool policy even on a development bot.
Exit 0 means a nonempty response; exit 4 means the probe failed. It reports token usage and duration
when available. Routine automated tests use fake CLIs instead of real provider quota.

## Retained conversations and runs

```sh
okbot history list --bot shop
okbot history show 1 --bot shop
okbot history search "invoice" --channel discord:main --bot shop
okbot history runs --conversation 1 --errors --bot shop --json
```

`list` supplies the database conversation id used by `show` and `--conversation`. It includes the
structured channel/space/room/thread, retained message count, latest conversation update and latest
reset boundary. Time filters on `list` apply to that update; on messages/runs they apply to the
message time/run start time.

`show` reads retained messages across the latest `/reset`. JSON marks old messages with
`before_reset: true`; terminal output labels the old/current context. This does not restore old
messages to the bot's active context. `search` finds literal substrings using Unicode lowercase,
including Vietnamese and Japanese; `%`, `_` and quotes are ordinary search characters.

`runs` displays the existing audit record per turn: run id, conversation, recorded backend/model,
granted tools, token counters, duration, error and start time. Failed turns may name the chain;
unknown model/usage stay null. It does not reconstruct individual fallback attempts or a default
model name that was not recorded.

All four commands accept `--channel PLATFORM:INSTANCE`, `--since` and `--until` as RFC3339 timestamps
with offsets, `--limit` (1–1000, default 50) and `--offset` (default 0). `search` and `runs` also
accept `--conversation ID`; `runs --errors` includes failures and cancellations. Time ranges include
`since` and exclude `until`. JSON uses UTC; terminal output uses `[bot].timezone` or UTC.

Records are selected newest first, breaking equal-time ties by message/conversation/run id.
`show` reverses the selected page for reading oldest first; increasing `--offset` walks back in
time. Separate invocations see committed writes, so page offsets can shift while new messages
arrive. Use an exclusive `--until` bound when reviewing a fixed time window.

The database is opened read-only and no migration runs. A bot without a database returns empty
history without creating its state directory. Corrupt and unsupported future databases are errors.
Retention remains `[bot].retention_days` (90 by default); deleted records cannot be reconstructed.
History covers the messages and runs the existing runtime recorded, not every platform event.
Known bot secrets and token-bearing bot URLs are redacted from command output.

## JSON and exit codes

History returns `{ "ok": true, "bot": "shop", "kind": "messages", "items": [], "limit": 50,
"offset": 0 }`. Agent status/detection returns `agents`; edits return `saved`, `changed`,
`restarted`, `pending_restart` and `next`. A preview returns `saved: false` with `preview` text.
These contracts and command help are snapshot-tested.

Exit codes follow the existing CLI contract: 0 success, 1 operation/storage/configuration error,
2 argument usage error, 3 missing user input (for example effort without a model), 4 agent readiness
findings or a failed probe. With `--json`, operational errors use the existing `error` envelope.
