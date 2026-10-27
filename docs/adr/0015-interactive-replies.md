# 0015 — Interactive replies

Status: Accepted (2026-10-26)

## Context
Every platform okbot serves except Zalo and the console has native components: Discord buttons,
menus, modals and Components V2 containers; Slack Block Kit and modals; Google Chat cards and dialogs;
Telegram inline keyboards. Replies used none of them: a choice was a list to retype, a record a run of
lines. Prompts (ADR 0007) already carried buttons and forms, but only for `[[mcp]]` servers and
approvals, and only in memory. The agent is untrusted (design §4.4) and must not write platform
payloads.

## Decision
- **Opt-in.** `reply_mode = "text" | "interactive"` in `[bot]`, per channel and per room (room over
  channel over bot); `text` is the default and keeps today's behaviour exactly.
- **A tool, not a payload.** In an interactive room the gateway offers a built-in `ui` server (like
  `team`, ADR 0014) with one tool, `show`: text, up to 5 cards (title, tone, text, fields, a table, a
  bar chart drawn with `▇`), 2–25 choices, one form (≤ 5 fields), up to 3 `https` link buttons. The
  runtime checks it against the platform's limits at once, so the agent can fix it, and sends it
  after the answer text; at most 3 views per answer. The system prompt gains an `<interactive>`
  section. Automations (ADR 0013) and peer tasks never get the tool.
- **Core grows, adapters render.** `Prompt` gains `cards` and `select`; `PromptButton.url` makes
  link buttons; `FormField.value` pre-fills. `ChannelCaps.ui` (`buttons`, `select`, `forms`, `cards`)
  says what a platform shows natively; the runtime puts everything else into the text (choices as a
  numbered list, a form as a list of fields). `Channel::update_prompt` replaces a prompt statelessly;
  `InboundSink::prompt` gives an adapter that restarted the prompt behind a click in the same room.
- **A click is a message.** The first answer to a view becomes the clicker's next message
  (`→ <label>`, or the form's title and values), after the same access checks as typed text,
  made before the view is used up: a press the bot would drop (rate limit, length, someone other
  than a personal bot's owner) leaves the view open for others. The arrow also keeps a label from
  running as a `/command`. The view then shows who chose what and
  loses its buttons (links stay). The history keeps every view as text, so later turns know what
  was offered and typing works as well as clicking.
- **Views outlive restarts.** Answerable views are kept in the store (`views` table, operational
  data) for 7 days; `take_view` lets exactly one answer through. ADR 0007 prompts stay in memory.

## Consequences
- Unlike ADR 0007 forms, values typed into the agent's forms reach the agent, its provider and the
  history. The agent is told never to ask for secrets and fields named like passwords, codes, tokens
  or card numbers (in English, Vietnamese and Japanese) are refused; collecting secrets stays the job of an MCP server's prompt.
- Buttons from the agent are not approvals: `[[mcp]] confirm` still guards actions with effects.
- Clicks cost agent turns and count for `[limits]` and the rate limit.
- Platform limits differ (Slack cuts labels to 75 characters, Google Chat dialogs need HTTP delivery,
  Telegram has no forms); each platform guide lists them.
- Charts are text bars; images of charts would need a drawing crate and fonts with Vietnamese glyphs,
  left for later.
