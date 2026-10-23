---
type: Guide
title: Running on a VPS
description: Running bots as services, several bots on one machine, webhooks behind a reverse proxy, containers, backups and upgrades.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-23
aliases: [triển khai, deploy, vps, server, systemd, docker, reverse proxy, caddy, nginx, cloudflare tunnel, nhiều bot, backup, nâng cấp]
generated: {by: okbot-guide-pack, at: "2026-10-23"}
---

# Running on a VPS

## Services

```sh
okbot new shop /srv/bots/shop
okbot start shop          # systemd user unit okbot-shop.service; restarts on failure and at boot
okbot status shop
okbot logs shop -f
okbot restart shop
okbot stop shop
okbot list                # every bot on this machine
```

`okbot start` enables lingering so user services survive logout; if that needs root it prints the
command. As root, okbot installs a system unit. Without systemd, run `okbot run` under your own
supervisor, or use the image `ghcr.io/tidusvn05/okbot`.

## Several bots on one machine

Each bot is its own directory and process, with its own tokens, knowledge and history.

| Shared | Default | To separate |
|---|---|---|
| Agent CLI login and quota | the user's login | `[agent] home = "agent-home"`, then `HOME=<bot>/agent-home claude` once |
| Agent processes | `[agent] max_concurrent` per bot | machine-wide `[limits] max_agents` in `~/.config/okbot/config.toml` |
| HTTP port | the wizard picks a free one from 8787 | only bots with webhooks, OAuth callbacks, notify or automation webhooks need one |

## Webhooks and public URLs

Webhook delivery (Zalo, Telegram, Google Chat HTTP) and `/connect` for people on other machines
need a public HTTPS URL that reaches `[http] listen`. With Caddy:

```
bot.example.com {
    reverse_proxy /zalo/shop 127.0.0.1:8787
    reverse_proxy /oauth/* 127.0.0.1:8787
}
```

Or use a Cloudflare Tunnel when the machine has no public IP. Expose only the paths the platforms
need; keep `/notify` and `/automation` on loopback when the callers are local.

## Monitoring

`okbot doctor` checks everything; `/healthz` and Prometheus `/metrics` are on the HTTP port. Logs
are structured and secrets are redacted.

## Backups

Back up the whole bot directory: `okbot.toml`, `.env`, `kb/` (or its git remote) and `state/`.
`state/secrets.key` and the database go together: without the key, people must reconnect their
services.

## Upgrades

Install the new version the same way, then `okbot restart <bot>`. After an upgrade, run `okbot kb
upgrade` to add new default files, and `okbot kb pack add okbot-guide` to refresh this guide.
