# 0018 — Reloading access lists without a restart

Status: Accepted (2026-10-30)

## Context
Who the bot answers and with which rights comes from `okbot.toml`: `[bot] admins`, each channel's
`allow_users` and `blocked_users`, `[people]` and `[roles]`. The runtime read them once at start, so
every change (an admin adding a person from a gateway, `okbot people link`) needed `okbot restart`,
which cuts the platform connections and interrupts answers in progress (#61).

## Decision
- **Reloadable lists, fixed everything else.** `okbot_runtime::AccessLists` holds those five lists.
  The runtime keeps them behind the access gate (`policy::Gate`, an `RwLock<Arc<AccessLists>>`) and
  every check — answering, admin and role checks, prompt and approval answerers, routines, `[[notify]]`
  `admins`, automations' `run_as`, peer people — reads the current snapshot.
  `RuntimeHandle::reload_access` swaps it. Other settings (channels, agents, `[[mcp]]`, commands,
  limits) still apply at start only: changing them safely means rebuilding connections.
- **Triggers.** `okbot run` checks `okbot.toml` every two seconds (modification time and size) and
  reloads on SIGHUP. `okbot reload <bot>` validates the file, then sends SIGHUP to the service's main
  process only (`systemctl kill --kill-whom=main`), so agent CLIs it runs are not signalled. A file that
  does not load is logged and changes nothing.
- **Snapshots, not locks.** A turn takes the lists it needs when it checks them. A turn already
  running when an admin is removed finishes; the next event is checked with the new lists. Temporary
  grants (`/grant`) live in the store and were already live.

## Consequences
- Access changes take effect in seconds without dropping conversations.
- `RuntimeConfig::access`, `people` and `roles` are the starting values only; code inside the runtime
  reads `Gate::lists()`.
- A polling watcher costs one `stat` every two seconds and needs no new dependency.
