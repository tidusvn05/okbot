# Running okbot on a VPS

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/tidusvn05/okbot/main/install.sh | sh
```

The installer checks the archive against the release's `SHA256SUMS` and puts `okbot` in
`~/.local/bin`. Before the first release, build from source: `cargo install --locked --git
https://github.com/tidusvn05/okbot okbot-cli`.

Install and log in to at least one agent CLI that can restrict its tools: Claude Code
(`claude`, then log in) or Codex (`codex login`). `okbot doctor` checks both.

## 2. Create a bot

A bot is a directory. Choose one and follow the wizard:

```sh
okbot new shop /srv/bots/shop
```

It detects the agent CLIs, asks for the platform (Zalo: paste the bot token; it goes to `.env`),
writes `okbot.toml` and the default knowledge in `kb/`, and registers the bot. Put your own
knowledge in `kb/knowledge/` and check it with `okbot kb lint shop`.

```sh
okbot doctor shop          # everything green?
okbot chat                 # in /srv/bots/shop: try the bot in the terminal
```

## 3. Run it as a service

```sh
okbot start shop           # systemd user unit okbot-shop.service, restarts on failure
okbot status shop
okbot logs shop -f
okbot restart shop         # after editing okbot.toml or kb/persona
okbot stop shop
```

`okbot start` asks systemd to keep user services running after logout
(`loginctl enable-linger`); if that needs root, it prints the command to run once. As root, okbot
installs a system unit instead. Without systemd (containers), run `okbot run` under your own
supervisor, or use the Docker image (see the `Dockerfile`).

## 4. More platforms, and one person on several of them

A bot can serve several platforms at once, with one knowledge bundle:

```sh
okbot channel add discord --bot shop      # asks for the token, saves it to .env
okbot channel list --bot shop
okbot channel remove slack:shop --bot shop
okbot restart shop
```

Each platform account has its own personal memory. For a personal assistant used from Zalo and
Discord, link your accounts so they share one profile and memory (`kb/memory/people/<person>/`):

```sh
# `/whoami` in each chat shows the account id
okbot people link luan zalo:shop/6ede9afa66b88fe6d6a9 discord:shop/123456789012345678
okbot restart shop
```

Notes saved before the link stay visible from every linked account. Only the operator can link
accounts (in `okbot.toml` `[people.<person>]`), because linked accounts read each other's notes.

## 5. Many bots on one machine

Repeat `okbot new <name> <dir>` per bot; `okbot list` shows them all. Each bot is its own
process, with its own token, knowledge, history and service.

| Shared resource | Default | To separate |
|---|---|---|
| Agent CLI login and quota | the user's login, shared by all bots | `[agent] home = "agent-home"` in a bot's `okbot.toml`, then log in once with `HOME=/srv/bots/shop/agent-home claude` |
| Agent processes at once | `[agent] max_concurrent` per bot | a machine-wide cap in `~/.config/okbot/config.toml`: `[limits]` `max_agents = 4` |
| HTTP port | the wizard picks a free one from 8787 | only bots with webhook delivery need one |

## 6. Webhooks behind a reverse proxy

Webhook delivery needs a public HTTPS URL that reaches the bot's `[http] listen` address. With
Caddy:

```
bot.example.com {
    reverse_proxy /zalo/shop 127.0.0.1:8787
    reverse_proxy /zalo/cafe 127.0.0.1:8788
}
```

`okbot doctor shop --test-webhook` asks Zalo to call the URL (Zalo rate-limits this check).
