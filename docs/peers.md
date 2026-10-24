# Peers and teams: bots that work together

Several okbots can hand work to each other: a team bot whose leader gives tasks to its members, or
a main assistant that asks a research bot or an operations bot. This page shows how to set it up;
[ADR 0014](adr/0014-peers-and-teams.md) records why it works this way.

```
 person ─chat─▶ assistant ──A2A──▶ research   (reads the web; may not call ops)
                    │
                    └────────A2A──▶ ops        (infrastructure tools; every change approved)
```

- The agent of a bot delegates with MCP tools of its own bot (`team_directory`, `delegate`,
  `task_status`, `task_cancel`). Bots talk to each other with [A2A](https://a2a-protocol.org/)
  1.0 (JSON-RPC binding; 0.3 requests are answered too) plus one okbot extension.
- Agents and programs that are not okbots can take part too (§9).
- A **team** says *who is who*: the members, their keys, how to reach them and which member may call
  which. Each **bot** says *what is allowed*: its own `okbot.toml` decides what a call may do. A
  team can never give a bot more than its configuration allows.
- By default nobody can call a bot (`accept = false`), and only administrators can make a bot
  delegate.

## 1. Each bot's key

Each bot signs its requests with an Ed25519 key, created on first use in `state/peer.key`, and
has an X25519 "box" key (`state/peer.box`) for messages that pass through a relay (§3):

```sh
okbot peer id            # line 1: ed25519:… (the bot's identity); line 2: x25519:… (its box key)
```

The public key is the bot's identity in every team. Keep `state/` private; anyone with
`peer.key` can speak as the bot.

## 2. A team on one machine

Bots of the same OS user on one VPS form the **local team**. Its manifest is a file under
`~/.config/okbot/teams/`, so it needs no signature: whoever can write it already controls these
bots.

The shortest way, from each bot (it writes `[teams.local]` and adds the bot to the manifest;
`okbot init` offers it for a new bot):

```sh
okbot team join local --bot assistant --role main --delegate-role everyone
okbot team join local --bot research --role research --skill web-research --accept --map main=research
okbot team link local --from role:main --to research
```

Or step by step:

```sh
okbot team init local
okbot team add local assistant --dir /srv/bots/assistant --role main
okbot team add local research  --dir /srv/bots/research  --role research --skill web-research
okbot team add local ops       --dir /srv/bots/ops       --role ops      --skill infrastructure
okbot team link local --from role:main --to research --to ops
okbot team show local
```

Members with `dir` talk over a Unix socket in `$XDG_RUNTIME_DIR/okbot/`, named after the bot
directory: no port, no TLS, no token to copy. Each bot's keys are read from `<dir>/state/peer.pub`
and `<dir>/state/peer.box.pub`. Bots running as different OS users share sockets by setting
`OKBOT_PEER_DIR` to the same directory for all of them.

`okbot chat` in a bot directory can delegate to the bot's teams too (it does not take tasks: the bot
itself does, with `okbot run`).

Then each bot says what it accepts and who may delegate:

```toml
# /srv/bots/assistant/okbot.toml: the bot people talk to
[teams.local]
delegate_roles = ["everyone"]   # who in this bot's chats may make the agent delegate

# /srv/bots/research/okbot.toml: a member without a chat platform of its own
[teams.local]
accept = true
roles = { main = ["research"] } # the caller's team role -> roles here

# /srv/bots/ops/okbot.toml
[teams.local]
accept = true
roles = { main = ["ops"] }

[[mcp]]
name = "infra"
url = "http://127.0.0.1:9100/mcp"
roles = ["ops"]
confirm = "always"              # every change waits for the person who asked
```

A bot without `[channels]` runs only for its team: `okbot run` serves peer requests and nothing else.
A bot with `accept = false` still answers its team's requests, with a refusal that says why.

## 3. A team across machines

A team that spans machines is signed with a **team key** held by its owner, ideally on the owner's
own computer rather than on a bot:

```sh
okbot team init acme --owner            # creates ~/.config/okbot/teams/acme.key and acme.toml
okbot team add acme lead     --key ed25519:… --url https://lead.example.com/a2a/acme --role leader
okbot team add acme writer   --key ed25519:… --url https://writer.example.net/a2a/acme --role member --skill writing
okbot team add acme research --key ed25519:… --box-key x25519:… --relay lead --role member --skill web-research
okbot team link acme --from role:leader --to '*'
okbot team link acme --from '*' --to role:leader
okbot team person acme lan --account zalo:shop/123 --account discord:main/456
okbot team sign acme                    # writes acme.toml.sig, version + 1
```

Publish both files where members can fetch them over HTTPS: `okbot team publish acme --to <dir>`
copies `acme.toml` and `acme.toml.sig` into a directory your web server serves. Whoever can fetch
the manifest reads the members, their URLs and the `people` in it, so serve it where only the team
reaches it (a private path, basic auth at the proxy, a VPN). Each member then points to it:

