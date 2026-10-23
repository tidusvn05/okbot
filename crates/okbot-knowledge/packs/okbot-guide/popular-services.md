---
type: Guide
title: Popular services
description: Which MCP server to use with okbot for Notion, GitHub, Jira, Slack, Microsoft 365, databases, CI, search and other common services, with the settings that keep each one safe.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-24
aliases: [dịch vụ phổ biến, notion, github, gitlab, jira, confluence, linear, trello, asana, clickup, todoist, airtable, microsoft 365, outlook, onedrive, teams, slack, postgres, mysql, database, sentry, stripe, cloudflare, home assistant, hubspot, dropbox, brave, tavily, exa, searxng, sonarqube, buildkite, circleci, context7, ci, lập trình, phát triển phần mềm, rust mcp]
generated: {by: okbot-guide-pack, at: "2026-10-24"}
---

# Popular services

Checked in October 2026. Vendor-hosted servers are maintained by the service itself and need nothing
to run. Every self-hosted server below was reviewed for security (published advisories, code,
read-only modes); servers that did not pass are not listed. Use the settings in the **Safe use**
column: they are part of the recommendation. How each kind is configured is in
[Connecting services through MCP](services-mcp.md); servers for SSH, Kubernetes, Grafana and
Ansible are in [Servers and infrastructure](servers.md).

**Settings** column: `oauth` = `url` + `auth = "oauth"` (each person `/connect`s); `oauth (client)`
= the same plus `[mcp.oauth] client_id` and `client_secret_env` from an app you register at the
service; `token` = `url` + `token_env` (an API key sent as Bearer, one shared account); `command` =
a local stdio program. A `command` server receives only the variables named in `env`, so settings
such as `READ_ONLY_MODE=true` go into `.env` and their names into `env = [...]`.

## Work and projects

| Service | Server | Settings | Safe use |
|---|---|---|---|
| Notion | `https://mcp.notion.com/mcp` (vendor) | `oauth` | preset `okbot mcp add notion` |
| Linear | `https://mcp.linear.app/mcp` (vendor) | `oauth` | preset `okbot mcp add linear` |
| Jira, Confluence (cloud) | `https://mcp.atlassian.com/v1/mcp` (vendor) | `oauth` | |
| Jira, Confluence (Data Center) | `sooperset/mcp-atlassian` (Python), version 0.23.1 or later | `command` | `READ_ONLY_MODE=true`; `ENABLED_TOOLS` listing the read tools you need; tokens in `env`, never as arguments; a low-privilege account |
| Trello | `https://mcp.trello.com/v1` (vendor) | `oauth` | one workspace per connection |
| Asana | `https://mcp.asana.com/v2/mcp` (vendor) | `oauth (client)` | register an MCP app at Asana |
| ClickUp | `https://mcp.clickup.com/mcp` (vendor) | `oauth` | not tested with okbot |
| Todoist | `https://ai.todoist.net/mcp` (vendor) | `oauth` | |
| Airtable | `https://mcp.airtable.com/mcp` (vendor) | `token` | personal access token |
| HubSpot | `https://mcp.hubspot.com` (vendor) | `oauth (client)` | create an MCP auth app at HubSpot |

## Google and Microsoft

| Service | Server | Settings | Safe use |
|---|---|---|---|
| Gmail, Drive, Sheets, Calendar | `google-mcp-rs` (**Rust**) or `workspace-mcp` | `oauth` | see [Google Workspace](google-workspace.md) |
| Outlook, OneDrive, Teams, Microsoft 365 | `Softeria/ms-365-mcp-server` (TypeScript) | `oauth` | run with `--http 127.0.0.1:<port> --read-only` and `--preset` or `--enabled-tools` for the services you use; never add `--http-local-file-tools`, `--trust-proxy-auth` or `--enable-auth-tools` |
| Dropbox | `https://mcp.dropbox.com/mcp` (vendor, beta) | `oauth (client)` | register a Dropbox app |

## Software development

