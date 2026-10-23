# Connecting Gmail, Drive, Sheets, Notion and other services

okbot reaches everyday services through their MCP servers. You do not write integration code. Each
person connects their own account from chat, and okbot keeps the tokens
([ADR 0012](adr/0012-oauth-for-mcp-servers.md)). The agent never sees a token. It calls the
services through okbot's gateway, which applies the same roles, rooms, tool lists, approvals and
audit as for any `[[mcp]]` server ([ADR 0011](adr/0011-mcp-gateway-and-roles.md)).

| Service | Server | Preset |
|---|---|---|
| Notion | hosted, `https://mcp.notion.com/mcp` | `okbot mcp add notion` |
| Gmail, Drive, Docs, Sheets, Calendar, Chat | [`workspace-mcp`](https://github.com/taylorwilsdon/google_workspace_mcp), self-hosted | `okbot mcp add google-workspace` |
| Linear | hosted, `https://mcp.linear.app/mcp` | `okbot mcp add linear` |
| Any MCP server with OAuth | its URL | `[[mcp]] auth = "oauth"` by hand |

Google announced its own Workspace MCP servers in May 2026 as a developer preview for enrolled
organizations. When they are open to you, point an `auth = "oauth"` server at them.

## 1. Let browsers come back to the bot

Connecting an account sends the person's browser to the provider and back to okbot's
`/oauth/callback`. Set where that is:

```toml
[http]
listen = "127.0.0.1:8787"              # the bot's HTTP server
public_url = "https://bot.example.com" # the same server behind your HTTPS reverse proxy
```

With only `listen` set, the callback is `http://127.0.0.1:8787/oauth/callback`. That works when the
browser runs on the bot's machine (a personal bot on your laptop). With neither set, connect accounts
from the terminal with `okbot mcp login` (step 3).

## 2. Add the server

```sh
okbot mcp add notion           # writes a reviewed [[mcp]] block into okbot.toml
okbot restart
```

The block it writes:

```toml
[[mcp]]
name = "notion"
url = "https://mcp.notion.com/mcp"
auth = "oauth"
# Each person connects their own account with /connect in a direct chat.
confirm = "destructive"        # sending, sharing and deleting wait for approval
```

- `credentials = "person"` (the default): each person connects their own account. Their account is
  used **only in their direct chats and in rooms marked `personal`**. It is never used in a group,
  whatever `rooms` says, because everyone in a group reads the answers.
- `credentials = "shared"`: an administrator connects one account (a team's Notion workspace, a
  shared inbox). Everyone the server's `roles` and `rooms` allow can use it, as with a bearer token.
- `[mcp.oauth] client_id`, `client_secret_env` and `scopes` are for providers without dynamic client
  registration. Register okbot there with both redirect addresses shown by `okbot doctor` (the
  bot's and `http://127.0.0.1:47823/oauth/callback`).

## 3. Connect accounts

In a **direct chat** with the bot:

```
/connections          which accounts you can connect, and which are connected
/connect notion       a one-time link (10 minutes); open it and allow access
/disconnect notion    okbot forgets the account (remove it at the provider too)
```

The page at the end shows a six-digit code: send `/connect <code>` in the same chat to finish, and
the bot answers "✓ Connected **notion**." Until the code arrives nothing is kept, so a link someone
else opened (phishing) connects nothing unless they also hand over the code. Five wrong codes drop
the waiting account.

From the terminal (no public URL needed):

```sh
okbot mcp login notion                      # personal bot: the owner's account
okbot mcp login google --as person:lan      # team bot: say whose (or an account id from /whoami)
okbot mcp list                              # servers and connected accounts
okbot mcp logout notion --as person:lan
```

`login` prints a link. When the browser ends on `127.0.0.1:47823` and the page does not load (the
bot runs on a server), copy that page's address and paste it into the terminal. On the bot's own
machine, it completes by itself.

When the person asks for a service that is not connected, the agent is told so and suggests
`/connect <name>`. `[[command]]`s on these servers use the person's account the same way.

## Google Workspace (Gmail, Drive, Docs, Sheets, Calendar)

`workspace-mcp` runs next to the bot and talks to Google for it. In OAuth 2.1 mode, okbot logs in
through it like any MCP client, and every person connects their own Google account.

1. In Google Cloud Console, create a project, enable the APIs you want (Gmail, Drive, Docs, Sheets,
   Calendar), configure the OAuth consent screen, and create an OAuth client of type **Web
   application** with the redirect URI `http://127.0.0.1:8000/oauth2callback` (or your
   `WORKSPACE_EXTERNAL_URL` + `/oauth2callback`). None of this costs money or needs a billing
   account: the project only identifies the app to Google, and these APIs have quotas, not fees.
   Pick the consent screen's audience with care (see below).
2. Run the server with that client, next to the bot:

   ```sh
   GOOGLE_OAUTH_CLIENT_ID=… GOOGLE_OAUTH_CLIENT_SECRET=… MCP_ENABLE_OAUTH21=true \
   WORKSPACE_MCP_PORT=8000 uvx workspace-mcp --transport streamable-http --tool-tier core
   ```

   `--read-only` limits it to reading. `--tools gmail drive sheets` picks services. Keep it on
   loopback, or behind HTTPS if it runs elsewhere. Its README has the details of each setting.
3. `okbot mcp add google-workspace` (its name in okbot is `google`), then `okbot restart`, then
   `/connect google`.

### Choosing a Google Workspace server

okbot works with any Google Workspace MCP server that implements the MCP authorization spec. Two
were checked against okbot in October 2026:

| | [`workspace-mcp`](https://github.com/taylorwilsdon/google_workspace_mcp) (Python) | [`google-mcp-rs`](https://github.com/matpb/google-mcp-rs) (Rust) |
|---|---|---|
| Maturity | ~3,400 stars, ~100 contributors, frequent releases | 1 star, 2 contributors, v1.0.2 |
| Services | Gmail, Drive, Docs, Sheets, Slides, Forms, Calendar, Tasks, Chat, Contacts, Apps Script, Search (~120 tools) | Gmail, Drive, Docs, Sheets, Calendar, Tasks, Contacts, Search Console (~113 tools); no Slides, Forms or Chat |
| MCP authorization | Protected resource and server metadata, dynamic registration, PKCE | The same, plus RFC 8707 audience checks, its own consent page and a Google account allowlist |
| Tool annotations | Yes: reads are `readOnlyHint`, sends and deletes `destructiveHint` | None, so okbot treats every tool as destructive |
| Narrowing | `--tool-tier`, `--tools`, `--read-only`, `--disabled-tools` | `ENABLED_DOMAINS` per service |
| Token for okbot | Refreshable | 30 days, no refresh: people `/connect` again monthly |
| Footprint | Python runtime via `uvx`, ~100 MB resident | One static binary or distroless image, ~10 MB resident |

**Rust is not faster where it matters here.** In published MCP benchmarks a Rust server adds
about 0.4 ms per call and a Python one about 2 ms, while each Gmail or Drive call spends
100–500 ms at Google. Rust clearly wins on memory, startup and deployment (no Python runtime),
which counts when several bots share a small VPS.

**Recommendation: `workspace-mcp` (the preset).** Its tool annotations let `confirm =
"destructive"` ask only for sends, shares and deletes. With `google-mcp-rs`, every call waits for
**Allow**, including searches, and every call in an automation's turn asks too. Choose
`google-mcp-rs` when memory or a Python-free host matters more, and then narrow its tools:

```toml
[[mcp]]
name = "google"
url = "http://127.0.0.1:8433/mcp"   # google-mcp-rs with MCP_HOST=127.0.0.1, BASE_URL=http://127.0.0.1:8433
auth = "oauth"
tools = ["gmail_search_*", "gmail_get_*", "gmail_list_*", "calendar_list_*", "calendar_get_*",
         "calendar_freebusy", "drive_list_files", "drive_get_file", "sheets_get*",
         "sheets_batch_get_values"]
confirm = "never"                   # only reading tools are offered
```

Hosted servers (Notion, Linear) run at the provider, so their language does not matter.

### The consent screen and the seven-day logout

Google limits how long an app's access lasts by the consent screen's audience and publishing status:

| Your accounts | Audience | Status | Result |
|---|---|---|---|
| Google Workspace (your own domain) | **Internal** | — | Best: no review, no warning, tokens last. Only accounts of your organization can connect. |
| Personal `@gmail.com` | External | **In production** (Publish app, no verification submitted) | Tokens last. The login shows "Google hasn't verified this app": choose *Advanced → Go to …*. Up to 100 users. |
| Personal `@gmail.com` | External | Testing | Works for the test users you list, but **refresh tokens expire after 7 days**: everyone has to `/connect google` again every week. |

Verification by Google (and, for Gmail scopes, a paid security assessment) is only needed for an
app published to the public beyond 100 users, never for a personal or team bot.

When a token expires or is revoked, Google answers `invalid_grant`. okbot forgets the token, and the
agent tells the person to `/connect google` again; nothing else breaks.

Gmail is where prompt injection arrives: anyone can mail your assistant instructions. The preset's
`confirm = "destructive"` makes every send, share or delete wait for your **Allow**. Consider
`tool_policy = "mcp-only"` for rooms where mail is read, and `tools = [...]` with only the tools you
use.

## Security notes

- Tokens and client registrations are stored in the bot's database, sealed with
  ChaCha20-Poly1305 under `state/secrets.key` (created with mode 0600). Back up both together. If
  you lose the key, people have to connect again.
- Tokens are refreshed shortly before they expire. When a provider answers `invalid_grant` (revoked
  or expired access), okbot forgets the token and the person connects again; other failures keep it.
- `okbot audit` lists every call, with whom it was for. Arguments and results are never stored.
- `okbot doctor` shows the callback address and which accounts are connected.
