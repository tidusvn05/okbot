---
type: Guide
title: Google Workspace
description: Connecting Gmail, Drive, Docs, Sheets and Calendar through a self-hosted Google MCP server, choosing between google-mcp-rs and workspace-mcp, and the Google Cloud and consent screen setup.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [google, gmail, google drive, google sheets, google calendar, google docs, kết nối google, kết nối gmail, google-mcp-rs, workspace-mcp, oauth google, google cloud]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Google Workspace

Gmail, Drive, Docs, Sheets and Calendar are reached through a Google MCP server that you run next
to the bot. In OAuth mode, every person connects their own Google account with `/connect google`.

## Which server

| | `google-mcp-rs` (Rust) | `workspace-mcp` (Python) |
|---|---|---|
| Repository | github.com/matpb/google-mcp-rs | github.com/taylorwilsdon/google_workspace_mcp |
| Services | Gmail, Drive, Docs, Sheets, Calendar, Tasks, Contacts, Search Console, opt-in Health (~127 tools) | also Slides, Forms, Chat, Apps Script, Search (~120 tools) |
| Footprint | one binary or small image, ~10 MB memory | Python runtime via `uvx`, ~100 MB |
| Tool annotations | proposed upstream (PR #16) | yes |
| Token for okbot | 30 days, no refresh; refresh proposed upstream (PR #17) | refreshable |
| okbot preset | none yet | `okbot mcp add google-workspace` |

**Recommendation today: `workspace-mcp`**, because its annotations let `confirm = "destructive"`
ask only for sends, shares and deletes. Choose **`google-mcp-rs`** when memory or a Python-free host
matters more, and narrow it to reading tools (below). Once the two upstream changes are released,
`google-mcp-rs` is the better fit for okbot. Speed is not a reason to choose: each Gmail or Drive
call spends 100–500 ms at Google, far more than either server adds.

Google's own MCP servers (gmailmcp.googleapis.com and others) are a developer preview, need
enrollment, cannot send mail or share files, and do not work with okbot yet.

## 1. Google Cloud (free)

1. In Google Cloud Console create a project and enable the APIs you want (Gmail, Drive, Docs,
   Sheets, Calendar). No billing account is needed: these APIs have quotas, not fees.
2. Configure the OAuth consent screen (see the table below).
3. Create an OAuth client of type **Web application** with the server's redirect URI:
   `http://127.0.0.1:8000/oauth2callback` for workspace-mcp, `<BASE_URL>/oauth/google/callback`
   for google-mcp-rs.

| Your accounts | Audience and status | Result |
|---|---|---|
| Google Workspace (your domain) | **Internal** | best: no review, no warning, tokens last |
| Personal `@gmail.com` | External, **In production** (publish, no verification) | tokens last; login shows "Google hasn't verified this app" → Advanced → continue; up to 100 users |
| Personal `@gmail.com` | External, Testing | **refresh tokens expire after 7 days**: everyone reconnects weekly |

Verification by Google is only needed for public apps beyond 100 users.

## 2a. workspace-mcp

```sh
GOOGLE_OAUTH_CLIENT_ID=… GOOGLE_OAUTH_CLIENT_SECRET=… MCP_ENABLE_OAUTH21=true \
WORKSPACE_MCP_PORT=8000 uvx workspace-mcp --transport streamable-http --tool-tier core
okbot mcp add google-workspace     # its name in okbot is `google`
okbot restart
```

`--read-only` limits it to reading; `--tools gmail drive sheets` picks services.

## 2b. google-mcp-rs

Run it with `MCP_HOST=127.0.0.1` (it listens on all interfaces by default), `BASE_URL`,
`GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `JWT_SECRET`, `STORAGE_ENCRYPTION_KEY` and
`ENABLED_DOMAINS` (see its README). Then:

```toml
[[mcp]]
name = "google"
url = "http://127.0.0.1:8433/mcp"
auth = "oauth"
tools = ["gmail_search_*", "gmail_get_*", "gmail_list_*", "calendar_list_*", "calendar_get_*",
         "calendar_freebusy", "drive_list_files", "drive_get_file", "sheets_get*",
         "sheets_batch_get_values"]
confirm = "never"        # only reading tools are offered
```

## 3. Connect

Each person sends `/connect google` in a direct chat, logs in, and sends back the code. When Google
answers `invalid_grant` (expired or revoked), okbot forgets the token and the agent asks the person
to `/connect google` again.

## Safety

Mail is where prompt injection arrives: anyone can email your assistant instructions. Keep
`confirm = "destructive"` so every send, share or delete waits for **Allow**, offer only the tools
you use, and consider `tool_policy = "mcp-only"` where mail is read. To report new mail to chat, see
[Automations](automations.md).
