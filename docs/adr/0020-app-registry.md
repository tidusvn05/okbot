# 0020 — An app registry in a separate, signed repository

Status: Accepted (2026-11-26)

## Context
Apps (ADR 0019) are data: a manifest and static files against a versioned App API. Operators
should be able to add a new app or a fixed one without waiting for an okbot release. Installing
code from the internet into a bot that holds people's tasks and accounts is a supply-chain risk.
The source can be changed, the network can be intercepted, and an update can turn bad. Full design:
[`docs/design-apps.md`](../design-apps.md) §6.

## Decision
- **The registry is a separate repository, `tidusvn05/okbot-apps`**, not a folder of okbot. Reviews of
  third-party code stay apart from core changes. Authors can own their app's folder (CODEOWNERS)
  without rights on okbot. The signing workflow has nothing else to release. Built-in apps stay in
  `crates/okbot-apps/builtin/` and ship with the binary. `[apps] registry` holds the index URL, so
  the registry can move.
- **Signed index, pinned files.** On merge, CI checks every manifest with the same validation as
  installation. It writes `index.json`, which pins each app's version to the merge commit with the
  SHA-256 of each file, its `api` major version, an optional `okbot` lower bound and its
  permissions. CI signs the index with Ed25519 (`index.json.sig`).
- **Key.** One key, generated offline by the maintainer (`okbot app keygen`), with an offline
  backup. The private key is a GitHub Actions environment secret that needs a maintainer's approval per run, so a merged pull
  request alone signs nothing. The binary embeds two public keys (current and next) for rotation.
  `[apps] registry_keys` adds keys for another index, such as a company's own registry.
- **Three trust tiers:** built-in, registry (reviewed and signed), local (`okbot app add <dir>` or
  `--git <url> --rev <commit>`, shown as *unverified*). The registry has no unsigned section. All
  three run in the same sandbox under the same permission grants.
- **Install and update are explicit.** `okbot app search|info|add|update|remove` fetch the index
  and signature over HTTPS (rustls), verify them, fetch each file from the pinned commit, check its
  hash and the file rules (type allow-list, 2 MB per file, 10 MB per app, no symlinks), then write
  `apps/<name>/` atomically. The running bot picks the change up without a restart. Permissions are
  shown and confirmed on install. An update that asks for a new permission is applied only when
  the operator accepts it; otherwise the installed version stays. An app needing a newer App API
  than the binary serves is listed, not installed. There are no automatic updates.
- **Yanking.** The index can mark a version `yanked` with a reason. `okbot app update` reports an
  installed yanked version and marks it (`.okbot-yanked`), and with `[apps] refuse_yanked = true`
  (default) the bot stops serving it within seconds.
- **Tools for the registry.** `okbot app index` and `okbot app sign` are what the registry's CI
  runs (`templates/okbot-apps/` is the repository's starting point). okbot's built-in keys
  (`OFFICIAL_KEYS`) stay empty until the maintainer makes the key. Until then the official
  registry installs nothing and `[apps] registry_keys` names the keys of any other registry.

## Consequences
- New apps and fixes reach bots between okbot releases, and only when an operator asks for them.
- A compromised CDN, a mirror or the network cannot change an app. A compromised registry
  repository still needs the signing approval. A bad version that was signed is bounded by the
  sandbox and the permission grants, and can be yanked.
- The App API becomes a compatibility contract. v1 grows only by adding methods and optional fields,
  and breaking changes are v2, served beside v1.
- The maintainer holds one more key and approves each signing run.
