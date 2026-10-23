---
type: Guide
title: Popular services
description: Which MCP server to use with okbot for Notion, GitHub, Jira, Slack, Microsoft 365, databases, mail, search and other common services, with Rust options first and the okbot settings for each.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [dịch vụ phổ biến, notion, github, gitlab, jira, confluence, linear, trello, asana, clickup, todoist, airtable, microsoft 365, outlook, onedrive, teams, slack, postgres, mysql, database, imap, email, brave, tavily, exa, sentry, stripe, cloudflare, home assistant, hubspot, dropbox, rust mcp]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Popular services

Checked in October 2026. Prefer the vendor's hosted server for SaaS: it is maintained by the
service and needs nothing to run. Self-hosted picks are Rust where a maintained Rust server exists.
How each kind is configured is in [Connecting services through MCP](services-mcp.md).

**Settings** column: `oauth` = `url` + `auth = "oauth"` (each person `/connect`s); `oauth (client)`
= the same plus `[mcp.oauth] client_id` and `client_secret_env` from an app you register at the
service; `token` = `url` + `token_env` (an API key sent as Bearer, one shared account); `command` =
a local stdio program with its key in `env`.

## Work and projects

| Service | Server | Settings | Notes |
|---|---|---|---|
| Notion | `https://mcp.notion.com/mcp` (vendor) | `oauth` | preset `okbot mcp add notion` |
| Linear | `https://mcp.linear.app/mcp` (vendor) | `oauth` | preset `okbot mcp add linear` |
| Jira, Confluence (cloud) | `https://mcp.atlassian.com/v1/mcp` (vendor) | `oauth` | |
| Jira, Confluence (Data Center) | `sooperset/mcp-atlassian` (Python) | `command` | API token in `env`; annotated |
| Trello | `https://mcp.trello.com/v1` (vendor) | `oauth` | one workspace per connection |
| Asana | `https://mcp.asana.com/v2/mcp` (vendor) | `oauth (client)` | register an MCP app at Asana |
| ClickUp | `https://mcp.clickup.com/mcp` (vendor) | `oauth` | not tested with okbot |
| Todoist | `https://ai.todoist.net/mcp` (vendor) | `oauth` | annotated |
| Airtable | `https://mcp.airtable.com/mcp` (vendor) | `token` | personal access token |
| HubSpot | `https://mcp.hubspot.com` (vendor) | `oauth (client)` | create an MCP auth app at HubSpot |

## Google and Microsoft

| Service | Server | Settings | Notes |
|---|---|---|---|
| Gmail, Drive, Sheets, Calendar | `google-mcp-rs` (**Rust**) or `workspace-mcp` | `oauth` | see [Google Workspace](google-workspace.md) |
| Outlook, OneDrive, Teams, Microsoft 365 | `Softeria/ms-365-mcp-server` (TypeScript), run with `--http` | `oauth` | dynamic registration built in; `--read-only` available; annotated |
| Dropbox | `https://mcp.dropbox.com/mcp` (vendor, beta) | `oauth (client)` | register a Dropbox app |

## Developers and operations

| Service | Server | Settings | Notes |
|---|---|---|---|
| GitHub | `https://api.githubcopilot.com/mcp/` or `github/github-mcp-server` (Go) | `token`, or `command = ["github-mcp-server", "stdio"]` | personal access token; well annotated |
| GitLab | built into GitLab: `https://<host>/api/v4/mcp` | `oauth` | may need a paid tier with Duo |
| Sentry | `https://mcp.sentry.dev/mcp` (vendor) | `oauth` | annotated |
| Cloudflare | `https://mcp.cloudflare.com/mcp` (vendor) | `oauth` | |
| Stripe | `https://mcp.stripe.com` (vendor) | `token` | a **restricted** key, never the secret key |
| Home Assistant | built in: `https://<ha>/api/mcp` | `oauth (client)` with `client_id` = the bot's public URL | not tested with okbot |

## Data

| Service | Server | Settings | Notes |
|---|---|---|---|
| Postgres, MySQL, SQLite (safest) | `googleapis/mcp-toolbox` (Go) | `command` | exposes only the queries you define in YAML; annotated |
| Postgres, MySQL, MariaDB, SQLite | `haymon-ai/dbmcp` (**Rust**) | `command` | read-only mode, PII redaction; young project |
| Postgres | `crystaldba/postgres-mcp` (Python) | `command` | restricted mode |
| A GraphQL API | `apollographql/apollo-mcp-server` (**Rust**) | `command` or `token` | turns GraphQL operations into tools |

Give database servers a **read-only** database user. A query tool that can write is a destructive
tool.

## Mail, calendar, search

| Service | Server | Settings | Notes |
|---|---|---|---|
| Any mailbox (IMAP/SMTP, Exchange) | `tecnologicachile/mail-mcp` (**Rust**) | `command` | use stdio (its HTTP mode has no auth); no annotations: narrow `tools` |
| CalDAV calendars | `dominik1001/caldav-mcp` (TypeScript) | `command` | no annotations |
| Brave Search | `brave/brave-search-mcp-server` (vendor) | `command` | `BRAVE_API_KEY` |
| Tavily | `https://mcp.tavily.com/mcp` (vendor) | `token` | |
| Exa | `https://mcp.exa.ai/mcp` (vendor) | `token` or `oauth` | |
| SearXNG (self-hosted search) | `ihor-sokoliuk/mcp-searxng` | `command` | |

The agent CLIs already search and fetch the web themselves in rooms with `tool_policy = "web"`; a
search server is only needed for a specific engine.

## Chat platforms as data

| Service | Server | Settings | Notes |
|---|---|---|---|
| Slack (read channels, threads) | `korotovsky/slack-mcp-server` (Go) | `command` | an `xoxp` user token; avoid its browser-token mode. The official `mcp.slack.com` needs `oauth (client)` with your own Slack app |
| Telegram (your own chats) | `chigwell/telegram-mcp` (Python) | `command` | logs in as your user account |
| Zalo, WhatsApp | none worth using | | Zalo works as a chat platform of okbot, not as data |

## Rust bridge for servers okbot cannot reach directly

`agentgateway` (Rust, Linux Foundation) can sit in front of a server that only speaks the old SSE
transport or wants its key in a custom header (`X-API-Key`). okbot then talks streamable HTTP to
agentgateway with `token_env`. Use it only as a protocol bridge: okbot's own gateway stays the place
for permissions.

## Avoid

- **File and notes servers** (filesystem, Obsidian vaults): they give the agent file access, which
  okbot never allows. Save documents into the knowledge base instead ([Knowledge base](knowledge-base.md)).
- **Browser automation** (Playwright and others): the agent could reach internal addresses.
- Servers that are archived, unmaintained, or under non-commercial or AGPL licences when you
  redistribute.
