# Discord

okbot answers on Discord as a bot user (`discord` feature, on by default).

1. In the [Developer Portal](https://discord.com/developers/applications), create an application,
   add a **Bot**, and copy its token (Reset Token).
2. `okbot new main --platform discord`, or `okbot channel add discord` for an existing bot (paste
   the token; it goes to `.env`). `channel add` prints the link that adds the bot to a server with
   the permissions okbot needs (*View Channels*, *Send Messages*, *Send Messages in Threads*,
   *Read Message History*); `okbot doctor` shows it too.
3. Open the link and pick your server. To talk to the bot in direct messages you must share a
   server with it.

```toml
[channels.discord.main]
token_env = "DISCORD_TOKEN"
message_content = false     # true (and enable the intent in the portal) to read unmentioned messages
reply_on = ["dm", "mention"]
```

- **Direct messages** and messages that **mention the bot or reply to it** are answered. Discord
  delivers their text without the privileged *Message Content* intent, so it stays off by default.
- Replies quote the message they answer, show "typing…" while the agent works, and never ping
  anyone: `@everyone`, roles and users in the agent's answer are not notified.
- Messages hold 2000 characters; longer answers are split. Tables become lists.
- Ids: `/whoami` shows user and channel ids for `allow_users`, `admins` and `rooms`.
