# okbot — Implementation plan

> Version 1.0 (2026-10-12) · Companion to [`design.md`](design.md) (the *what* and *why*); this document
> is the *how* and *in which order*. Section references like "design §4.4" point into `design.md`.

## 0. How to read this plan

- The work is split into **phases (P0–P14)**. Each phase has a goal, the crates it touches, tasks, tests,
  and **exit criteria** that must hold before the phase is closed.
- Phases are grouped into **releases** (§3). Every release is a usable, tagged product, never a half-built one.
- Parts 1–2 define the engineering standards and architecture rules that apply to every phase. A
  change that breaks a rule in Part 2 needs a design decision recorded in `docs/adr/`.

---

## Part 1 — Project standards (open source, Rust)

### 1.1 Repository and licensing

| Item | Decision |
|---|---|
| License | `MIT OR Apache-2.0` (same as okbase and agent-core); `LICENSE-MIT`, `LICENSE-APACHE` |
| Repository text | English (README, docs, rustdoc, CLI help, errors, commits, issues) |
| Community files | `README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` (Contributor Covenant), `SECURITY.md` (private reporting via GitHub advisories), `CHANGELOG.md` (generated), `AGENTS.md` (rules for coding agents working on okbot), `llms.txt` (agent-facing contract) |
| Third-party notices | `cargo about` → `THIRD_PARTY.md`, generated in CI |
| Decisions | Architecture Decision Records in `docs/adr/NNNN-title.md` (context, decision, consequences) |

### 1.2 Toolchain and Cargo conventions

- Rust **edition 2024**, stable toolchain, **MSRV 1.89** (aligned with okbase), workspace `resolver = "3"`.
- `rust-toolchain.toml` pins the development toolchain; CI also tests the MSRV.
- All dependency versions live in `[workspace.dependencies]`; crates use `dep.workspace = true`.
- Workspace lints:
  ```toml
  [workspace.lints.rust]
  unsafe_code = "forbid"
  missing_docs = "warn"          # deny for okbot-core (public SDK)
  unreachable_pub = "warn"
  [workspace.lints.clippy]
  all = { level = "warn", priority = -1 }
  pedantic = { level = "warn", priority = -1 }   # with a short, documented allow-list
  unwrap_used = "warn"                           # deny outside tests
  ```
- `rustfmt.toml` with defaults (plus `imports_granularity = "Crate"` on nightly fmt only if the team agrees; otherwise defaults).
- Release profile: `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`.

### 1.3 Errors, logging, configuration

| Concern | Standard |
|---|---|
| Errors in libraries | `thiserror`, one `Error` enum per crate, `#[non_exhaustive]`; never `anyhow` in libraries |
| Errors in the binary | `anyhow` with context in `okbot-cli` only; user-facing messages say what to do next |
| Panics | No `unwrap`/`expect` on runtime data; `expect` only for invariants, with the invariant in the message |
| Logging | `tracing` everywhere; JSON output in services, pretty output in the terminal; span per inbound event and per agent run (`conversation`, `run_id`) |
| Secrets | A `Secret<String>` newtype (no `Debug`/`Display` of the value); a redaction layer in the tracing subscriber removes known secrets and the `bot<TOKEN>` URL pattern |
| Configuration | `serde` + `toml`; one versioned schema (`config_version = 1`); unknown keys are errors; `toml_edit` when the CLI edits a file (keeps comments) |
| Time | `jiff` or `chrono` (pick one in P0, ADR); UTC inside, user timezone only in prompts |

### 1.4 Async and trait conventions

- Runtime: Tokio (multi-threaded). Blocking work (okbase, SQLite) goes through `spawn_blocking` behind async APIs.
- Traits that third parties implement and that the runtime stores as `dyn` (`Channel`, `AgentBackend`,
  `ConversationStore`) use `#[async_trait]` so they stay object-safe. Internal traits may use native
  `async fn` in traits. (ADR-0002.)
- Cancellation: `tokio_util::sync::CancellationToken` passed down from the runtime; every long task observes it.
- No global state: everything is constructed in `okbot-cli` and injected (§2.4).

### 1.5 Quality gates (every pull request)

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --workspace --no-default-features      # the console-only build must keep working
cargo deny check                                   # licenses, advisories, bans, sources
cargo doc --workspace --no-deps                    # with RUSTDOCFLAGS="-D warnings"
cargo +1.89 check --workspace                      # MSRV
```

- Feature matrix in CI: default, `--no-default-features`, each platform feature alone, `--all-features`.
- Tests never need network, real tokens or real agent CLIs. Real-CLI and real-platform tests are
  `#[ignore]` and run manually or in a scheduled, secret-holding workflow.
- Coverage is measured (`cargo llvm-cov`) and reported, not gated.

### 1.6 Git, versioning, releases

- Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`); `git-cliff` generates `CHANGELOG.md`.
- `main` is always releasable; work happens on short-lived branches with pull requests.
- SemVer for the whole workspace (one version, `0.x` until the plugin SDK is stable).
- Releases (same model as okbase): tag `vX.Y.Z` → CI builds `okbot` for
  `x86_64/aarch64-unknown-linux-gnu` (+ musl), `x86_64/aarch64-apple-darwin`; `SHA256SUMS`;
  GitHub Release; `install.sh`; container image `ghcr.io/<owner>/okbot`.
- **crates.io**: every dependency is on crates.io (agent-core was renamed agent-call; okbase 0.2.3
  was published on 2026-10-14), so nothing blocks `cargo publish`. okbot crates are published from
  v1.0.0, when the `okbot-core` API is frozen. The names `okbot` and `okbot-*` are free today.

---

## Part 2 — Architecture rules

### 2.1 Style: ports and adapters

```
                     ┌──────────────────────────────── okbot-cli (composition root) ─────────────────────────────┐
                     │ parses config, builds every adapter, injects them into the runtime, owns process lifetime │
                     └───────────────┬───────────────────────────────────────────────────────────────────────────┘
                                     ▼
   adapters (driving)          application                         adapters (driven)
 ┌──────────────────────┐   ┌───────────────────────────┐   ┌─────────────────────────────────────────┐
 │ okbot-channel-console│   │ okbot-runtime             │   │ okbot-agent      (AgentBackend)          │
 │ okbot-channel-zalo   │──►│ pipeline, queues, policy, │──►│ okbot-store      (ConversationStore)     │
 │ okbot-channel-gchat  │   │ commands, ContextBuilder  │   │ okbot-knowledge  (KnowledgeProvider,     │
 │ okbot-channel-…      │   └─────────────┬─────────────┘   │                   MemoryStore, MCP)      │
 └──────────┬───────────┘                 │                 │ okbot-render     (Renderer)              │
            │                             ▼                 └──────────────────┬──────────────────────┘
            └──────────────────────► okbot-core ◄─────────────────────────────┘
                              domain types + ports (traits), no I/O
```

### 2.2 Crates and allowed dependencies

| Crate | Role | May depend on (okbot crates) |
|---|---|---|
| `okbot-core` | Domain model (`InboundEvent`, `ConversationKey`, `UserRef`, `OutboundMessage`, `ChannelCaps`, `ToolPolicy`, ids), ports (`Channel`, `AgentBackend`, `ConversationStore`, `KnowledgeProvider`, `MemoryStore`, `Renderer`, `Clock`), shared errors. **The public SDK for third-party adapters** | none |
| `okbot-config` | Config schema, loading, validation, env/secret resolution, migration of `config_version` | core |
| `okbot-render` | Canonical Markdown → `MarkdownDialect`s, length-aware splitting (pulldown-cmark) | core |
| `okbot-store` | SQLite implementation of `ConversationStore`, migrations, retention | core |
| `okbot-agent` | `CliBackend` (agent-core), `FallbackBackend`, CLI detection, prompt assembly helpers | core |
| `okbot-knowledge` | okbase `Bundle`, catalog cache, `ScopeBuilder`, RunToken registry, MCP servers (`/mcp/kb`, `/mcp/bot`), memory writer, default bundle template | core |
| `okbot-runtime` | Pipeline, dedupe, access policy, rate limits, conversation queues, commands, `ContextBuilder`, shared HTTP server assembly, health | core (ports only) |
| `okbot-channel-*` | One platform each: console, zalo, gchat, discord, slack | core (+ render types via core) |
| `okbot-testkit` | Fakes: `FakeChannel`, `FakeAgent`, `MemoryStore`, fake CLI scripts, fake Zalo/GChat servers, fixtures | core (dev-dependency only) |
| `okbot-cli` | Binary `okbot`: commands, wizard, bot registry, service management, composition root | all |

Rules, enforced by review and a CI check (`cargo metadata` script):
1. `okbot-core` has no I/O and no dependency on Tokio beyond `tokio-util` cancellation types and `async-trait`.
2. `okbot-runtime` depends only on ports from `okbot-core`, never on a concrete adapter crate.
3. Adapter crates never depend on each other.
4. Only `okbot-cli` knows every crate; it is the only place that turns config into objects.
5. External crates that cross a port boundary (agent-core, okbase, rmcp, platform SDKs) never appear
   in `okbot-core` types; adapters convert at the edge.

### 2.3 Extension points

| To add | Implement | Register |
|---|---|---|
| A chat platform | `Channel` (+ a `MarkdownDialect` if needed) in a new `okbot-channel-<name>` crate | a feature in `okbot-cli`, a `[channels.<name>.*]` config section, a wizard step, a doc page (`docs/platforms/<name>.md`) |
| An agent backend (e.g. a direct API) | `AgentBackend` | `[agent.providers.<name>] kind = "<name>"` |
| A store (e.g. Postgres) | `ConversationStore` | `[store] kind = …` |
| A chat command | `Command` trait in runtime | command registry |
| A bot tool exposed to the agent | `BotTool` trait in knowledge (served on `/mcp/bot`) | tool registry, gated by `ToolPolicy` |
| A platform in another language | the bridge protocol (P14) | `okbot-channel-<name>` executable on `PATH` |

`docs/extending/` documents each, with a template crate for a new channel (`templates/channel/`) and
a conformance test suite in `okbot-testkit` that every `Channel` implementation must pass.

### 2.4 Runtime composition

```rust
// okbot-cli, simplified
let config  = okbot_config::load(dir)?;
let store   = Arc::new(okbot_store::Sqlite::open(&config.state_dir)?);
let kb      = okbot_knowledge::Knowledge::open(&config.kb, &config.memory)?;
let agents  = okbot_agent::build_chain(&config.agent)?;          // Arc<dyn AgentBackend>
let channels: Vec<Arc<dyn Channel>> = build_channels(&config)?;   // feature-gated
let runtime = okbot_runtime::Runtime::builder()
    .store(store).knowledge(kb).agent(agents).channels(channels)
    .policy(config.policy()).build()?;