```toml
[teams.acme]
manifest = "https://lead.example.com/team/acme.toml"
team_key = "ed25519:…"          # pinned: the manifest must be signed by this key
accept = true
roles = { leader = ["member"] }
```

- To try a team that spans machines on one machine, members may fetch the manifest from
  `http://127.0.0.1:<port>/…` (loopback only), and members' `url`s may be `http://127.0.0.1:<port>/a2a/<team>`.
- Members fetch the manifest every 10 minutes. A manifest with a lower version is refused; an expired
  one (`expires`, at most 90 days ahead) stops the team until it is signed again.
- **Removing a member** is `okbot team remove acme writer && okbot team sign acme`. Members stop
  trusting it within 10 minutes, or at once when a caller presents the newer version.
- **Relays** let a member behind NAT take part (`relay = "lead"`): it polls the relay member over
  HTTPS (`okbot/relay.poll`, held up to 20 seconds), runs what comes, and posts the answers back
  (`okbot/relay.reply`); callers send through the relay (`okbot/relay.forward`). Requests and
  answers are signed by their sender and **sealed for their recipient** with its box key (X25519,
  HKDF-SHA256, ChaCha20-Poly1305), so the relay can neither forge nor read them. A relayed member
  needs `--box-key` in the manifest, and so does every member that calls it.
- **Rotating the team key**: sign the manifest with the old and the new key (the `.sig` file holds
  one line per signature), change `team_key` on every member, then stop signing with the old key.

## 4. What a delegated request may do

A request from another bot runs as the account `peer:<team>/<caller>` in the room
`<caller>/<context>`. Its roles are:

> `everyone` + the roles mapped from the caller's team roles, **intersected with** the roles the
> origin person has on this bot.

The origin person is the person who wrote to the first bot. Person ids are the team's: the first bot
names them by its own `[people]` id (else by the manifest's `people`), and every bot uses the same
id (here `lan`). On this bot that person has the roles of `person:lan` in `[roles]`; a person the
team does not know has only `everyone`. A personal bot never sends its owner as a team person. So:

| Situation | Result |
|---|---|
| lan (in `[roles] ops = ["person:lan"]` on ops) asks the assistant to restart a service | ops runs with `everyone` + `ops`: `infra` tools are offered, each change waits for lan's approval |
| a guest asks the assistant the same | ops runs with `everyone` only: `infra` is not offered |
| research tries to call ops | refused: no link from research to ops |
| any caller maps to `admins` | `admins` is never given through a peer |

Peer requests never run chat commands. Knowledge is scoped to the peer room.

**Acting as the person.** The calling bot names the person; this bot cannot check that claim. So by
default a request from another bot never uses the person's own accounts (`credentials = "person"`
servers), their private memory or their daily limits: it counts for `peer:<team>/<caller>`. Turn it
on only when you trust the operators of every member that may call this bot:

```toml
[teams.acme]
act_as_person = true   # the person must be in this bot's [people]; never on a personal bot
```

Approvals this bot asks for during a delegated task are answered through the calling bot, which
relays the person's choice: an honest caller shows them to the person; a dishonest one could answer
itself. Keep `confirm` servers and `act_as_person` for teams whose members you trust.

## 5. How the agent delegates

An agent whose requester is in `delegate_roles` gets three tools:

