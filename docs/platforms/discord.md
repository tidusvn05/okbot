# Discord

okbot answers on Discord as a bot user (`discord` feature, on by default).

1. In the [Developer Portal](https://discord.com/developers/applications), create an application,
   add a **Bot**, and copy its token (Reset Token).
2. `okbot new main --platform discord`, or `okbot channel add discord` for an existing bot (paste
   the token; it goes to `.env`). `channel add` prints the link that adds the bot to a server with
   the permissions okbot needs (*View Channels*, *Send Messages*, *Send Messages in Threads*,
   *Read Message History*, *Attach Files* for chart images); `okbot doctor` shows it too.
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

## Interactive replies

With `reply_mode = "interactive"` everything an agent shows is native on Discord:

- **Cards** become a Components V2 message: the text, then one container per card with a coloured
  accent bar (its tone), a `###` title, a divider and the card text. Up to 5 cards, about 4000
  characters of text in all.
- **Charts** (`[charts] images = true`, ADR 0016): the chart PNG is attached to the message and
  shown inside its card as a media gallery, with the chart's description as alt text. Later edits
  after a click keep the picture; if Discord refuses it, the card is shown without it.
  In servers the bot needs the *Attach Files* permission (in the invite link since okbot 0.11; for
  a bot added earlier, grant it to its role); without it the view is sent with the chart as text.
- **Menus** are a string select (2–25 options, with descriptions); picking an option answers with it.
- **Buttons** sit in one row of up to 5; **link buttons** open their `https` URL and send nothing.
- **Forms** open as a modal of up to 5 text fields, pre-filled when the agent gives values.
- Prompts without cards stay classic messages (text, menu row, button row).
- Clicks are acknowledged at once (Discord allows 3 seconds). Prompts survive a restart: the bot asks
  the runtime for a prompt it no longer holds, and says "This prompt has expired" only when neither
  knows it.
- When a prompt is answered or closed, its message is rebuilt with what is still answerable; a
  message with cards keeps them (Discord cannot turn a V2 message back into plain content).
