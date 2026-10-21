# Telegram

okbot answers in Telegram through the [Bot API](https://core.telegram.org/bots/api) (`telegram`
feature, on by default).

## 1. Create the bot

1. Open [@BotFather](https://t.me/BotFather) in Telegram and send `/newbot`.
2. Pick a display name and a username ending in `bot` (for example `MyShopBot`).
3. BotFather replies with the **bot token** (`123456789:AA…`). Treat it like a password; `/revoke`
   in BotFather replaces it.

```sh
okbot channel add telegram --bot shop     # asks for the token, checks it, saves it to .env
# or, non-interactively:
okbot init --platform telegram --telegram-token-env TELEGRAM_BOT_TOKEN --yes
```

## 2. Choose how okbot receives messages

| | `polling` | `webhook` |
|---|---|---|
| Public URL | not needed | an HTTPS URL that reaches okbot (reverse proxy, Cloudflare Tunnel) |
| Setup | none | okbot registers the URL and your secret on start |
| Fits | most bots, including behind NAT | many busy bots on one server |

okbot switches modes on start: polling removes a registered webhook (`deleteWebhook`, pending
messages are kept), webhook mode registers it (`setWebhook`). With `webhook`, Telegram's `POST`
requests must reach okbot's HTTP server at `/telegram/<instance>`; okbot checks the
`X-Telegram-Bot-Api-Secret-Token` header and answers at once. Telegram only allows ports 443, 80,
88 and 8443 for webhooks.

```toml
[channels.telegram.shop]
token_env = "TELEGRAM_BOT_TOKEN"    # the token lives in .env, never in okbot.toml
delivery = "polling"                # or "webhook"
# public_url = "https://bot.example.com/telegram/shop"
# secret_env = "TELEGRAM_WEBHOOK_SECRET"   # 1–256 of A-Z a-z 0-9 _ -
reply_on = ["dm", "mention"]
```

Only one process may poll a bot token at a time: two okbot bots (or another program) using the same
token make `getUpdates` fail with `409 Conflict`.

## 3. What to expect

- **Private chats**: every message is answered.
- **Groups**: with privacy mode on (the default), Telegram delivers commands, replies to the bot and
  messages that mention `@YourBot`; okbot answers those. For `reply_on = ["all"]`, turn privacy mode
  off in BotFather (`/setprivacy` → Disable) and add the bot to the group again; `okbot doctor` warns
  when the two disagree. In groups okbot quotes the message it answers.
- **Commands**: `/tasks@YourBot` works like `/tasks`; commands addressed to another bot are ignored.
- **Forum topics**: each topic is its own conversation with its own history.
- **Formatting**: bold, italic, strikethrough, code, code blocks, links and quotes (`parse_mode =
  HTML`); tables become lists. If Telegram rejects the markup, okbot resends the message as plain
  text.
- **Length**: 4096 characters per message; longer answers arrive as several messages.
- **Progress**: okbot shows "typing…" while the agent works and sends the answer when it is ready.
- **Photos and files**: the agent can view photos (`attachment_view`), and a personal bot's owner can
  save documents to the knowledge base ("save this"). Files up to 20 MB, the Bot API's download limit.
  Voice messages and stickers are acknowledged as such.

## 4. Buttons

Prompts and tool approvals are inline buttons; okbot asks Telegram for `callback_query` updates next to
messages. A press by someone the prompt is not for shows them a notice and does nothing. Forms are not
offered on Telegram.

## 5. Reminders

Telegram lets a bot message a user at any time once the user has started a private chat with it
(`/start`), so reminders and routines work as long as the owner has written to the bot directly once
and has not blocked it.

## 6. Troubleshooting

| Symptom | Check |
|---|---|
| `okbot doctor` reports `401 Unauthorized` | The token was revoked in BotFather; update `.env`. |
| `409 Conflict` in the log | Another process polls the same token, or a webhook was set elsewhere. Stop it, or use one okbot bot per token. |
| No group messages | Privacy mode is on and the message does not mention the bot; see *Groups* above. |
| No messages in webhook mode | `okbot doctor` shows Telegram's last delivery error (`getWebhookInfo`). Telegram needs a valid certificate and one of ports 443, 80, 88, 8443. |
