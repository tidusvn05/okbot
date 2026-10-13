# Releasing okbot

Versions follow SemVer; the whole workspace shares one version (`[workspace.package] version`).

1. Make sure `main` is green (`scripts/verify.sh`, CI) and the opt-in real tests pass for each
   agent CLI and platform touched by the release (see the release checklist in
   `docs/implementation-plan.md`, Part 5).
2. Set the version in `Cargo.toml` (`[workspace.package]` and every internal crate entry in
   `[workspace.dependencies]`), then `cargo update -w`.
3. Generate the changelog: `git cliff --tag vX.Y.Z -o CHANGELOG.md`.
4. Commit (`chore(release): vX.Y.Z`), tag and push:
   ```sh
   git tag -a vX.Y.Z -m "okbot vX.Y.Z" && git push origin main vX.Y.Z
   ```
5. The `Release` workflow checks that the tag matches the version and the changelog, runs the
   tests, builds Linux and macOS archives with `THIRD_PARTY_LICENSES.md`, publishes the GitHub
   release with `SHA256SUMS`, and pushes `ghcr.io/tidusvn05/okbot:<version>`.
6. Bump to the next `-dev` version.

Never replace the assets of a published tag: users verified their checksums.

Every dependency comes from crates.io (ADR 0005, amended). okbot crates are published to crates.io
from v1.0.0, when the `okbot-core` API is frozen; until then releases are binaries and images only.
