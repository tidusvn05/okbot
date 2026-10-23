# 0012 — OAuth for `[[mcp]]` servers, per person or shared

Status: Accepted (2026-10-23)

## Context
People want their assistant to read and write the services they use every day: Gmail, Drive, Sheets,
Calendar, Notion. These services publish MCP servers, or have well-maintained community ones, so
okbot does not need to wrap their APIs: one connector per service would be a product of its own,
and every API change would break it. What `[[mcp]]` cannot do yet is log in. It sends a fixed
secret (`token_env`) or a token signed with one (ADR 0007), while these servers want OAuth:
Notion's hosted server (`mcp.notion.com`) accepts nothing else, and the self-hosted Google Workspace
server in multi-user mode answers 401 until the client completes the MCP authorization flow
(protected resource metadata, authorization server metadata, dynamic client registration, PKCE).

Two more facts shape the design. The credentials are personal: a token reads one person's mail, so
it must only serve that person, and only where nobody else reads the answers. And hosted MCP
gateways (Composio, Zapier, Pipedream) or self-hosted ones (agentgateway, Bifrost) could hold the
tokens, but each is a second place that decides who may call what; ADR 0011 made okbot's gateway
the single place on purpose.

## Decision
- **`auth = "oauth"` on an HTTP `[[mcp]]` server.** okbot is the MCP client in the specification's
  authorization flow: it reads the server's `WWW-Authenticate` challenge and protected resource
  metadata, then the authorization server's metadata (RFC 8414, OpenID discovery as fallback),
  registers itself with dynamic client registration (RFC 7591) unless `[mcp.oauth] client_id` is
  set, and runs the authorization code flow with PKCE (S256) and the `resource` parameter
  (RFC 8707). Client ID metadata documents are not used yet: they need a public HTTPS document.
- **`credentials = "person"` (default) or `"shared"`.** Person credentials belong to the person who
  connected them (`person:<id>` for linked people and the owner of a personal bot, else the
  account) and are offered **only in that person's turns in a direct chat or a `personal` room**,
  never in a group, whatever `rooms` says. Shared credentials are connected once by an
  administrator and serve everyone the server's `roles` and `rooms` allow, like a bearer secret.
- **Connecting.** `/connect <server>` in a direct chat replies with a one-time link (ten minutes,
  random `state`, PKCE verifier kept in memory); the provider sends the browser to
  `[http] public_url` + `/oauth/callback`, or to the loopback `[http] listen` address when no
  public URL is set. The callback page then shows a six-digit code, and the account is kept only
  when the same person sends `/connect <code>` in the chat that asked for the link (five wrong codes
  drop it), so a link opened by someone else connects nothing. `okbot mcp login <server> [--as
  <person or account>]` does the same from the terminal and accepts the redirected address pasted
  back, so a bot without a public URL can still connect. `/disconnect`, `/connections`, `okbot mcp logout|status` complete the set. Shared
  credentials can only be connected by administrators.
- **Storage.** Tokens and client registrations are operational data in SQLite (`sealed_secrets`),
  sealed with ChaCha20-Poly1305 under a key in `state/secrets.key` (created with mode 0600,
  never in the database), with the server and subject as associated data, so a copied database
  reveals nothing and a sealed value cannot be moved to another person.
- **Use.** The gateway forwards with the person's access token, refreshed ahead of expiry under a
  per-credential lock (providers rotate refresh tokens; two refreshes would revoke them). Only an
  `invalid_grant` answer forgets a token, and only when no other process rotated it meanwhile. The agent
  still only holds its turn token. A server the person has not connected is not offered, and the
  prompt names it so the agent can say `/connect <server>`. `[[command]]` and automations
  (ADR 0013) use the same credentials.
- **Presets.** `okbot mcp add <preset>` writes a reviewed `[[mcp]]` block: `notion` (hosted),
  `google-workspace` (the self-hosted `workspace-mcp` server in OAuth 2.1 mode) and `linear`
  (hosted). Presets set `confirm = "destructive"`, so sending mail or sharing a file waits for the
  person's approval.

## Consequences
- Any MCP server that implements the specification's authorization works without code in okbot;
  services without one stay reachable through `token_env` or a local `command` server.
- A connect link binds whatever account the browser logs into to the person who asked for the link.
  Links are only shown in direct chats, and the code step means a phished link connects nothing
  unless its victim also hands over the code the page shows.
- Without `[http] listen` a bot cannot receive callbacks; `okbot mcp login` with a pasted address
  remains. Providers that refuse loopback redirect URIs need `[http] public_url`.
- Losing `state/secrets.key` loses the stored credentials (people connect again), not anything else.
- Schema version 5 adds `sealed_secrets` (and the automation tables of ADR 0013).
