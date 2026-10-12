# 0004 — `reqwest` with rustls as the HTTP client

Status: Accepted (2026-10-12)

## Context
Platform APIs (Zalo Bot API, Google Chat, Pub/Sub) are called from async code. okbase uses `ureq`,
which is blocking.

## Decision
Use `reqwest` with `default-features = false` and `rustls-tls`. `native-tls` and OpenSSL are banned in
`deny.toml`.

## Consequences
Every request URL that can contain a secret (the Zalo Bot API puts the token in the path) must be
built through the adapter's client wrapper, which redacts it from errors and logs.