runtime.run(shutdown_token).await
```

### 2.5 Data and versioning rules

- SQLite schema migrations are numbered and embedded (`rusqlite_migration`); a newer database is never
  opened by an older binary (stored schema version check).
- `okbot.toml` carries `config_version`; `okbot` migrates old files with `okbot config migrate` (never silently).
- The default bundle template carries a template version; `okbot kb upgrade` uses `generated` provenance (design §5.1).
- The bridge protocol and the MCP tool schemas are versioned and snapshot-tested (`insta`).

### 2.6 Key dependencies (initial choice, each one confirmed by an ADR in P0)

| Need | Crate |
|---|---|
| Async runtime, utilities | `tokio`, `tokio-util`, `futures` |
| HTTP server | `axum` 0.8 (same as okbase) |
| HTTP client | `reqwest` (rustls, no native-tls) |
| SQLite | `rusqlite` (bundled; same as okbase) |
| MCP server | `rmcp` (pinned to okbase's minor to avoid two versions) |
| Agent CLIs | `agent-core` (git tag) |
| Knowledge | `okbase`, `okbase-mcp` (git tag) |
| Markdown | `pulldown-cmark` |
| CLI, wizard | `clap` 4, `inquire` (or `dialoguer`) |
| JWT (Google Chat) | `jsonwebtoken` + cached JWKS |
| Google auth (service account) | `gcp_auth` (or `yup-oauth2`) |
| Discord | `twilight-gateway`, `twilight-http`, `twilight-model` |
| Slack | `slack-morphism` (Socket Mode) |
| Testing | `insta`, `wiremock`, `assert_cmd`, `proptest`, `tempfile` |

---

## Part 3 — Releases

| Release | Phases | What a user gets |
|---|---|---|
| **v0.1.0** | P0–P8 | Create bots in folders, chat in the terminal and on **Zalo** (polling or webhook), answers from an okbase bundle, web tools on by default with no shell access, several bots per VPS under systemd |
| **v0.2.0** | P9–P10 | **Google Chat** (HTTP or Pub/Sub), personal/room/global **memory** |
| **v0.3.0** | P11 | **Discord** and **Slack** |
| **v0.4.0** | P12–P13 | Session resume, progressive replies, fallback metrics, attachments |
| **v0.5.0** | P15–P20 | Personal assistant: tasks, reminders, snooze, routines, notes, saving documents; **Telegram** |
| **v1.0.0** | P14 | Stable `okbot-core` SDK and bridge protocol, crates.io publication |

---

## Part 4 — Phases

### P0 — Repository foundation

**Goal:** an empty but production-grade workspace that every later phase builds on.

Tasks:
1. `git init`, workspace `Cargo.toml`, crate skeletons from §2.2 (each compiles, `lib.rs` with crate docs).
2. Licenses, community files (§1.1), `AGENTS.md` (verify commands, hard rules, layout), `rust-toolchain.toml`, `rustfmt.toml`, `deny.toml`, `cliff.toml`, `.gitignore`.
3. CI (`.github/workflows/ci.yml`): the gates in §1.5 and the feature matrix on Linux and macOS.
4. Dependency-rule check script (§2.2) run in CI.
5. ADRs: 0001 ports-and-adapters, 0002 async traits, 0003 time crate, 0004 HTTP client, 0005 dependencies on agent-core/okbase by git tag.
6. Open an agent-core issue about the crates.io name conflict (§1.6).
7. Move `docs/design.md` and this plan into the repository; `README.md` with status "pre-alpha".

Exit criteria: CI green on an empty workspace; `cargo deny check` passes; the dependency-rule check fails on a deliberately wrong edge (tested once, then reverted).

### P1 — Domain model and ports (`okbot-core`)

**Goal:** the types and traits every other crate speaks, small and stable.

Tasks:
1. Identifiers: `BotName`, `ChannelId`, `ConversationKey`, `UserRef`, `MessageId`, `RunId` (newtypes, `Display`, `FromStr`, serde).
2. Messages: `InboundEvent`, `OutboundMessage`, `RenderedMessage`, `Attachment` (enum, `#[non_exhaustive]`), `ReplyTarget` (opaque, adapter-owned payload).
3. Capabilities: `ChannelCaps`, `MarkdownDialect`, `AgentCaps`, `ToolPolicy` (`Web` default, `McpOnly`).
4. Ports: `Channel`, `InboundSink`, `AgentBackend` (`AgentTurn`, `AgentReply`, `AgentError` with retryable classification), `ConversationStore`, `KnowledgeProvider`, `MemoryStore`, `Renderer`, `Clock`.
5. `Secret<T>` and the redaction helper.
6. Rustdoc with examples for each port; `missing_docs = "deny"`.

