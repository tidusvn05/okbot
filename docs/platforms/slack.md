# Slack

okbot answers on Slack through **Socket Mode** (`slack` feature, on by default): no public URL.

1. Create an app at [api.slack.com/apps](https://api.slack.com/apps) and enable **Socket Mode**;
   create an **app-level token** with `connections:write` (`xapp-…`).
2. Bot token scopes: `app_mentions:read`, `chat:write`, `im:history`, `im:read`. Add
   `files:write` only for chart images (`[charts] images = true`).
3. Event subscriptions (bot events): `app_mention`, `message.im`. Allow users to message the app
   (App Home → Messages tab).
4. Turn on **Interactivity & Shortcuts** (no request URL is needed with Socket Mode): prompts, tool
   approvals and interactive replies use buttons, menus and modals. Modals need no extra scope.
5. Install the app to the workspace and copy the **bot token** (`xoxb-…`).
6. `okbot new acme --platform slack` (paste both tokens; they go to `.env`).

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
- Prompts (approvals of tool calls, prompts of your MCP servers) are Block Kit buttons; a press by someone
  else gets a private "not yours" notice, a press on a prompt okbot no longer waits for a private
  "expired" notice. A button with a form opens a **modal**.
- Formatting uses mrkdwn (`*bold*`, `_italic_`, `<url|text>`); okbot keeps messages under 3000
  characters and splits longer answers.

## Interactive replies

With `reply_mode = "interactive"` everything the agent shows is native Block Kit:

- **Cards**: a header (plain text, with the tone's emoji in front: ℹ️ ✅ ⚠️ ❌) and a mrkdwn section;
  cards are separated by dividers. Slack has no coloured blocks, so the tone is only the emoji.
- **Menu**: a `static_select`; picking an option answers at once.
- **Buttons**: Slack has only a default, a green (`primary`, also used for success) and a red
  (`danger`) style. **Link buttons** open their URL; Slack reports their presses too, okbot ignores them.
- **Forms**: a modal with one text input per field (multi-line for paragraph fields, pre-filled,
  optional fields marked optional, length limits enforced by Slack). The modal must open within
  3 seconds of the press, which okbot does as soon as the press arrives. Values go to the runtime
  and are never logged.
- **Charts** (`[charts] images = true`, ADR 0016): okbot uploads the chart PNG
  (`files.getUploadURLExternal`, then `files.completeUploadExternal`) as its own message, with
  alt text, just before the prompt. This needs the `files:write` bot scope; without it okbot logs
  a warning naming the scope and sends the whole reply as text, chart bars included.
- After an answer okbot rewrites the message: the text and cards stay, the menu and buttons go.
- Prompts survive a restart: a press on a prompt shown before it is looked up in the runtime.

Slack limits that okbot applies by shortening: button and option labels and option descriptions
are cut to 75 characters, modal titles to 24, card titles to 150; text over 3000 characters is
split into several sections.
