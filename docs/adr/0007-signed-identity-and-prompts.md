# 0007 — Signed run identity and prompts for your own MCP servers

Status: Accepted (2026-10-17)

## Context
`[[mcp]]` lets the agent call tools of servers the operator runs. Such a server only saw the agent: one
shared bearer secret, no idea which person asked or where. It could not authorize per person, and an
action that needs a person's decision or input (approve a deployment, type a key) had to pass through
the agent's arguments: the model could press "yes" itself, and secrets ended up in the transcript.

## Decision
- **Signed run identity.** `[[mcp]] auth = "signed"`: instead of the secret, each turn's agent gets
  `okbot1.<base64url(claims)>.<base64url(HMAC-SHA256(secret, "okbot1." + base64url(claims)))>`.
  Claims: `bot`, `mcp`, `user` (`platform:instance/user-id`), `name`, `conversation`, `admin`,
  `direct`, `interact` (okbot's prompt endpoint), `exp` (turn timeout + 60 s), `jti`. The server verifies
  it with the shared secret. The agent cannot widen it.
- **Prompts.** With that token, the server calls `POST /interact` on okbot's HTTP listener
  (a loopback port is opened when no `[http] listen` is set). The body names text, 1–5 buttons, an
  optional form per button (Discord: a modal of up to five text fields), who may answer
  (`requester` | `admins` | `anyone`), an expiry and a callback URL on the server's own origin. okbot
  shows the prompt in the token's conversation only, and accepts only tokens it issued (their `jti`
  is kept in memory until `exp`): the server knows the secret and could otherwise sign claims for any
  conversation.
- **Answers bypass the agent.** A press (and form) arrives through `InboundSink::answer`, never as an
  `InboundEvent`. okbot POSTs it to the callback with `x-okbot-timestamp` and
  `x-okbot-signature = base64url(HMAC-SHA256(secret, "<timestamp>.<body>"))`, and replaces the
  prompt's buttons with the callback's `text` (or keeps them with `keep_open`). Values are not logged,
  stored, added to the history or shown to the agent.
- Core gains `Prompt` types and two `Channel` methods with `Unsupported` defaults; Discord implements
  them (buttons, modals, `INTERACTION_CREATE`). Other platforms answer `501` until they do.

## Consequences
- A server can enforce roles on every tool call and on every answer (the clicker is checked by okbot
  against `answer_by`, and the server sees who answered).
- Secrets a person types go platform → okbot → server. They still cross the chat platform (TLS, not
  end-to-end), so prompts suit revocable credentials and one-time codes, not passwords.
- Open prompts live in memory: after a restart their buttons answer "expired".
- okbot-runtime now has an HTTP client (reqwest, ADR 0004) for callbacks.