Tests: serde round-trips, `ConversationKey` stability (hash/format), property tests for ids.
Exit criteria: public API reviewed against design §3–§5; `cargo doc` clean; no I/O dependencies.

### P2 — Rendering (`okbot-render`)

**Goal:** turn the agent's Markdown into what each platform can show.

Tasks:
1. Parse with pulldown-cmark into a small intermediate representation.
2. Dialects: `CommonMark` (console), `ZaloMd` (design §3.3.2: no links/code blocks/tables, escaping, 2000 chars), `GChatText`, `DiscordMd`, `SlackMrkdwn` (the last two can be stubs until P11).
3. Splitter: respects `max_message_len` in characters as the platform counts them (UTF-16 for Zalo), prefers paragraph → line → sentence boundaries, never splits inside a code span, and re-opens the fence when a code block spans several messages.

Tests: snapshot tests per dialect over a shared corpus (Vietnamese text, code, tables, nested lists, long URLs); proptest: concatenating split parts preserves visible text; no part exceeds the limit.
Exit criteria: corpus snapshots reviewed; property tests pass 10k cases.

### P3 — Persistence (`okbot-store`)

**Goal:** conversation history, dedupe and audit, safe under concurrency.

Tasks:
1. Schema v1 (design §6.1): `conversations`, `messages`, `inbound_dedupe`, `agent_runs`, `users`, `schema_meta`.
2. `ConversationStore` implementation over `rusqlite` in WAL mode, behind `spawn_blocking`, with one serialized connection (ample for chat traffic; revisit with measurements). rusqlite's version must match okbase's, because `libsqlite3-sys` can be linked only once.
3. History query by message count; the runtime trims it to the prompt's token budget (P5).
4. Retention job (`retention_days`), dedupe TTL cleanup.
5. Migrations tracked with `PRAGMA user_version` (no extra crate) and "newer schema" refusal.

Tests: migrations from empty and from every previous version; concurrent writers; retention.
Exit criteria: all `ConversationStore` conformance tests in `okbot-testkit` pass.

### P4 — Agent backends (`okbot-agent`)

**Goal:** run an agent CLI turn through agent-core with explicit capabilities.

Tasks:
1. `CliBackend` over `agent_core::AgentCli` (v0.3.1): maps `AgentTurn` → `RunRequest`; until agent-core gains system prompt / MCP / tool policy (P7), declares those as unsupported in `AgentCaps` and degrades (design §4.1).
2. Per-run working directory `state/work/<hash>/`, `env_remove` list built from config secrets.
3. `FallbackBackend`: ordered chain, retry classification (`Timeout`, `Exit`, `EmptyResponse` → next; invalid request → stop), skips backends that cannot enforce the requested `ToolPolicy`.
4. **CLI detection** (design §6.3.1): PATH + known locations, `--version`, login checks (`claude auth status`, `codex login status`, `devin auth status`), optional probe. Returns a `DetectedCli` list consumed by the wizard (P6).
5. Global concurrency semaphore and per-run timeout.

