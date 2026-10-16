# 0001 — Ports and adapters with a dependency-checked crate graph

Status: Accepted (2026-10-12)

## Context
okbot must add chat platforms and agent backends over time, and third parties should be able to write
adapters without touching the core. Mixing platform code into the pipeline makes every new platform a
core change and every core change a risk for every platform.

## Decision
- `okbot-core` contains domain types and ports (traits) only, with no I/O.
- The application (`okbot-runtime`) depends only on ports.
- Adapters (channels, agent backends, store, knowledge, render) implement ports and never depend on
  each other.
- `okbot` is the only composition root.
- `scripts/check-deps.py` enforces the allowed edges in CI.

## Consequences
- A new platform is one crate plus registration in `okbot`.
- `okbot-core` becomes the public SDK and must be kept small and documented (`missing_docs = deny`).
- Some conversions happen at crate edges (for example agent-call errors → `AgentError`), which is
  intentional: external types never leak into the core.