| Tool | Does |
|---|---|
| `team_directory` | Lists the members it may call: name, team roles, skills, description |
| `delegate` | `to` (a member, `role:<r>` or `skill:<s>`), `task` (text), `wait` (seconds, default 60) |
| `task_status` | The state and result of a task started earlier; while it works, what it is doing |
| `task_cancel` | Stops a task started earlier (the other bot's turn stops) |

`delegate` returns the other bot's answer as text from another bot: data to use, never instructions.
A task not finished within `wait` keeps running; when it ends, its result starts a new turn in the
conversation it came from, so the agent can finish the work. Inside a peer turn `delegate` always
waits, because a bot answers its caller once.

When a tool on the other bot needs approval, the person who started the work sees the prompt in their
own conversation with the first bot, and their answer goes back along the chain. No bot approves
for a person.

## 6. Limits and safety

```toml
[teams.acme]
max_depth = 2          # longest chain of delegations accepted (a → b → c)
max_concurrent = 2     # inbound tasks running at once per caller
chain_tokens = 200000  # tokens a chain started here may spend (finished tasks); 0: no limit
```

- Finished tasks report the tokens they used (their turn and their own delegations); a delegation is
  refused once the chain has none left. It is checked before a task starts: a running turn is never
  cut.
- A turn that fails, is stopped or is over its limit fails its task; the caller sees why.
- Prometheus (`/metrics`): `okbot_delegations_total` and `okbot_delegation_seconds_sum` by team
  and outcome, `okbot_peer_tasks_total` by team and outcome.

- A request carries its chain; a bot refuses one that already passed through it.
- The deadline only shrinks along the chain; a request past it is refused.
- Every turn counts against the origin person's `[limits]` on each bot.
- `okbot peer tasks` lists inbound and outbound tasks; `okbot audit` shows the `delegate` calls. Each
  record carries the trace id shared by every bot of the chain.
- A task that was running when its bot stopped is marked failed, never re-run by itself: delegated
  work may have side effects. A task this bot delegated keeps being followed after a restart, so its
  result still reaches the person who asked.

## 7. Two shapes

**Team with a leader.** The leader is the only member with chat channels. Members are headless and
accept only `role:leader`; members report or ask for help through `--from '*' --to role:leader`.
Members with the same skill share work: `delegate to = "skill:writing"` picks one that is up.

**Main assistant with specialists.** The assistant faces people; research has web tools and no way to
call ops; ops accepts only the assistant, maps to a role that `[[mcp]]` rules allow, and sets
`confirm = "always"` on every server that changes things. Keep ops's roles narrow: anything the web
says can reach research, and research's answers reach the assistant's agent.

## 8. The okbot extension to A2A {#delegation-v1}

URI: `https://github.com/tidusvn05/okbot/blob/main/docs/peers.md#delegation-v1`. Declared in the
Agent Card's `capabilities.extensions` and required for every request. The message's `metadata`
holds:

```json
{"okbot/delegation": {
  "origin": {"bot": "assistant", "conversation": "zalo:shop|-|123|-", "person": "lan", "name": "Lan"},
  "chain": ["assistant"],
  "deadline": "2026-10-24T09:15:00Z",
  "budget": {"turns": 4},
  "trace": "4f1c2a9e0b7d3e61"
}}
```

The request token's `body` claim covers the whole message, so the context is signed with it.

### Request token

```
Authorization: OkbotPeer okp1.<base64url(claims JSON)>.<base64url(Ed25519 signature of "okp1." + claims part)>
```

Claims: `team`, `iss`, `aud`, `iat`, `exp` (≤ `iat` + 300), `jti`, `body` (hex SHA-256 of the body),
`ver` (manifest version). Responses carry `okbot-peer-signature` with the same format, `iss` being the
callee, and `body` the response body's hash.

### Manifest

```toml
[team]
id = "acme"
version = 7
expires = "2027-01-15T00:00:00Z"
description = "Acme operations team"

[members.lead]
key = "ed25519:…"
url = "https://lead.example.com/a2a/acme"
roles = ["leader"]
description = "Team leader; talks to people"

[members.research]
key = "ed25519:…"
box_key = "x25519:…"
relay = "lead"
roles = ["member"]
skills = ["web-research"]

[[links]]
from = ["role:leader"]
to = ["*"]

[people.lan]
name = "Lan"
accounts = ["zalo:shop/123", "discord:main/456"]
```

`okbot team check <file>` validates a manifest and its signature.

### Versions

okbot sends A2A 1.0 (`SendMessage`, `GetTask`, `CancelTask`, `TASK_STATE_*`, parts without `kind`,
headers `A2A-Version: 1.0` and `A2A-Extensions`). It answers 0.3 requests (`message/send`,
`tasks/get`, `tasks/cancel`) in 0.3's shapes, and an `A2A-Version` it does not know with
`VersionNotSupported` (-32009). A finished task's status message carries
`{"okbot/usage": {"tokens": n}}` in its metadata.

## 9. Agents and programs that are not okbots

`[a2a]` makes them the built-in team `external`:

```toml
[a2a]
delegate_roles = ["everyone"]   # who here may make the agent delegate to these agents
confirm = "always"              # the requester approves each delegation

[[a2a.agent]]                   # an A2A agent this bot may delegate to
name = "travel"
url = "https://agent.example.com/a2a"
token_env = "TRAVEL_A2A_TOKEN"  # bearer token it expects, from .env
version = "1.0"                 # or "0.3"
description = "Books trains and hotels"
skills = ["travel"]

[[a2a.client]]                  # a program that may give this bot tasks
name = "n8n"
token_env = "N8N_A2A_TOKEN"     # the bearer token it presents
roles = ["reports"]             # roles its tasks get here (never admins)
person = "lan"                  # the [people] person it acts for (optional)
```

- An agent gets the task text only: never the person, the conversation or the chain.
- A client posts A2A requests (1.0 or 0.3) to `<public_url>/a2a/external` with
  `Authorization: Bearer <token>`. Its task starts a chain of one (whatever context it sends is
  ignored), so it cannot be handed on, and its roles are intersected with its person's: without
  `person`, it has only `everyone`.
