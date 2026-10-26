# Google Chat

okbot answers in Google Chat as a Chat app (`gchat` feature, on by default). Chat apps need a
Google Workspace account and a Google Cloud project.

## 1. Create the app

1. In the [Google Cloud console](https://console.cloud.google.com/), create (or pick) a project and
   enable the **Google Chat API** (and the **Pub/Sub API** for Pub/Sub delivery).
2. Create a **service account** and a JSON key for it. okbot posts replies as this account.
3. Configure the Chat app (APIs & Services → Google Chat API → Configuration): name, avatar,
   description, and the connection settings from step 2 below
   ([guide](https://developers.google.com/workspace/chat/configure-chat-api)).

## 2. Choose how events reach okbot

| | `pubsub` | `http` |
|---|---|---|
| Public URL | not needed: okbot pulls from a subscription | an HTTPS URL that reaches okbot (reverse proxy) |
| Google Cloud setup | topic + pull subscription; grant `chat-api-push@system.gserviceaccount.com` the **Pub/Sub Publisher** role on the topic and your service account **Pub/Sub Subscriber** on the subscription | none beyond the app |
| Authentication | IAM | okbot verifies Google's signed bearer token on every request |
| Limits | no forms (dialogs); cards are updated whole | none |
| Billing | the project must have billing enabled (a small bot usually stays in the free tier) | not required for Chat |

In both modes okbot answers through the Chat API: an agent turn takes longer than Chat's
30-second synchronous window, so the webhook returns an empty response at once. While the agent
works, okbot posts "_Thinking…_" in the thread and replaces it with the answer.

References: [receive and respond](https://developers.google.com/workspace/chat/receive-respond-interactions) ·
[Pub/Sub quickstart](https://developers.google.com/workspace/chat/quickstart/pub-sub) ·
[verify requests](https://developers.google.com/workspace/chat/verify-requests-from-chat) ·
[send a message](https://developers.google.com/workspace/chat/create-messages)

## 3. Set up the bot

```sh
okbot new corp --platform gchat \
  --gchat-credentials ~/Downloads/my-project-1234.json \
  --gchat-delivery pubsub --gchat-subscription projects/my-project/subscriptions/okbot
```

The key is copied to `gchat-service-account.json` in the bot directory (mode 0600, git-ignored)
and checked by asking Google for an access token.

```toml
[channels.gchat.corp]
credentials = "gchat-service-account.json"
delivery = "pubsub"                               # or "http"
subscription = "projects/my-project/subscriptions/okbot"
# HTTP delivery: exactly one of
# audience_url = "https://bot.example.com/gchat/corp"   # "Authentication audience: HTTP endpoint URL"
# audience_project_number = "123456789012"              # "Authentication audience: Project number"
reply_on = ["dm", "mention"]
```

With `http`, point the app's HTTP endpoint at `https://<your host>/gchat/<instance>` and route that
path to okbot's `[http] listen` address.

## 4. What to expect

- **Direct messages**: every message is answered, in one conversation per person.
- **Spaces**: Chat delivers messages to the app when it is @mentioned; each thread is its own
  conversation, and okbot replies in the thread.
- **Formatting**: `*bold*`, `_italic_`, `~strike~`, code, lists and `<url|text>` links; tables are
  turned into lists. Messages hold 4096 characters.
- **Ids**: users are `users/<id>` and spaces `spaces/<id>`; `/whoami` shows them for `allow_users`,
  `admins` and per-space `rooms`.
- **Buttons**: prompts and tool approvals are card buttons; a click arrives as a `CARD_CLICKED` event
  through the same endpoint or subscription. Only the people a prompt is for can answer; other clicks are
  ignored. Forms open as dialogs with `http` delivery only (see below).

## Interactive replies

With `reply_mode = "interactive"` the agent's cards, menus and buttons are native Google Chat cards
(`cardsV2`):

- **Text** stays the message text; each **card** is a card of its own with a header (its title, with
  the tone's emoji: ℹ️ ✅ ⚠️ ❌) and its text. Card text uses the card HTML subset: bold, italic,
  strike and links are converted; code stays as typed.
- **Menus** are a dropdown in a last card with the buttons. Its first item is the placeholder;
  picking an option answers with it (descriptions follow the label after "—"). Picking the
  placeholder again does nothing.
- **Buttons** are a button list (danger red, success green, secondary outlined); **link buttons**
  open their URL and send nothing.
- **Forms** open as a dialog of text inputs (pre-filled when the agent gives values) with a submit
  button; required fields are checked before the answer is sent, and the dialog then closes. Chat opens
  dialogs only as the synchronous answer to a click, so forms need `delivery = "http"`. With `pubsub`
  the bot does not offer forms and the agent asks for the values in chat instead.
- Prompts survive a restart: a click on a prompt the bot no longer holds is checked with the runtime;
  unknown prompts are ignored (a dialog submission says "This prompt has expired.").
- When a prompt is answered or closed, the message is rebuilt (text and cards, plus whatever can still
  be answered) with one `messages.patch`.
