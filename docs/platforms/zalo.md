# Zalo Bot

okbot answers in Zalo through the [Zalo Bot Platform](https://bot.zapps.me/docs/) (`zalo` feature,
on by default). The API client is the
unofficial [`zalo-bot`](https://github.com/tidusvn05/zalo-bot) crate, usable on its own.

## 1. Create the bot

1. In the Zalo app, open the official account **Zalo Bot Manager** and choose **Tạo bot** to open
   *Zalo Bot Creator* ([guide](https://bot.zapps.me/docs/create-bot/)).
2. Name the bot. The name must start with `Bot` (for example `Bot MyShop`).
3. Zalo sends the **bot token** (`12345689:abc-xyz`) to your Zalo account. It does not expire until
   you reset it in Zalo Bot Creator. Treat it like a password.

## 2. Choose how okbot receives messages

| | `polling` | `webhook` |
|---|---|---|
| Public URL | not needed | an HTTPS URL that reaches okbot (reverse proxy, Cloudflare Tunnel) |
| Setup | none | okbot registers the URL and a random secret itself |
| Zalo's guidance | development and testing | production |

okbot switches modes on start: polling removes a registered webhook (`deleteWebhook`), webhook
mode registers it (`setWebhook`). With `webhook`, Zalo's `POST` requests must reach okbot's HTTP
server at `/zalo/<instance>`; okbot checks the `X-Bot-Api-Secret-Token` header and answers at once.

```toml
[channels.zalo.shop]
token_env = "ZALO_BOT_TOKEN"        # the token lives in .env, never in okbot.toml
delivery = "polling"                # or "webhook"
# public_url = "https://bot.example.com/zalo/shop"
# secret_env = "ZALO_WEBHOOK_SECRET"
```

## 3. What to expect

- **Private chats**: every message is answered.
- **Groups** (beta on Zalo's side): add the bot through its invite link; it receives only messages
  that mention it or reply to it ([group guide](https://bot.zapps.me/docs/build-bot-interaction-with-group/)).
- **Formatting**: bold, italic, lists and quotes. Zalo has no links, code blocks or tables in
  messages, so okbot writes URLs in plain text and turns tables into lists.
- **Length**: 2000 characters per message; longer answers arrive as several messages.
- **No editing**: Zalo cannot edit sent messages, so okbot shows "typing…" while it works and sends
  the answer when it is ready.
- **Protected users**: for some accounts (children, people with disabilities, …) Zalo withholds the
  message content and sends `message.unsupported.received`. okbot replies with a fixed message and
  never processes these.
- **Images, stickers, voice**: images are answered from their caption; stickers and voice messages
  are acknowledged as such.

## 4. Your responsibilities

The [Zalo Bot terms](https://bot.zapps.me/docs/terms/) treat you, the operator, as the provider or
deployer of an AI system: you are responsible for what the bot says, and incidents involving
end-user data or generated content must be reported to Zalo Platforms within 24 hours. Keep the
bot's knowledge accurate and its tool policy appropriate for the people it talks to.

## 5. Troubleshooting

| Symptom | Check |
|---|---|
| `okbot doctor` reports the token as invalid | The token was reset in Zalo Bot Creator; update `.env`. |
| No messages in webhook mode | `okbot doctor` runs Zalo's `testWebhook`; `webhook.http.403` usually means a WAF or CDN blocks Zalo's `Java/…` user agent; `webhook.err.tls` means an incomplete certificate chain. Zalo does not follow redirects. |
| Replies arrive late in polling mode | Polling can miss or delay events; switch to `webhook` for production. |
