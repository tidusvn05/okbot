# Architecture decision records

Each file records one decision: its context, the decision, and its consequences. A decision is changed
by a new ADR that supersedes the old one; old ADRs are kept.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-ports-and-adapters.md) | Ports and adapters with a dependency-checked crate graph | Accepted |
| [0002](0002-async-traits.md) | `async-trait` for object-safe extension traits | Accepted |
| [0003](0003-time-crate.md) | `jiff` for time | Accepted |
| [0004](0004-http-client.md) | `reqwest` with rustls as the HTTP client | Accepted |
| [0005](0005-git-dependencies.md) | agent-core and okbase as git dependencies | Superseded (all on crates.io) |
| [0006](0006-tasks-and-schedules.md) | Tasks as bundle files, firings in SQLite, routines as scheduled turns | Accepted |
| [0007](0007-signed-identity-and-prompts.md) | Signed run identity and prompts for your own MCP servers | Accepted |
| [0008](0008-commands-for-mcp-tools.md) | Chat commands for MCP tools | Accepted |
| [0009](0009-chat-text-layout.md) | Chat line breaks, `-# ` notes and the reply layout convention | Accepted |
| [0010](0010-notifications.md) | Notifications from other programs (`[[notify]]`) | Accepted |
| [0011](0011-mcp-gateway-and-roles.md) | An MCP gateway, roles and per-person limits | Accepted |
| [0012](0012-oauth-for-mcp-servers.md) | OAuth for `[[mcp]]` servers, per person or shared | Accepted |
| [0013](0013-automations.md) | Automations: triggers, sources and actions | Accepted |
| [0014](0014-peers-and-teams.md) | Peers and teams: bots that delegate work to each other | Accepted |
| [0015](0015-interactive-replies.md) | Interactive replies: cards, buttons, menus and forms from the agent | Accepted |
| [0016](0016-chart-images.md) | Chart images: a small PNG renderer for Vietnamese, English and Japanese | Accepted |
| [0017](0017-views-for-commands-and-notifications.md) | Views and buttons for `[[command]]` results and notifications | Accepted |
| [0018](0018-reloading-access-lists.md) | Reloading access lists (admins, allow_users, people, roles) without a restart | Accepted |
