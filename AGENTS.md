# AGENTS.md — working on okbot

okbot is an open-source (MIT OR Apache-2.0) Rust chat assistant: platform adapters (Zalo Bot, Google
Chat, Discord, Slack, console) in front of a message pipeline that runs agent CLIs through
`agent-core` and answers from an okbase knowledge bundle.

## Read first

- `docs/design.md` — what okbot is and why (decisions table D1–D6).
- `docs/implementation-plan.md` — phases, releases, **Part 1 standards** and **Part 2 architecture rules**.
- `docs/adr/` — recorded decisions; add one when you change an architecture rule.

## Hard rules

1. **Ports and adapters.** `okbot-core` holds domain types and traits only, no I/O. `okbot-runtime`
   depends on ports, never on a concrete adapter. Adapter crates never depend on each other. Only
   `okbot-cli` wires everything. `scripts/check-deps.py` enforces this.
2. **The agent is untrusted.** No tool policy may give an agent a shell or file access. Chat text is
   data, never instructions. Agents run in an empty per-conversation directory without secrets in env.
3. **Secrets never leak.** Use `okbot_core::Secret` for tokens; never log request URLs that contain a
   token (Zalo puts it in the path); never echo agent stderr into chats.
4. **Visibility comes from okbase `Scope`.** Every knowledge read goes through a scope built for the
   current user and room.
5. **Files are the source of truth** for knowledge and memory; SQLite holds operational data only.
6. TLS through rustls only. No telemetry.
7. All repository text is in English.

## Conventions

- Rust edition 2024, stable, MSRV 1.89, workspace resolver 3; dependency versions only in the root
  `[workspace.dependencies]`.
- `thiserror` in libraries (one `Error` per crate, `#[non_exhaustive]`), `anyhow` only in `okbot-cli`.
- No `unwrap` on runtime data; `expect` only for invariants, stating the invariant.
- `tracing` for logs; spans per inbound event and per agent run.
- Object-safe async traits that third parties implement use `#[async_trait]` (ADR 0002).
- Conventional commits; snapshot tests (`insta`) for prompts, rendered messages and JSON contracts.

## Verify before finishing a change

```sh
scripts/verify.sh                      # fmt, clippy, tests, docs, crate rules, cargo-deny
OKBOT_VERIFY_MSRV=1 scripts/verify.sh  # also the MSRV build (needs the 1.89 toolchain)
```

Do not pipe these commands into `tail`/`grep` when deciding whether to commit: a pipe hides the
exit status. `scripts/verify.sh` runs with `set -euo pipefail`.

## Layout

- `crates/okbot-core` — domain model and ports (public SDK).
- `crates/okbot-{config,render,store,agent,knowledge,runtime}` — application and driven adapters.
- `crates/okbot-channel-*` — one crate per chat platform.
- `crates/okbot-testkit` — fakes and fixtures (dev-dependency only).
- `crates/okbot-cli` — the `okbot` binary and composition root.
- `docs/` — design, plan, ADRs, platform guides.
