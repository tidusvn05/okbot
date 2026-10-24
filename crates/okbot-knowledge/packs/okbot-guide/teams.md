---
type: Guide
title: Teams of bots
description: Several okbots that hand work to each other, on one machine or several, with keys, team manifests, links, roles, approvals, relays and limits.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-24
aliases: [team, teams, nhóm bot, nhiều bot, bot gọi bot, peer, peers, delegate, giao việc, A2A, okbot team, okbot peer, relay, leader]
generated: {by: okbot-guide-pack, at: "2026-10-24"}
---

# Teams of bots

Several okbots can hand work to each other: a main assistant that asks a research bot or an
operations bot, or a leader that gives tasks to members. Bots talk with A2A; the agent of a bot
delegates with three tools of its own bot: `team_directory`, `delegate` and `task_status`.

Two rules decide everything:

- The **team** says who is who: members, their keys, how to reach them, and which member may call
  which (links).
- Each **bot** says what is allowed: its own `okbot.toml`. A team can never give a bot more than its
  configuration allows, and never `admins`.

## 1. Bots on one machine

The shortest way: `okbot team join local --bot <name>` for each bot (`okbot init` offers it). It
writes `[teams.local]` and adds the bot to the manifest. Step by step:

```sh
okbot team init local
okbot team add local assistant --dir /srv/bots/assistant --role main
okbot team add local research  --dir /srv/bots/research  --role research --skill web-research
okbot team add local ops       --dir /srv/bots/ops       --role ops --skill infrastructure
okbot team link local --from role:main --to research --to ops
okbot team show local
```

The manifest is `~/.config/okbot/teams/local.toml`. Bots of one machine talk over a socket in each
bot's `state/`; nothing to copy, no port.

The bot people talk to:

```toml
[teams.local]
delegate_roles = ["everyone"]   # who here may make the agent delegate (default: admins)
```

The bots that take work:

```toml
[roles]
research = []

[teams.local]
accept = true                    # default false: nobody may give this bot work
roles = { main = ["research"] }  # the caller's team role -> roles here
```

A bot without chat channels runs only for its team.

## 2. Bots on several machines

The team's owner keeps a team key (better on their own computer than on a bot) and signs the
manifest:

```sh
okbot peer id                     # on each bot's machine: its public key
okbot team init acme --owner
okbot team add acme lead   --key ed25519:… --url https://lead.example.com/a2a/acme --role leader
okbot team add acme writer --key ed25519:… --url https://writer.example.net/a2a/acme --skill writing
okbot team add acme laptop --key ed25519:… --box-key x25519:… --relay lead --skill research
okbot team link acme --from role:leader --to '*'
okbot team sign acme --days 30
okbot team publish acme --to /var/www/team
```

Each member points to it and pins the team key:

```toml
[http]
listen = "127.0.0.1:8787"
public_url = "https://lead.example.com"

[teams.acme]
manifest = "https://lead.example.com/team/acme.toml"
team_key = "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
accept = true
roles = { leader = ["everyone"] }
```

- The bot's A2A endpoint is `<public_url>/a2a/<team>`, behind your HTTPS reverse proxy.
- Members re-read the manifest every 10 minutes and refuse an older version or an expired one: sign
  again before `expires`. Removing a member (`okbot team remove`, then `okbot team sign`) takes
  effect within 10 minutes.
- A bot behind NAT (a laptop) uses `--relay <member>`: it polls that member, which passes requests
  on. Requests and answers are signed and sealed with the recipient's box key (`okbot peer id`, second
  line; `--box-key`), so the relay can neither forge nor read them.
- The published manifest lists members, their URLs and the `people` in it: put it where only the
  team can fetch it, and prefer each bot's own `[people]` for accounts.

## 3. What a request from another bot may do

It runs as `peer:<team>/<caller>` with: `everyone` plus the roles mapped from the caller's team
roles, **intersected with** the roles the person who started the work has on this bot. Person ids
are shared by the team: the same `[people]` id on every bot (or the manifest's `people`).

- The person could not do it by asking this bot directly? Then the delegated request cannot either.
- `admins` is never given through another bot, and chat commands never run from one.
- A tool that needs approval asks the person who started the work, in their own chat with the first
  bot; their answer goes back along the chain. No bot approves for a person.

## 4. Limits

```toml
[teams.local]
max_depth = 2          # longest chain accepted: a -> b -> c
max_concurrent = 2     # tasks at once per calling bot
wait = 60              # seconds delegate waits before the task continues in the background
task_timeout = 600     # seconds a task this bot delegates may take
confirm = "always"     # the requester approves each delegation first
chain_tokens = 200000  # tokens a chain started here may spend; 0: no limit
```

The agent can also stop a task (`task_cancel`), and `task_status` shows what a running task is
doing. Prometheus counts delegations and tasks from other bots (`okbot_delegations_total`,
`okbot_peer_tasks_total`).

A task still running after `wait` keeps going; its result starts a new turn in the conversation it
came from. Each bot's agent turn counts against the person's `[limits]` on that bot.

## 5. Checking

- `okbot doctor`: the manifest, this bot's membership, and whether the bots it may call answer.
- `okbot peer tasks`: tasks given and received, with the trace id shared by every bot of a chain.
- `okbot audit`: the `delegate` calls, with who asked.

## 6. Agents and programs that are not okbots

```toml
[a2a]
delegate_roles = ["everyone"]

[[a2a.agent]]
name = "travel"
url = "https://agent.example.com/a2a"
token_env = "TRAVEL_A2A_TOKEN"
version = "1.0"
skills = ["travel"]

[[a2a.client]]
name = "n8n"
token_env = "N8N_A2A_TOKEN"
roles = ["everyone"]
person = "lan"
```

An agent (any A2A 1.0 or 0.3 server) gets the task text only, never the person. A client posts A2A
requests to `<public_url>/a2a/external` with its bearer token; its task cannot be handed on, and
without `person` it has only `everyone`.

## 7. Two shapes

**Main assistant with specialists.** The assistant faces people. The research bot reads the web and
has no link to ops. The ops bot accepts only the assistant, maps it to a narrow role, and sets
`confirm = "always"` on every `[[mcp]]` server that changes things: anything a web page says can
reach the research bot, and its answers reach the assistant's agent.

**Team with a leader.** Only the leader has chat channels. Members accept only `role:leader` and may
report back with `okbot team link <team> --from '*' --to role:leader`. Members with the same skill
share work: `delegate` to `skill:writing` picks one that answers.
