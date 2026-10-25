---
type: Guide
title: Agent CLIs
description: Which agent CLIs okbot can use (Claude Code, Codex, OpenCode, Devin), fallback chains, models and effort, separate logins per bot, and what each one costs.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [agent, claude, codex, chọn model, đổi model, fallback, okbot agent, quota, chi phí, api key]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Agent CLIs

okbot does not call a model API itself. It runs an **agent CLI** that is installed and logged in on
the machine, so it uses that CLI's subscription or quota. No API key is stored in the bot.

| Agent CLI | Web tools | Tool lock-down | Use for |
|---|---|---|---|
| Claude Code (`claude`) | WebSearch, WebFetch | enforced | recommended |
| Codex (`codex`) | web_search | enforced | recommended |
| OpenCode, Devin | built in | **not enforceable** | only with `allow_unrestricted_agents = true`, never for a public bot |

"Lock-down" means okbot can forbid the shell and file tools. A CLI that cannot be locked down is
refused unless you explicitly allow it, because it could run commands on your server.

## Commands

```sh
okbot agent detect                  # installed CLIs, versions and login; no model call
okbot agent status                  # this bot's chain, model, effort and readiness
okbot agent use codex               # Codex first, the others stay as fallbacks
okbot agent chain codex,claude      # replace the fallback order
okbot agent set --model MODEL_ID --effort high
okbot agent test                    # one small real request (uses quota)
okbot restart                       # apply
```

When the first CLI fails (quota, login, outage), the next one in the chain answers.

The bot finds each CLI when it starts (`PATH`, then `~/.local/bin`, `~/.npm-global/bin`, `~/.bun/bin`,
`~/.cargo/bin`, `/usr/local/bin`), so nothing needs editing when a CLI updates itself into a new
directory or the bot moves to another machine. `okbot agent set codex --cli PATH` pins one
executable; if that path disappears, the bot uses the one it finds and `okbot doctor` warns.
`okbot agent set codex --default-cli` removes the pin.

```toml
[agent]
chain = ["claude", "codex"]
timeout_secs = 180          # upper bound per answer
max_concurrent = 2          # agent processes at once for this bot
tool_policy = "web"         # web (default) or mcp-only: no web search or fetch
history_tokens = 6000       # chat history budget in the prompt
# home = "agent-home"       # a separate login and quota for this bot
```

## Separate logins and quotas

All bots on a machine share the user's CLI login by default. To give a bot its own account (for
example a customer's subscription), set `[agent] home = "agent-home"` and log in once with
`HOME=/srv/bots/shop/agent-home claude`. A machine-wide cap on agent processes goes in
`~/.config/okbot/config.toml`: `[limits] max_agents = 4`.

## Cost control

- `[limits] turns_per_day` and `tokens_per_day` cap each person ([Permissions](permissions.md)).
- `[[command]]` and `text` automations answer without the agent and cost nothing.
- `okbot history runs` lists agent turns, with token usage when the CLI reports it.
