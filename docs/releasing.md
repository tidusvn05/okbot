# Releasing okbot

One tag `vX.Y.Z` on `main` produces everything a release ships. Each output serves different users:

| Output | For | Made by |
|---|---|---|
| **GitHub release**: Linux and macOS archives, `SHA256SUMS`, release notes | operators on a VPS: `install.sh` downloads a prebuilt binary, no Rust toolchain needed | `Release` workflow, jobs `build` and `release` |
| **Container image** `ghcr.io/tidusvn05/okbot:X.Y.Z` (and `latest`) | Docker deployments | job `image` |
| **crates.io**: every crate at `X.Y.Z` | Rust users (`cargo install okbot`), and third-party adapters that depend on `okbot-core` and `okbot-testkit` | job `crates`: `scripts/release.py publish` with a short-lived token from Trusted Publishing |

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

## Publishing without stored tokens

okbot publishes with crates.io **Trusted Publishing**. Every crate trusts exactly this repository,
`release.yml` and the GitHub environment `crates-io`. The `crates` job then asks crates.io for a token
(through `rust-lang/crates-io-auth-action`). The token lives 30 minutes and is revoked when the job
ends. No crates.io token is stored in GitHub, so there is nothing to leak or rotate.

Trusted Publishing cannot create a crate. The first version of each crate is published by hand with
a temporary token, as follows.

### First release (once)

1. On crates.io, create a token with the scopes `publish-new`, `publish-update` and
   `trusted-publishing`, limited to the crates `okbot` and `okbot-*`, expiring in a few days.
2. Prepare the release as in [Steps](#steps), step 1, and merge it. Then, from the merged `main`
   and before pushing the tag:
   ```sh
   cargo login                                   # paste the temporary token
   scripts/release.py publish                    # 16 new crates, up to ~2.5 h: crates.io allows
                                                 # about one new crate every 10 minutes
   scripts/release.py trustpub tidusvn05/okbot --only
   cargo logout
   ```
   `trustpub` makes each crate trust `tidusvn05/okbot`, `release.yml` and the `crates-io` environment.
   `--only` makes crates.io refuse any other way of publishing these crates.
3. Delete the token on crates.io. In GitHub, create the environment **`crates-io`** (Settings →
   Environments). You can add yourself as a required reviewer, so each publish waits for your
   approval.
4. Push the tag. The `crates` job finds every crate already published and skips them.

### A new crate later

When a release adds a crate (a new `okbot-channel-*`, `okbot-peer` in v0.10.0), the `crates` job
publishes the crates that come before it in publishing order (`scripts/release.py order`), then
stops and names it. By then its dependencies, dev-dependencies included, are on crates.io at the new
version, so it can be published by hand with a token that has the `publish-new` and
`trusted-publishing` scopes:

```sh
cargo login                                   # paste the temporary token
cargo publish --locked -p <crate>
scripts/release.py trustpub tidusvn05/okbot --crates <crate> --only
cargo logout
```

Then re-run the failed job (`gh run rerun <run id> --failed`): it skips what is published and goes
on. Before v0.11.0 the job refused to start while any crate was new, so `okbot-core` and
`okbot-testkit` 0.10.0 had to be published by hand too, with `trustpub_only` lifted for the time.

### Other repositories

The same command sets up any other project of yours, for example
`scripts/release.py trustpub tidusvn05/zalo-bot --crates zalo-bot --only`. That repository's
workflow needs `permissions: id-token: write` and the `rust-lang/crates-io-auth-action@v1` step,
and must run in the environment named in the configuration (`--environment`, default `crates-io`;
use `--environment ""` for none).

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
- **crates.io:** re-run the `crates` job. Versions already published are skipped. If the job says
  crates.io refused the token, check the crate's Trusted Publishing settings: the repository, the
  workflow `release.yml` and the environment `crates-io` must match.
- **Never** replace the assets of a published tag: users have verified their checksums. Ship
  `X.Y.Z+1` instead.
- crates.io versions cannot be deleted. A broken version is yanked with
  `cargo yank --version X.Y.Z <crate>` for each crate, and a fixed patch release follows.