| Service | Server | Settings | Safe use |
|---|---|---|---|
| GitHub | `https://api.githubcopilot.com/mcp/readonly` (vendor) | `token` | a fine-grained token with read permissions; one toolset: `/mcp/x/<toolset>/readonly` (`issues`, `pull_requests`, `actions`, …) |
| GitHub (self-run) | `github/github-mcp-server` (Go): `command = ["github-mcp-server", "stdio"]` | `command` | `GITHUB_READ_ONLY=1`, `GITHUB_TOOLSETS=repos,issues,pull_requests,actions`, `GITHUB_LOCKDOWN_MODE=1` |
| GitLab | built into GitLab: `https://<host>/api/v4/mcp` | `oauth` | may need a paid tier with Duo |
| Sentry | `https://mcp.sentry.dev/mcp` (vendor) | `oauth` | |
| SonarQube, SonarQube Cloud | `sonarsource/sonarqube-mcp` (Docker image, source-available licence) | `command` | `SONARQUBE_READ_ONLY=true`, `TELEMETRY_DISABLED=true` (it is on by default), `SONARQUBE_TOOLSETS=issues,quality-gates,measures,security-hotspots,coverage`; mount no workspace |
| Buildkite | `https://mcp.buildkite.com/direct/readonly` (vendor) | `token` | a token with read scopes only |
| CircleCI | `https://mcp.circleci.com/v1/mcp` (vendor) | `oauth` | `deny_tools = ["rerun_workflow", "cancel_workflow"]`; not the old npm package |
| Library documentation | Context7, `https://mcp.context7.com/mcp` | `token` | free API key; queries are sent to Context7 |
| Cloudflare | `https://mcp.cloudflare.com/mcp` (vendor) | `oauth` | |
| Stripe | `https://mcp.stripe.com` (vendor) | `token` | a **restricted** key, never the secret key |

Servers, containers and monitoring: [Servers and infrastructure](servers.md).

## Data

| Service | Server | Settings | Safe use |
|---|---|---|---|
| Postgres, MySQL, MariaDB, SQLite | `haymon-ai/dbmcp` (**Rust**), version 0.15.1 or later | `command` | read-only mode is on by default, but it is a SQL filter: the database user must be the real limit (below) |
| Postgres, MySQL, SQLite (fixed queries) | `googleapis/mcp-toolbox` (Go), `--stdio` | `command` | hand-written `tools.yaml` with bound `parameters` only; never `templateParameters`, `*-execute-sql` tools, `--prebuilt` or `--ui` |
| A GraphQL API | `apollographql/apollo-mcp-server` (**Rust**) | `command` | `mutation_mode: none` (the default), predefined operations only, log level `info`, a read-only API token |

The database user decides what a database server can do:

- **Postgres:** a role with `SELECT` grants only, `ALTER ROLE … SET default_transaction_read_only = on`,
  not a superuser, without `pg_read_server_files`, allowed to connect to one database.
- **MySQL:** `SELECT` only, without the `FILE` privilege.
- **SQLite:** a file the server process can only read.

## Search

| Service | Server | Settings | Safe use |
|---|---|---|---|
| Brave Search | `brave/brave-search-mcp-server` (vendor) | `command` | `BRAVE_API_KEY` in `env`, not as an argument |
| Tavily | `https://mcp.tavily.com/mcp` (vendor) | `token` | |
| Exa | `https://mcp.exa.ai/mcp` (vendor) | `token` or `oauth` | |
| SearXNG (self-hosted search) | `ihor-sokoliuk/mcp-searxng` | `command` | `deny_tools = ["web_url_read"]`; never set `MCP_HTTP_ALLOW_PRIVATE_URLS` |

The agent CLIs already search and fetch the web themselves in rooms with `tool_policy = "web"`; a
search server is only needed for a specific engine.

## Chat platforms as data

| Service | Server | Settings | Safe use |
|---|---|---|---|
| Slack (read channels, threads) | `korotovsky/slack-mcp-server` (Go) | `command` | a bot token (`SLACK_MCP_XOXB_TOKEN`) from an app with read scopes; `SLACK_MCP_ENABLED_TOOLS=conversations_history,conversations_replies,conversations_search_messages,channels_list,users_search` (without it, some write tools are on) |

Zalo, Telegram and WhatsApp work as chat platforms of okbot, not as data sources.

## Home

| Service | Server | Settings | Safe use |
|---|---|---|---|
| Home Assistant | built in: `https://<ha>/api/mcp` | `oauth (client)` with `client_id` = the bot's public URL | not tested with okbot; expose only the entities you want controlled |

## Rust bridge for servers okbot cannot reach directly

`agentgateway` (Rust, Linux Foundation) can sit in front of a server that only speaks the old SSE
transport or wants its key in a custom header (`X-API-Key`). okbot then talks streamable HTTP to
agentgateway with `token_env`. Use it only as a protocol bridge, with:

- a static configuration file with that one server and no stdio targets;
- `UI_READ_ONLY=true` (its local admin page can otherwise rewrite the configuration);
- `STATS_ADDR` and `READINESS_ADDR` on `127.0.0.1`.

okbot's own gateway stays the place for permissions.

## Avoid

- **File and notes servers** (filesystem, Obsidian vaults): they give the agent file access, which
  okbot never allows. Save documents into the knowledge base instead ([Knowledge base](knowledge-base.md)).
- **Browser automation** (Playwright and others): the agent could reach internal addresses.
- **Servers that log in as your personal account** with all its powers (a Telegram or WhatsApp user
  session, a browser cookie): a message in a chat could make the agent act as you.
- Servers that are archived, unmaintained, or not listed here: check them the same way first.
