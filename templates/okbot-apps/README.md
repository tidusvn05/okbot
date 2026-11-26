# okbot-apps

The registry of [okbot](https://github.com/tidusvn05/okbot) apps (ADR 0020): pages a bot serves
behind its chat summaries. `okbot app search` lists them and `okbot app add <name>` installs one,
without a new okbot release.

## How it works

- Each app is a folder `apps/<name>/` with an `app.toml` and static files (see okbot's
  `docs/apps.md`). Nothing in an app runs on the bot: it runs sandboxed in the browser and reaches
  data only through okbot's App API, within its permissions and the person's rights.
- On every merge to `main`, CI checks every app as okbot's installer would, writes
  `index.json` and pins each app to the merge commit with the SHA-256 of each of its files.
  CI then signs the index with the registry key (`index.json.sig`).
- Signing runs in the `registry-signing` environment, which needs a maintainer's approval. A
  merged pull request alone signs nothing.
- Bots check the signature with the public keys built into okbot, then every file against its
  hash, before anything is written. Updates happen only when an operator runs `okbot app update`.

## Adding an app

1. Fork, add `apps/<your-app>/`, and add yourself to `CODEOWNERS` for that folder.
2. Ask only for the permissions the app needs. Reviewers read every file.
3. Open a pull request. CI runs `okbot app index` on it, and a maintainer reviews it.

Apps must not load anything from outside the bot. The sandbox blocks it anyway (no network, no
cookies), and reviewers refuse apps that try.

## Yanking a bad version

Add `--yank <name>@<version>=<reason>` to the index step in `.github/workflows/index.yml` and
merge. `okbot app update` reports the yanked version on every bot that has it, and stops serving
it unless the bot sets `[apps] refuse_yanked = false`.

## Keys

The maintainer makes the signing key offline with `okbot app keygen --out signing.key` and keeps
the file offline, with a backup. The key goes into the environment secret `OKBOT_APPS_SIGNING_KEY`
as `base64 -w0 signing.key`. The public key it prints goes into okbot's
`crates/okbot-apps/src/registry.rs` (`OFFICIAL_KEYS`), followed by a second, standby key for
rotation.
