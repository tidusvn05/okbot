# 0014 — Peers and teams: bots that delegate work to each other

Status: Accepted (2026-10-24)

## Context
Operators run several okbots and want them to work together: a team bot whose leader hands work to
members, or a main assistant that asks a research bot, an operations bot or another specialist.
Some bots must be callable by everyone in the group, some only by one bot, and some by none. Bots
may share a VPS or run on different machines, behind NAT included.

Agent CLIs only speak MCP, so an agent must reach another bot through an MCP tool. Between bots,
handing over work is a *task*, not a function call: it runs for minutes, may need to ask a person,
can be cancelled and returns text later. A2A (Agent2Agent, Linux Foundation) models exactly that
(Agent Card, `message/send`, tasks and their states, extensions). MCP would need the same pieces
rebuilt on top of tool calls.

Pairwise configuration does not scale to a mesh: n bots need n² entries that drift apart, and
removing a compromised bot means editing every other one. Rules enforced in more than one place
fail in one of them (ADR 0011).

## Decision

### Names
- The feature is **peers**: bots that delegate to each other. A **team** is a group of peers that
  trust each other's identity. Commands: `okbot peer …` and `okbot team …`. No new protocol name:
  the wire is A2A with one okbot extension.

### Two layers
- **Agent → its own okbot: MCP.** A bot that may delegate offers a built-in server `team` through
  its gateway (ADR 0011) with tools `team_directory`, `delegate` and `task_status`. The calls pass
  `permission::Access` like any other tool, are recorded in `tool_calls`, and their arguments go
  through the same approval rules.
- **okbot → okbot: A2A** (protocol version 0.3.0, the JSON-RPC binding): `message/send`,
  `tasks/get`, `tasks/cancel`, and the Agent Card. The A2A types stay inside the `okbot-peer`
  crate behind okbot's own types, so a new A2A version changes one module.

### Identity
- Each bot has an **Ed25519 key** (`state/peer.key`, 0600, created on first use). Its public key
  (`ed25519:<base64url>`) is the bot's identity; changing URL or machine keeps it.