Tests: fake CLI scripts in `okbot-testkit` (JSON shapes copied from agent-core's tests); detection against a temp `PATH` with fake executables; fallback ordering.
Exit criteria: `#[ignore]` smoke test answers through each installed CLI; fallback tested with a failing fake.

### P5 — Runtime and console (`okbot-runtime`, `okbot-channel-console`)

**Goal:** the full message pipeline, usable in a terminal — the first end-to-end product.

Tasks:
1. Pipeline stages as small, separately tested units: dedupe → access policy → command router → conversation queue → context build → agent run → render → send → persist (design §2.2).
2. Conversation queue: one worker per active `ConversationKey`, coalescing messages while busy, idle eviction, `/stop` cancellation.
3. Commands: `/help`, `/reset`, `/whoami`, `/stop` (memory commands come in P10).
4. `ContextBuilder` with prompt templates (persona, rules, format, context, history) in versioned files under `okbot-runtime/prompts/`.
5. Access policy: allowlists, `reply_on` (mention, dm, thread), per-user rate limit.
6. Shared HTTP server assembly (health endpoint now; webhook routes later).
7. Console channel and `okbot chat` (temporary minimal CLI; the full CLI is P6).
8. Graceful shutdown: stop intake, finish or cancel running turns within a deadline, flush store.

Tests: pipeline tests with `FakeChannel` + `FakeAgent` (ordering, coalescing, cancellation, dedupe, policy denials); prompt snapshots.
Exit criteria (**M0**): multi-turn conversation in `okbot chat` with Claude, history survives restart.

### P6 — Knowledge (`okbot-knowledge`) — read path

**Goal:** answers grounded in the bot's okbase bundle, with per-user visibility.

Tasks:
1. Default bundle template (`crates/okbot-knowledge/template/`, embedded with `include_dir` so the crate stays self-contained): `okbase.toml`, `index.md`, `_meta/vocabulary.md`, `_meta/types/{Persona,Memory,UserProfile}.md`, `persona/`, `system/` (how to use the bot, commands, policies). Lints at L2 with `okbase lint`.
2. `Knowledge::open`: `Bundle::open`, `sync`, catalog cache, file watcher → `sync` + catalog refresh.
3. `ScopeBuilder` (design §5.3) and the **RunToken registry** (issue, look up, revoke, expire).
4. Mount `okbase_mcp::router` at `/mcp/kb` on a loopback listener with okbot's auth layer and a `ScopeProvider` backed by the registry.
5. `KnowledgeProvider` port implementation used by `ContextBuilder` (catalog, persona, user profile).
6. Fallback for backends without MCP: pre-retrieval with `bundle.grep` into the prompt.

Tests: scope isolation (user A cannot `kb_get` user B's profile through MCP with A's token); expired/revoked tokens → 401; catalog refresh after a file change; template lint.
Exit criteria (**M1, part 1**): answers cite ids from `kb/`; isolation test passes.

### P7 — agent-core upstream work and secure defaults

**Goal:** the agent cannot run a shell, can browse the web, and reaches okbase over MCP.

Tasks (in agent-core, one PR per issue, each with fake-CLI tests and opt-in smoke tests):
1. [#1](https://github.com/tidusvn05/agent-core/issues/1) tool policy, including the `Web` preset.
2. [#2](https://github.com/tidusvn05/agent-core/issues/2) MCP servers + child env.
3. [#3](https://github.com/tidusvn05/agent-core/issues/3) system prompt.
4. Tag a new agent-core release; bump okbot's git tag.

Status (2026-10-12): implemented as one pull request, [agent-core#7](https://github.com/tidusvn05/agent-core/pull/7)
(the three features share `RunRequest` and the command builders). okbot pins its commit until
v0.4.0 is tagged. Measured while building it: with agent-core 0.3, Codex could read files in its
working directory; Claude's `--allowedTools` + `dontAsk` alone still allowed read-only built-ins,
so restricted runs use a `--tools` whitelist. ReadOnly/Allow policies and OpenCode support are
follow-ups.

Tasks (in okbot):
5. `CliBackend` passes `ToolPolicy`, MCP servers (`/mcp/kb`, `/mcp/bot` with the RunToken in env), system prompt; `AgentCaps` now reports them.
6. Per-room `tool_policy` resolution (`web` default, `mcp-only` override).
7. Security test suite: prompt-injection fixtures (asks to run commands, read files, exfiltrate via URL) against fake and real CLIs.

Exit criteria (**M2**): with every installed CLI, `web` rooms browse and cite URLs; no policy allows shell or file tools (verified by tests); injection fixtures produce no side effects.

### P8 — CLI, multi-bot fleet, Zalo → **release v0.1.0**

Split into three tracks that can run in parallel once P5–P7 are done.

**P8a — `okbot` CLI and onboarding (`okbot-cli`)**
1. Commands (design §6.3, §6.5): `new`, `init`, `run`, `chat`, `list`, `doctor`, `kb sync|lint|upgrade`, `config migrate`, `help --agent`.
2. Bot registry `~/.config/okbot/bots.toml` (name → directory), name resolution from the current directory.
3. Wizard (design §6.6) with three front-ends over one step engine: interactive, flags, and `--json` `ask`/`run`/`tell` steps (okbase onboard contract); exit codes 0/1/3/4 as in okbase.
4. `.env` handling: created `0600`, never printed, loaded only by `okbot run`.
5. `doctor --json`: CLIs (detection + login), platform credentials, bundle lint, MCP round trip, config validity.

**P8b — Zalo channel (`okbot-channel-zalo`)** (design §3.3.2)
1. Bot API client: typed requests/responses, token kept in `Secret`, URL never logged, error mapping (401 stop, 429 backoff with jitter, 408 retry once).
2. Intake: `polling` (long-poll loop, `deleteWebhook` at start) and `webhook` (`setWebhook` with generated secret at start, axum route, constant-time header check, immediate 2xx, async processing, `verification` logged).
3. Event mapping (private, group beta, unsupported → fixed reply without the agent), dedupe by `message_id`.
4. Sending: `ZaloMd` + split, `sendChatAction typing` loop, sequential parts.
5. Wizard step (token → `getMe`, delivery mode, public URL), doctor checks (`getMe`, `getWebhookInfo`, optional `testWebhook`), terms notice.
6. `docs/platforms/zalo.md`.

**P8c — Operations (fleet)**
1. `start|stop|restart|status|logs|remove`: systemd unit generation (user unit + linger, or system unit as root), `journalctl` for logs; foreground mode for containers.
2. Port allocation for webhook-style intake; machine-wide concurrency limit (`~/.config/okbot/config.toml`).
3. Optional per-bot `[agent].home` (separate CLI login/quota); default shared.
4. Packaging: release workflow, `install.sh`, Dockerfile (agent CLIs installed by the user, documented).

Tests: fake Zalo API server (in the Zalo crate's tests: platform fakes live with their platform) for polling and webhook; wizard step engine tests (JSON contract snapshots); systemd unit snapshots; `assert_cmd` tests for every command.
Exit criteria (**v0.1.0**): on a clean VPS, `install.sh` → `okbot new shop` → answer the wizard →
`okbot start shop` → the bot answers in a Zalo private chat from the default bundle, using the web
when needed; a second bot in another directory runs at the same time; both survive a reboot.
Release checklist (§5) passes.

### P9 — Google Chat (`okbot-channel-gchat`)

1. Service-account auth for the Chat API (`spaces.messages.create` / `patch`).
2. Intake `http`: route, Google JWT verification (both audience modes), immediate empty response.
3. Intake `pubsub`: REST pull loop, ack after the event is stored, at-least-once dedupe.
4. "thinking…" message then patch with the answer; `GChatText` dialect; threads.
5. Wizard step (delivery mode, service account file, subscription or endpoint) with printed GCP console steps; doctor checks.
6. `docs/platforms/google-chat.md`.

Tests: JWT verification with test keys; fake Pub/Sub and Chat API servers.
Exit criteria: the bot runs in a real Google Chat space in both modes.

### P10 — Memory (write path) → **release v0.2.0**

1. Memory writer: OKF frontmatter complete to L2 (`tags`, `lang`, `status`, `updated`) and an `index.md` in every new folder, so the bundle keeps linting clean; round-trip edits via okbase's frontmatter API, atomic writes, per-file locks, `bundle.sync()`.
2. `/mcp/bot` tools: `memory_save`, `memory_forget`, `memory_list`, scoped by RunToken (design §5.4).
3. Chat commands `/remember`, `/forget`; `okbot memory list|log|forget`.
4. Optional git commit per write; `source: web` tagging for runs that used the web.
5. User profile injection into the system prompt.

Tests: path traversal attempts, scope violations, concurrent writes, git history.
Exit criteria: "remember that…" works in the next turn; other users cannot read it; a bad global memory is reverted with one command.

Status (2026-10-12): done. Verified with the real Claude CLI: a note saved through `memory_save`
is used after `/reset`, another user is refused, the write is a git commit in `kb/`, and the
bundle still lints clean at L2. Building it showed that chat commands must run in order with the
conversation's messages (only `/stop` acts at once); the worker now queues them.

### P11 — Discord and Slack → **release v0.3.0**

1. Discord: twilight gateway, intents, threads, typing, 2000-char splitting, `DiscordMd`.
2. Slack: Socket Mode, 3 s ack, `thread_ts`, `SlackMrkdwn`, Events API retry dedupe.
3. Wizard steps, doctor checks, `docs/platforms/{discord,slack}.md`.
4. Discord REST tests against a fake API (twilight's proxy setting); Slack against a fake Web API and Socket Mode server.

Exit criteria: one bot directory serving Zalo, Google Chat, Discord and Slack from the same bundle.

Status (2026-10-12): Discord (twilight gateway + REST, no pings, no privileged intent by default)
and Slack (Socket Mode on tokio-websockets, written directly instead of slack-morphism) are done
and tested against fake APIs; one bot process answered Zalo and Google Chat together end to end.
rustls uses ring only and okbot installs it as the process-wide provider at start. Real Discord
and Slack workspaces have not been tried yet.

### P12 — Conversation UX

1. agent-core [#4](https://github.com/tidusvn05/agent-core/issues/4) resume and [#5](https://github.com/tidusvn05/agent-core/issues/5) streaming (PRs upstream).
2. Session resume per `ConversationKey` with stateless fallback.
3. Progressive replies on platforms with `edit` (Google Chat, Discord, Slack); Zalo keeps typing + final parts.
4. Web tool call logging from stream events.

Exit criteria: time to first visible feedback < 3 s on edit-capable platforms.

Status (2026-10-12): done. agent-core [#8](https://github.com/tidusvn05/agent-core/pull/8)
(stacked on #7) adds persisted/resumed sessions and progress events; okbot stores the session per
conversation (cleared by `/reset`), sends only the new messages when resuming, falls back to the
full history when the session is gone, and turns progress into edits of the thinking message
(tool status, then the answer as it is written). Platforms with a typing indicator (Zalo,
Discord) keep typing and send the final answer. Verified on Google Chat (fake API) with the real
Claude CLI.

### P13 — Attachments and observability → **release v0.4.0**

1. Inbound images/voice as `Attachment`s passed to agents that accept files; outbound images via public file URLs (needed by Zalo `sendPhoto`).
2. Metrics endpoint (Prometheus text format): turns, latency, errors and fallbacks per provider, queue depth.
3. Health across all bots: `okbot status --all` (services) and `okbot doctor --all` (checks).

Status (2026-10-12): done for inbound images (Zalo, Discord): the agent views them through the
`attachment_view` MCP tool, which only fetches the run's own attachment URLs over HTTPS (redirects
to HTTPS only, images only, 8 MiB). Google Chat and Slack attachments need authenticated
downloads and are not offered yet; outbound files are not implemented. `/metrics` serves
Prometheus counters (events, duplicates, ignored, turns and time by backend and outcome, queue
depth). Verified with the real Claude CLI describing an image sent through a fake Zalo API.

### P14 — Stable SDK and ecosystem → **release v1.0.0**

1. Freeze `okbot-core` API; semver checks (`cargo semver-checks`) in CI.
2. Bridge protocol (design §3.4): JSON-RPC over stdio, spec in `docs/bridge.md`, conformance tests, a reference adapter in TypeScript.
3. Channel template and conformance suite published; "add a platform" guide.
4. Publish crates to crates.io once agent-core (renamed) and okbase are published.
5. Candidates driven by users: Telegram (done, P20), Zalo OA, MS Teams, direct-API agent backend (reminders moved to P15–P18).

Status (2026-10-13): items 2 and 3 done. `okbot-channel-bridge` implements protocol 1
([`docs/bridge.md`](bridge.md)): `[channels.bridge.<instance>]` in `okbot.toml`, restart with backoff,
stderr to the log, other channels' secrets removed from the adapter's environment, `doctor` checks.
The reference adapter is plain JavaScript (`examples/bridge/echo-channel.mjs`, no build step) rather
than TypeScript. `okbot-testkit` has `conformance::channel`/`channel_stops` and `CollectSink`;
`templates/channel/` is a compiled, tested workspace member; guides are in `docs/extending/`. Verified
end to end: `okbot run` answering through the Node adapter with a fake `claude`. Items 1 and 4 wait for
a first release (no semver baseline yet); agent-call and okbase are on crates.io since 2026-10-14.

### P15 — Tasks (to-dos without notifications)

1. `okbot-core`: `Task`, `TaskKind`, `TaskStatus`, `Repeat` (parser and next-occurrence on `jiff`,
   DST-safe), the `TaskStore` port; testkit fake and conformance tests.
2. `okbot-knowledge`: areas in `paths` (`<area>/<owner>`), scope allowing `tasks/` and `notes/` owner
   folders, `FileTaskStore` (OKF files, atomic writes, ids `t-xxxxxx`), template `index.md` files.
3. `/mcp/bot`: `task_add`, `task_list`, `task_update`, `task_done`, `task_delete`; `memory_update`.
4. Chat: `/tasks`, `/done <n>`; CLI: `okbot task list|add|done|rm`.

Tests: scope isolation between users and rooms, path safety, repeat parsing, OKF lint of written files.
Exit criteria: "add buying milk to my to-dos" from Discord shows in `/tasks` on Zalo for the same owner.

Status (2026-10-14): done. Task files keep the bundle lint-clean at L2: the task's life is `state`
(OKF reserves `status` for documents) and a single `task` tag, which older bundles get added to their
vocabulary (with `tasks/` in the root index) on the first write. `routine` is refused by the tools
until P18. `memory_update` rewrites a note in place, keeping its other frontmatter.

### P16 — Reminders (scheduler and delivery)

1. Migration 3: `task_fires`; `ConversationStore` methods to claim, finish and list firings.
2. Scheduler in `okbot-runtime` (heap, notify on writes, 10-minute rescan, late grace, retries).
3. Delivery to `origin` or a channel's owner direct chat; the sent reminder added to history.
4. `[schedule]` config; `okbot doctor`: timezone set, upcoming and failed firings, which channels
   can send without a fresh user message (verify Zalo).

Tests: fake clock (on time, late, missed, crash between send and file update, restart), DST.
Exit criteria: a reminder set for two minutes later arrives once after `okbot restart`.

Status (2026-10-14): done in code and tests (fake clock: on time, repeats, missed, crash after
send, terminal tasks routed to the owner's direct chat, give-up after 5 attempts; a pipeline test
runs the real loop). The scheduler re-reads task files every 30 s instead of being notified.
`[schedule]` holds `enabled`, `deliver_to` and `late_grace_minutes`; `doctor` warns when
`[bot] timezone` is unset and lists undelivered firings. Whether Zalo accepts a message long after
the user's last one is checked on the real bot.

### P17 — Snooze and repeats

1. `/snooze <duration|time>` on the last reminder of a conversation; `task_update` for free text.
2. Repeating reminders and to-dos (`repeat`), `okbot task list --upcoming`.

Exit criteria: "remind me again in 10 minutes" after a reminder brings it back once.

Status (2026-10-14): done. `/snooze` (alias `/later`) takes `10m`, `2h`, `1d`, Vietnamese units
(`10 phút`, `2 giờ`), bare minutes, a clock time (`15:30`, today or tomorrow) or a date-time, and finds
the last reminder from the history line the scheduler writes; it reopens a one-shot reminder. Repeats
were already done in P15–P16.

### P18 — Routines (scheduled agent turns)

1. `InboundEvent.source` (`Platform` | `Schedule { task_id }`); worker path that skips dedupe and
   commands; routine preamble in the prompt.
2. Role check at creation and at every firing; `max_agent_runs_per_day`; paused when created in a turn
   that used web tools; `/tasks resume|pause <id>`, `okbot task pause|resume`.

Exit criteria: a daily 7:00 summary routine runs with real Codex and Claude; a team member cannot
create one; the daily cap holds.

Status (2026-10-14): done. Instead of `InboundEvent.source`, the scheduler sends the prompt (with a
preamble) to the inbound sink as the author's message, so no SDK type changed; `RunInfo.admin` tells
the tools who may create routines. Every routine the agent creates starts paused (not only after web
use): the agent cannot tell which text in its turn came from the web. Verified with real Codex: a
daily routine created in chat, resumed with `/resume`, ran at its time and answered from `task_list`.
Not yet tried with Claude.

### P19 — Notes and saving documents

1. `note_add`, `/note`; `notes/<owner>/<yyyy>/<date>.md`.
2. `knowledge_add` (owner/admins): text, URL source, attachments (images, PDF through okbase's
   converters); `okbot kb add|inbox`.
3. `okbot memory edit`; docs: `docs/assistant.md` (user guide for personal bots).

Exit criteria: a PDF sent in chat with "save this" is answered from in a later turn.

Status (2026-10-14): done. The journal is one OKF file per owner and day; `MemoryStore::note` (a
default method, so other stores keep compiling) serves `/note` and `note_add`. `knowledge_add` keeps
the original file next to a markdown document and lets okbase index it (okbot's okbase build reads
PDF, Word, PowerPoint, OpenDocument, RTF, EPUB, HTML and images). Discord now delivers non-image
attachments as files, numbered with images; Zalo's Bot API delivers images only, so the exit check
runs on Discord. `okbot memory edit` opens the file in `$EDITOR`. User guide: `docs/assistant.md`.

### P20 — Telegram (`okbot-channel-telegram`)

1. Bot API client on reqwest (token only in URLs that are never logged), long polling with offsets and
   webhook with the `secret_token` header.
2. `MarkdownDialect::TelegramHtml` in `okbot-render` (code blocks re-wrapped in `<pre>` when split);
   a plain-text resend when Telegram rejects the markup.
3. Groups (mentions, replies, `/cmd@bot`), forum topics as threads, photos and documents through
   `getFile`; config, wizard, `okbot channel add telegram`, `doctor` (token, privacy mode against
   `reply_on`, `getWebhookInfo`).

Status (2026-10-14): done in code and tests against a fake Bot API; real-bot smoke test pending
(release checklist).

---

## Part 5 — Release checklist (every release)

1. All quality gates (§1.5) green on `main`; MSRV build green.
2. Opt-in real tests run and recorded in the PR: one smoke test per installed agent CLI, one per platform added in the release.
3. Security review of the diff since the last release (secrets in logs, scope isolation, tool policies, webhook auth).
4. Docs updated: README, `docs/platforms/*`, `llms.txt`, `okbot help --agent`, CHANGELOG generated.
5. Config and database migrations tested from the previous release.
6. Tag, release workflow, `SHA256SUMS`, container image; install and upgrade tested on a clean VPS.

## Part 6 — Risks

| Risk | Mitigation |
|---|---|
| agent-core PRs take longer than expected | `AgentCaps` degradation keeps okbot working; okbot may pin an agent-core branch until tagged; P7 is the only hard gate before a public bot |
| Agent CLI output formats change | agent-core owns parsing; okbot's fake-CLI fixtures are refreshed from smoke tests each release |
| Zalo Bot API changes (young platform, groups in beta) | Typed client isolated in one crate; fake server tests; `doctor` surfaces API errors clearly |
| Prompt injection through chat or the web | No shell/file tools in any policy, scope-limited MCP, `mcp-only` rooms, tagged memory writes (design §4.4) |
| CLI quota shared by many bots | Machine-wide concurrency limit; per-bot `[agent].home` |
| Publishing to crates.io blocked by git dependencies and the `agent-core` name | Resolved 2026-10-14: agent-call and okbase are on crates.io |
| okbase write API never lands | okbot's own writer is complete on its own; switching is an internal change |
