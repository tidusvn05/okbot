# Slack

okbot answers on Slack through **Socket Mode** (`slack` feature, on by default): no public URL.

1. Create an app at [api.slack.com/apps](https://api.slack.com/apps) and enable **Socket Mode**;
   create an **app-level token** with `connections:write` (`xapp-…`).
2. Bot token scopes: `app_mentions:read`, `chat:write`, `im:history`, `im:read`.
3. Event subscriptions (bot events): `app_mention`, `message.im`. Allow users to message the app
   (App Home → Messages tab).
4. Install the app to the workspace and copy the **bot token** (`xoxb-…`).
5. `okbot new acme --platform slack` (paste both tokens; they go to `.env`).

```toml
[channels.slack.acme]
app_token_env = "SLACK_APP_TOKEN"
bot_token_env = "SLACK_BOT_TOKEN"
reply_on = ["dm", "mention"]
```

- Every envelope is acknowledged at once (Slack retries after 3 seconds); duplicates are dropped.
- Mentions in channels are answered **in a thread**, which is the conversation; direct messages are
  one conversation per person.
- Slack bots have no typing indicator, so okbot posts "_Thinking…_" and edits it into the answer.
- Formatting uses mrkdwn (`*bold*`, `_italic_`, `<url|text>`); okbot keeps messages under 3000
  characters and splits longer answers.
