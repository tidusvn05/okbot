# Releasing okbot

One tag `vX.Y.Z` on `main` produces everything a release ships. Each output serves different users:

| Output | For | Made by |
|---|---|---|
| **GitHub release**: Linux and macOS archives, `SHA256SUMS`, release notes | operators on a VPS: `install.sh` downloads a prebuilt binary, no Rust toolchain needed | `Release` workflow, jobs `build` and `release` |
| **Container image** `ghcr.io/tidusvn05/okbot:X.Y.Z` (and `latest`) | Docker deployments | job `image` |
| **crates.io**: every crate at `X.Y.Z` | Rust users (`cargo install okbot`), and third-party adapters that depend on `okbot-core` and `okbot-testkit` | job `crates` (`scripts/release.py publish`) |

The GitHub release is required: `install.sh` and the README install path use it, and it holds the
checksums and the release notes. crates.io does not replace it, because `cargo install` compiles for
several minutes and needs Rust 1.89+.

## Versioning

- One version for the whole workspace (`[workspace.package] version`), SemVer. While okbot is `0.x`, a
  minor bump (`0.5` → `0.6`) may break `okbot.toml`, the bridge protocol or the `okbot-core` API; a
  patch bump never does.
- `main` carries the next version with `-dev` (`0.6.0-dev`). Pre-release tags (`v0.6.0-rc.1`) make
  a GitHub pre-release and an image, but are not published to crates.io.
- The binary crate is `okbot` (in `crates/okbot-cli`), the libraries are `okbot-*`;
  `templates/channel` is never published.

## One-time setup

1. On crates.io, create an API token with the scopes `publish-new` and `publish-update`. After the
   first release, limit it to the crates `okbot` and `okbot-*`.
2. On GitHub, open Settings → Environments → **`crates-io`** and add the token as the secret
   `CARGO_REGISTRY_TOKEN`. Optionally add yourself as a required reviewer, so each publish waits for
   your approval.

## Steps

1. **Prepare** on a branch `release/vX.Y.Z`:
   ```sh
   scripts/release.py version X.Y.Z             # workspace version + internal dependencies, Cargo.lock
   git cliff --tag vX.Y.Z -o CHANGELOG.md       # notes from conventional commits
   scripts/verify.sh && scripts/release.py publish --dry-run
   ```
   Go through `docs/releases/vX.Y.Z-checklist.md` (copy the previous one; the items come from
   `docs/implementation-plan.md` Part 5). Open a pull request titled `chore(release): vX.Y.Z` and
   squash-merge it when CI is green.
2. **Tag** the merged commit:
   ```sh
   git switch main && git pull
   git tag -a vX.Y.Z -m "okbot vX.Y.Z" && git push origin vX.Y.Z
   ```
3. **Watch** the `Release` workflow. The `check` job confirms that the tag matches the version and
   the changelog, runs the tests and packages every crate; then come the archives, the GitHub
   release, the image and crates.io.
4. **Verify**: run `install.sh` on a clean machine (`okbot new`, `okbot doctor`, `okbot start`), check
   `cargo install okbot`, and `docker run ghcr.io/tidusvn05/okbot:X.Y.Z okbot --version`.
5. **Reopen development**: on a branch, run `scripts/release.py version X.Y+1.0-dev` and merge it.

## When something fails

- **Before the tag:** fix it on `main` and start over.
- **Build or GitHub release:** fix the cause and re-run the failed jobs. You can also run the
  workflow by hand with the tag (`workflow_dispatch`), which creates a draft release.
- **crates.io:** re-run the `crates` job. Versions already published are skipped.
- The first release creates 16 new crates. crates.io allows about one new crate every 10 minutes
  after a short burst, so the job runs for up to about 2.5 hours; the script waits on `429` answers
  by itself. To avoid the wait in CI, publish by hand before pushing the tag:
  `cargo login && scripts/release.py publish`. The job then finds everything already published.
- **Never** replace the assets of a published tag: users have verified their checksums. Ship
  `X.Y.Z+1` instead.
- crates.io versions cannot be deleted. A broken version is yanked with
  `cargo yank --version X.Y.Z <crate>` for each crate, and a fixed patch release follows.
