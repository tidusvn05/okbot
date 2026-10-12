# 0002 — `async-trait` for object-safe extension traits

Status: Accepted (2026-10-12)

## Context
The runtime stores channels, agent backends and stores as `Arc<dyn Trait>`, chosen at run time from
configuration. Native `async fn` in traits (stable since Rust 1.75) is not object-safe, and the
alternatives (manual `Pin<Box<dyn Future>>` signatures, `trait-variant`) make third-party
implementations noisier.

## Decision
Traits that are stored as `dyn` and implemented by third parties (`Channel`, `AgentBackend`,
`ConversationStore`, `MemoryStore`, `KnowledgeProvider`) use `#[async_trait]`. Internal traits that
are only used generically may use native `async fn`.

## Consequences
One boxed future per call on these traits, which is negligible next to network and agent latency.
When Rust gains object-safe async traits, a new ADR can migrate with a breaking release.