- Each request carries `Authorization: OkbotPeer okp1.<claims>.<signature>`, signed by the
  caller's key. Claims: `team`, `iss` (caller's member name), `aud` (callee's member name), `iat`,
  `exp` (at most 5 minutes later), `jti` (kept by the callee until `exp` to refuse replays), `body`
  (SHA-256 of the request body) and `ver` (the caller's manifest version). Responses are signed the
  same way (`okbot-peer-signature`), so a relay cannot forge either side. Clocks may differ by 60 s.

### Teams: who is who
- A **team manifest** (`team.toml`) lists the members (name, public key, how to reach them, team
  roles, skills), the people known to the team, and the **links**: which members may call which.
  It has an `id`, a `version` that only grows, and `expires` (at most 90 days ahead).
- A manifest fetched over the network must be **signed by the team key**
  (`team.toml.sig`, Ed25519 over the exact bytes). The team key belongs to the team's owner and
  never needs to be on a bot. A manifest read from a local file needs no signature: whoever can
  write that file already controls the bots that read it. The **local team** (bots of one OS user
  on one machine, `okbot team init local`) is such a file.
- Members refresh the manifest every 10 minutes and when a request names a higher `ver`. A lower
  version is refused (no rollback); an expired one stops all peer traffic of that team until it is
  renewed. **Revoking a member** = removing it and signing a higher version; members follow within
  the refresh interval, and a caller whose `ver` is higher makes the callee refresh at once.
- A member is reached directly (`url`, HTTPS through rustls), through a **relay** (another member
  with a public URL that the NAT-ed member polls over HTTPS: `okbot/relay.poll` held up to 20 s,
  `okbot/relay.reply`, and `okbot/relay.forward` for callers), or locally (`dir`: a bot directory on
  this machine, reached over a Unix socket in `$XDG_RUNTIME_DIR/okbot/` named after the bot
  directory's path (short whatever the path; `OKBOT_PEER_DIR` for bots of several OS users), its key
  read from `<dir>/state/peer.pub`). Polling fits the runtime's request/response HTTP server and keeps every
  request and answer signed end to end.

### Permissions: what is allowed (the callee decides)
- A link only says a call may be *made*. What a request may *do* is decided by the callee's own
  `okbot.toml`, never by a remote document:

  ```toml
  [teams.acme]
  manifest = "https://lead.example.com/team/acme.toml"
  accept = true                       # default false: nobody may call this bot
  roles = { leader = ["ops"] }        # team role of the caller -> local roles
  ```

- A request runs as the account `peer:<team>/<caller>`. Its roles are **`everyone` plus the mapped
  roles of the caller, intersected with the roles the origin person has here** (`person:<id>` in
  `[roles]`; an unknown person has only `everyone`). Person ids are the team's: the first bot names
  the person by its own `[people]` id, else by the manifest's `people`. A delegation can never do
  more than the person could do by asking this bot directly.
- **`admins` is never given through a peer**, whatever the mapping says, and peer requests never run
  chat commands (`/grant`, `/forget`, …).
- On the calling side, `[teams.<id>] delegate_roles` names who in this bot's chats may make the agent
  delegate (default: admins only), and `confirm = "always"` asks the requester first.

### Delegation semantics
- Every request carries the **delegation context** (A2A extension
  `https://github.com/tidusvn05/okbot/blob/main/docs/peers.md#delegation-v1`, signed with the
  request): `origin` (bot, conversation, person), `chain` (members passed through), `deadline`
  (absolute; only shrinks along the chain), `budget` (agent turns left) and `trace` (an id shared by
  every bot's records). A callee refuses a request whose chain contains itself or is longer than its
  `max_depth` (default 2), and one past its deadline.
- An inbound task becomes an **ordinary conversation**: channel `peer:<team>`, room `<caller>/<context>`,
  author the caller. The pipeline handles it like any message (queue, scope, gateway, limits), so no
  second path decides access. Knowledge is scoped to that room; the origin person's own memory is
  readable only when the person is known here.
- **The reply of a peer is data**: the caller's agent receives it as tool output, quoted as text
  from another bot, never as instructions.
- **Tasks are durable**: both sides keep them in SQLite (`peer_tasks`, schema 6). The A2A `messageId`
  is an idempotency key: a retry returns the existing task. A task that was working when its bot
  stopped is marked `failed` ("interrupted") and **never re-run by itself**.
- `delegate` waits up to `wait` (default 60 s). A task not done by then keeps running; its result
  arrives later as a new turn in the origin conversation, so the agent can finish the work. Inside a
  peer turn, `delegate` always waits (a callee answers its caller once).
- **Approvals travel back to the person.** When a tool in the callee needs approval, the callee
  shows its prompt through the peer channel: the task enters `input-required`, the caller shows the
  same prompt to the origin person in the origin conversation, and sends the answer back. No bot
  ever answers an approval for a person.
- **Results are pulled.** The caller polls `tasks/get` (or keeps the request open), so only the callee
  must be reachable; push notifications are not used.
- Usage of every turn in the chain counts against the origin person's `[limits]` on each bot, and
  `[teams.<id>] max_concurrent` caps inbound tasks per caller.

## Consequences
- Adding or removing a bot is one manifest change, signed once; no bot is edited.
- The hard rules hold: no policy gives an agent a shell or files, delegated or not; a remote team
  can never widen the permissions of `okbot.toml`; chat text and peer replies are data.
- Bot messages in shared chat rooms stay ignored (`answer_bots = false`): bots coordinate only through
  the protocol, which carries identity and limits that chat does not.
- Every delegation costs an agent turn on each bot of the chain; `max_depth` and `budget` bound it.
- A relay reads the payloads it forwards (signatures stop forgery, not reading). End-to-end encryption
  for relays can come later as another extension.
- The team key is the root of a team's trust. It is rotated by a manifest signed by both the old and
  the new key (one `.sig` line each) while members move their pinned `team_key`.
- A published manifest is readable by whoever can fetch it (members, URLs, `people`): it is served by
  the operator's own HTTPS server where only the team reaches it; bots keep accounts in `[people]`.
- A2A changes (version 1.0 renames methods and the card's interfaces) are absorbed in `okbot-peer`;
  the agent's tools and the configuration do not change.
- New crate `okbot-peer` (an adapter: `Channel` for inbound tasks, the `Peers` port for outbound);
  `okbot-core` gains the `Peers` port and its types.

## Amendments (2026-10-24, P28)
- **A2A 1.0 first.** okbot sends A2A 1.0 (`SendMessage`, `GetTask`, `CancelTask`, `TASK_STATE_*`,
  parts without `kind`, `A2A-Version: 1.0`, `A2A-Extensions`) and answers 0.3 requests in 0.3's
  shapes; a version it does not know gets `VersionNotSupported` (-32009). The Agent Card lists both
  interfaces in `supportedInterfaces`.
- **Sealed through relays.** Each bot also has an X25519 key (`state/peer.box`, `box_key` in the
  manifest). What passes through a relay is sealed for its recipient (a fresh X25519 key per
  message, HKDF-SHA256, ChaCha20-Poly1305); the okp1 tokens still sign the plain bodies. A member
  reached through a relay must have a `box_key`, so a relay forwards bytes it cannot read.
- **Agents and programs outside okbot** form the built-in team `external` (`[a2a]`):
  `[[a2a.agent]]` are agents this bot may delegate to (bearer token, the version they speak; they
  get the task text only, never the person or the chain); `[[a2a.client]]` may give this bot tasks
  at `/a2a/external` with a bearer token. A client's task starts a chain of one (its own context is
  ignored), acts for the client's `person` if configured, and gets the client's roles intersected
  with that person's — so without a person, only `everyone`.
- **Durable to the end.** Outbound tasks record who asked; after a restart the runtime follows the
  unfinished ones again. A peer turn that fails, stops or is over its limit fails its task
  (`Peers::fail`) rather than completing it with a chat apology. `CancelTask` stops the callee's
  turn; the agent has `task_cancel`.
- **Token budget.** `[teams.<id>] chain_tokens` caps the tokens a chain started here may spend.
  Finished tasks report what they used (`okbot/usage` in the status message's metadata: their turn
  and their own delegations); a delegation is refused once the chain has none left. It is checked
  before a task starts; a running turn is never cut.
- **Progress, not streaming.** A working task carries the latest progress of its turn (the tool in
  use, or the text being written) in `GetTask`. `SendStreamingMessage` is not offered: the runtime's
  HTTP server answers each request once, and callers already poll.
- **Setup and metrics.** `okbot team join` writes `[teams.<team>]` (and adds a local bot to the
  local manifest); `okbot init` offers it. Prometheus gains `okbot_delegations_total`,
  `okbot_delegation_seconds_sum` and `okbot_peer_tasks_total`.
- **The origin person is a claim (security review, 2026-10-24).** The caller names the person in the
  delegation context and the callee cannot check it. So a task from another bot acts as that person
  (their own `credentials = "person"` accounts, private memory, daily limits) only when the callee
  sets `[teams.<id>] act_as_person = true` and the person is in its own `[people]`; never on a
  personal bot, whose owner is never lent. Otherwise usage counts for `peer:<team>/<caller>` and the
  task is not personal. The role intersection still uses the claim, which can only lower the mapped
  roles; the mapping in `okbot.toml` stays the ceiling. Approvals a callee asks for are answered by
  its caller: "no bot answers for a person" holds when every member's operator is trusted, which is
  what `act_as_person` asserts. `[[a2a.client]] person` is set by the callee's own operator and is
  trusted as such.
