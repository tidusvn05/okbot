# Contributing to okbot

Thanks for your interest! okbot is dual-licensed under MIT OR Apache-2.0. Unless you state otherwise,
any contribution you submit is licensed the same way, with no additional terms.

## Before you start

- Read [`AGENTS.md`](AGENTS.md) (rules and conventions), [`docs/design.md`](docs/design.md) and
  [`docs/implementation-plan.md`](docs/implementation-plan.md) (Part 2: architecture rules).
- For larger changes (public API of `okbot-core`, config format, defaults, security behavior), open an
  issue first. Decisions that change an architecture rule get an ADR in `docs/adr/`.

## Development

Requirements: stable Rust (MSRV 1.89), Python 3 (for `scripts/check-deps.py`) and
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny).

Run these before opening a pull request (CI runs the same checks, plus macOS and an MSRV build):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 scripts/check-deps.py
cargo deny check
```

Tests never need the network, real tokens or real agent CLIs. Tests against real CLIs or platforms are
marked `#[ignore]` and documented next to the test.

## Pull requests

- Conventional commit messages (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`); the
  changelog is generated from them.
- Add tests. Output meant for agents or users (prompts, rendered messages, JSON contracts) gets snapshot tests.
- Signing off commits (`git commit -s`, DCO) is encouraged.

## Code of Conduct

This project follows the Contributor Covenant 2.1 (see [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md)).
