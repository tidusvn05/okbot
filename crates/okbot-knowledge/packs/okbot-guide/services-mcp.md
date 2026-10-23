---
type: Guide
title: Connecting services through MCP
description: How okbot connects Gmail, Drive, Notion, GitHub and other services through MCP servers, the three kinds of server, OAuth with /connect, presets, and what does not work yet.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [mcp, kết nối dịch vụ, tích hợp, integration, connect, /connect, oauth, okbot mcp add, kết nối notion, kết nối gmail]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Connecting services through MCP

okbot does not have built-in integrations for each service. Services publish **MCP servers** (Model
Context Protocol) that offer tools, and okbot connects to them. The agent reaches every server only
through **okbot's gateway**, which applies roles, rooms, tool lists, approvals and the audit log
([Permissions](permissions.md)). The agent never sees a token.

Do not put a separate MCP gateway (Composio, Zapier, Pipedream) between okbot and the services:
it would be a second place that decides who may call what, and it would hold everyone's tokens.

## Three kinds of server

| Kind | `[[mcp]]` settings | For |
|---|---|---|
| HTTP with OAuth | `url` + `auth = "oauth"` | services where each person logs in: Notion, Linear, Google Workspace, GitHub |
| HTTP with a fixed token | `url` + `token_env` (sent as `Authorization: Bearer …`); `auth = "signed"` sends a signed run identity instead | your own servers, API-key services |
| Local program (stdio) | `command = [...]` + `env = [...]` | servers published as `npx`, `uvx` or binary packages |

## OAuth: each person connects their own account

```sh
okbot mcp add notion          # presets: notion, google-workspace, linear
okbot restart
```

The preset writes:

```toml
[[mcp]]
name = "notion"
url = "https://mcp.notion.com/mcp"
auth = "oauth"
confirm = "destructive"       # sending, sharing and deleting wait for Allow
```

Browsers come back to okbot after login, so set where okbot listens:

```toml
[http]
listen = "127.0.0.1:8787"
public_url = "https://bot.example.com"   # when people use browsers on other machines
```

Then, in a **direct chat** with the bot:

```
/connections          which accounts you can connect, and which are connected
/connect notion       a one-time link (10 minutes); log in and allow access
/connect 123456       the six-digit code the page shows, to finish
/disconnect notion
```

From the terminal (no public URL needed): `okbot mcp login notion [--as person:lan]`, `okbot mcp
list`, `okbot mcp logout notion --as person:lan`.

- `credentials = "person"` (default): the account is used **only in its owner's direct chats and
  `personal` rooms**, never in a group.
- `credentials = "shared"`: an admin connects one account (a team workspace) that everyone the
  server's `roles` allow can use.
- Servers without automatic client registration need `[mcp.oauth] client_id`, `client_secret_env`
  and `scopes`; register okbot there with both addresses `okbot doctor` shows.
- Tokens are sealed in the database with `state/secrets.key`; back both up together.

## Fixed token and local servers

```toml
[[mcp]]
name = "github"
command = ["github-mcp-server", "stdio"]
env = ["GITHUB_PERSONAL_ACCESS_TOKEN"]    # from .env, by name
tools = ["search_*", "get_*", "list_*"]   # read-only tools only
```

A stdio server gets an empty environment except `PATH`, its own empty `HOME` and the variables in
`env`, and stops when the session ends. It has no per-person identity: everyone allowed uses the
same token.

## Tool annotations matter

MCP servers can mark tools as read-only or destructive. okbot treats a tool without annotations as
destructive, so with `confirm = "destructive"` every call of such a server waits for **Allow**. For
those servers, list only the tools you need in `tools = [...]`.

## What does not work yet

| Server | Workaround |
|---|---|
| Only the old HTTP+SSE transport (`/sse`) | run its stdio version, or put `agentgateway` (Rust) in front as a protocol bridge |
| An API key in a custom header (`X-API-Key`) | run its stdio version with the key in `env`, or bridge it through `agentgateway` |
| A stdio server that needs OAuth | it keeps its own token: use it as a shared account |
| Providers that need `access_type=offline` (Google's own MCP servers) | use a self-hosted Google server ([Google Workspace](google-workspace.md)) |

Next: [Google Workspace](google-workspace.md), [Popular services](popular-services.md),
[Your own tools and commands](own-tools.md).

Never add a server that offers a shell or file access: the agent would get them.
