---
type: Guide
title: Servers and infrastructure
description: Controlling servers from chat with ssh-mcp (several hosts, read-only profiles, approvals and audit), and Kubernetes, Grafana and Ansible through their MCP servers.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-24
aliases: [ssh, ssh-mcp, server, máy chủ, điều khiển server, quản lý server, vps, remote, chạy lệnh, kubernetes, k8s, grafana, ansible, devops, ops, hạ tầng]
generated: {by: okbot-guide-pack, at: "2026-10-24"}
---

# Servers and infrastructure

Checked in October 2026. Every server on this page was reviewed for security (advisories, code,
read-only modes) and is listed with the settings that keep it safe. Use them on a bot or in rooms
that only administrators use: these tools reach production systems.

Prefer, in this order:

1. **A chat command** for a task you repeat (`/restart web`, `/logs api`): your own small MCP server
   plus `[[command]]` ([Your own tools and commands](own-tools.md)). No agent decides anything.
2. **The platform's MCP server in read-only mode** (Kubernetes, Grafana, Ansible below).
3. **SSH commands through ssh-mcp** when you need to look around a server from your phone.

## SSH: tufantunc/ssh-mcp

[`ssh-mcp`](https://github.com/tufantunc/ssh-mcp) (TypeScript, MIT) connects to many hosts and
checks every command before it runs:

- Each command is classified as `read-only`, `safe`, `destructive` or `privileged` from its full
  syntax. Redirects, `;`, `$(…)`, `tee`, `sed -i` and `awk system()` hidden in a "read" command are
  refused. Some commands are never allowed.
- **`read-command`** runs only read-only commands and is marked read-only, so okbot runs it without
  asking. **`run-command`** is marked destructive, so okbot shows **Allow** / **Deny** first.
- Each host is a profile with a role and a group (`prod`, `staging`, `dev`). A profile with
  `readOnly = true` refuses every change, even after **Allow**.
- An audit log records every decision.

Tested with okbot and ssh-mcp 2.20.2.

### 1. On each server

Never add the machine that runs okbot as a host: its `.env` holds the bot's tokens. On every other
server, create a user without sudo for the bot, and allow only its key:

```sh
sudo adduser --disabled-password okbot-ops
sudo install -d -m 700 -o okbot-ops /home/okbot-ops/.ssh
sudo install -m 600 -o okbot-ops ops_key.pub /home/okbot-ops/.ssh/authorized_keys
ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub    # the fingerprint to pin below
```

### 2. Next to the bot

`npm install -g ssh-mcp`, then write its configuration. The file must be `0600` in a `0700`
directory, or ssh-mcp refuses to start:

```toml ssh-mcp
# ~/.config/ssh-mcp/config.toml (ssh-mcp's file, not okbot.toml)
[defaults]
defaultProfile = "web"
approvalMode = "auto"     # okbot asks Allow/Deny; ssh-mcp's own prompts cannot reach the chat

[[profiles]]
name = "web"
host = "10.0.0.5"
user = "okbot-ops"
auth = "key"
keyRef = "/home/okbot/.ssh/ops_key"
trustedHostKey = "SHA256:…"      # from step 1
role = "operator"
group = "prod"

[[profiles]]
name = "db"
host = "10.0.0.6"
user = "okbot-ops"
auth = "key"
keyRef = "/home/okbot/.ssh/ops_key"
trustedHostKey = "SHA256:…"
role = "viewer"
group = "prod"
readOnly = true
```

Add one profile per host; the agent picks the profile by name ("disk usage on db"). Jump hosts use
`via = "<profile>"`.

- `approvalMode = "auto"` is required with okbot. ssh-mcp's own approval uses MCP elicitation,
  which okbot does not pass to the chat; without `auto`, every change is refused.
- By default an `operator` in group `prod` may run read-only and safe commands; destructive and
  sudo commands are refused. Widen it only per group with `[policy.roleBindings.<role>]` (see
  ssh-mcp's README), and add your own refusals with `[policy] denylist = ["regex", …]`.

Run it as a service on loopback:

```sh
ssh-mcp --config=/home/okbot/.config/ssh-mcp/config.toml \
  --transport=http --httpPort=8791 --bearerToken="$SSH_MCP_TOKEN"
```

It listens on `127.0.0.1` by default and refuses requests without the token. The token is a
command-line argument, so run it on a machine where other users cannot list processes. The audit
log is `~/.local/share/ssh-mcp/audit.log`.

### 3. In okbot.toml

Put the same token in `.env` as `SSH_MCP_TOKEN`. The MCP endpoint is the root path, not `/mcp`:

```toml
[[mcp]]
name = "ssh"
url = "http://127.0.0.1:8791/"
token_env = "SSH_MCP_TOKEN"
admins_only = true
confirm = "destructive"     # read-command runs at once; run-command waits for Allow
deny_tools = ["privileged-command", "open-session", "close-session", "signal-process",
              "sftp-upload", "sftp-upload-file", "sftp-download-file"]
```

Add `rooms = ["<your direct chat>"]` to keep it out of groups. In chat: "check disk and memory on
web", "show the last 50 lines of nginx's error log on web".

## Kubernetes: containers/kubernetes-mcp-server

Go, Apache-2.0. Use it read-only with a ServiceAccount that has the `view` role only:

```toml kubernetes-mcp-server
# okbot-k8s.toml (kubernetes-mcp-server's file)
read_only = true
toolsets = ["core"]

[[denied_resources]]
group = ""
version = "v1"
kind = "Secret"
```

```toml
[[mcp]]
name = "k8s"
command = ["kubernetes-mcp-server", "--config", "/home/okbot/okbot-k8s.toml"]
env = ["KUBECONFIG"]                       # a kubeconfig for the view-only ServiceAccount
admins_only = true
deny_tools = ["configuration_view"]        # it returns the kubeconfig with its token
```

`read_only` also removes `pods_exec` and `pods_run`.

## Grafana: grafana/mcp-grafana

Go, Apache-2.0. Use a service account with the **Viewer** role:

```toml
[[mcp]]
name = "grafana"
command = ["mcp-grafana", "--disable-write", "--disable-api", "--disable-admin"]
env = ["GRAFANA_URL", "GRAFANA_SERVICE_ACCOUNT_TOKEN"]
roles = ["ops"]
```

Never add `--allow-grafana-url-override`: it lets the agent point the server at other addresses.

## Ansible: ansible/aap-mcp-server

Red Hat's server for Ansible Automation Platform (Node.js, built from its repository, no releases
yet). Run it next to the bot with `BASE_URL` pointing at your AAP; it answers on
`http://127.0.0.1:3000/mcp/<toolset>`. Use it to look at jobs, inventories and templates:

- Leave `ALLOW_WRITE_OPERATIONS` unset: the server then allows only reads.
- Give okbot an AAP token of an **Auditor** user.
- Define a toolset in `aap-mcp.yaml` with only the `*_list` and `*_read` tools you need.

```toml
[[mcp]]
name = "ansible"
url = "http://127.0.0.1:3000/mcp/readonly"   # the toolset you defined
token_env = "AAP_TOKEN"
admins_only = true
```

Its tools carry no annotations, so with `confirm = "destructive"` every call waits for **Allow**;
leave `confirm` at its default once the toolset holds only reads. To launch playbooks from chat,
write a `[[command]]` for one job template instead of giving the agent write access.
