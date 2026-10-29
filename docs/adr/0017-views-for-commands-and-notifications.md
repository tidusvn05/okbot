# 0017 — Views for commands and notifications

Status: Accepted (2026-10-30)

## Context
Fixed `[[command]]`s (ADR 0008) call one `[[mcp]]` tool without the agent and show its text. That
is how operations keep working when the agent is down or out of quota. Since ADR 0015 the agent's
answers can use cards, tables, charts and buttons, but a command's result cannot. A status board
shows as a run of lines, and "Refresh" or "Restart" must be typed again. Notifications (ADR 0010)
are Markdown text only. Issue #58 asks for the same views on both, without the agent, and for
buttons that run another tool.

## Decision
- **One view format.** A view is what `show` takes, minus choices and forms: `text`, up to 5
  `cards` (title, tone, text, fields, table, chart), up to 3 `links`, and `buttons` that run a
  `[[command]]` (`{"label", "command": "/status freach", "style"}`). Buttons and links together are
  at most 5. It is checked by the same code as the agent's views (lengths, `https` links, chart
  limits), and unknown fields are refused. Choices and forms are left out because their answers
  become a message to the agent, which these views must not need.
- **Carrier for commands: `structuredContent.okbot_view` only.** The whole `structuredContent` is
  not read as a view, because servers already return structured data for agents and automations
  (ADR 0013). The key must be named, so such a result is never read as a view by mistake. A text
  is never read as a view: tool texts often repeat what third parties wrote (log lines, ticket
  titles), and such a text must not be able to make buttons. A signed server sees `command` in
  its token (ADR 0008), so it can send a view to a command and data to an agent. A view wins over
  the text. A view that fails the checks is logged as a warning, and the chat gets the text
  instead, or "answered with a view that cannot be shown" when there is none. The server's
  content is never put into the chat as an error.
- **Always rendered, whatever `reply_mode` says.** `reply_mode` (ADR 0015) decides whether the
  *agent* is offered the `show` tool: that costs prompt space and turns, and the agent is untrusted.
  A command view comes from a server the operator configured, costs nothing, and was opted into by
  that server. So okbot shows it natively wherever the platform has components. Elsewhere the
  existing text fallbacks apply (cards as lines, buttons as `- Refresh: \`/status freach\`` to
  type), so Zalo and the console get a readable answer.
- **A button runs a command, explicitly and narrowly.** A button's `command` must name a
  `[[command]]` the view may run when the view is checked: in a command's result, a command of the
  same `[[mcp]]` server as the command that produced it (a server cannot make people run another
  server's tools); in a notification, a command its `[[notify]] commands` lists (none by default).
  It is checked again when pressed (the bot may have changed since). It must be one line of at
  most 200 characters, and never a built-in. The agent's `show` cannot make such buttons.
- **The label never hides the command.** Natively shown buttons come with a line naming what each
  runs (`-# Buttons run: /status freach · /restart freach`); text fallbacks list the commands
  themselves. A misleading label ("Acknowledge" for `/restart prod`) is visible as such. A press is a message from the presser whose text is the command
  line. It does not get the `→` that keeps choice labels from running as commands: this is the
  explicit path. The press goes through the checks a typed message gets (ADR 0015: rate limit,
  personal bot owner, the dispatcher's access gate), then the command's own `admins_only`, `roles`
  and the server's rules as the presser, with the presser's identity in the signed token. The
  first press closes the view as with any view ("→ ✅ Ana: Refresh"); the new result comes with
  fresh buttons.
- **`[[mcp]] confirm` applies to presses.** A typed command is not confirmed: the person typed
  what runs (ADR 0011). A presser saw only a label, so a press whose tool needs approval under the
  server's `confirm` (and its tool annotations) is not run. okbot shows the command line and
  "Run it?" with one **Run** button. Only the presser (or, with `confirm_by = "admins"`, the
  channel's admins: "Run it as Bob?") may press it. They are checked against the current lists at
  the press (`confirm::approvers`, the one place approvers are decided), not only the prompt's
  snapshot. An approval waits 10 minutes instead of the 7 days of other views. Running from that
  button counts as typed **by the presser**: the event carries the presser's identity, so their
  roles and `may_run` are checked again and the signed token is theirs; an admin's press only
  approves. Presses are marked by their event id. A forged mark can only add a confirmation,
  never remove one.
- **Notifications.** `POST /notify/<name>` accepts an optional `view` beside `text` (either may be
  left out, not both). It is checked in the same way, and a bad view is a 400 that says why.
  Its buttons may run only the `[[command]]`s that the route's `commands` lists (validated by
  `okbot-config`), so a token holder cannot make an admin run anything else. Links need no list.
  Whoever presses them runs the command as
  themselves, in a direct chat when the target was a person. `okbot send --view <file>` posts one.
  The history keeps the view as text, marked as another program's words.
- ADR 0007 prompts (`/interact`) are unchanged.

## Consequences
- A status board, a deploy list or a disk alert has cards and Refresh/Restart buttons even when
  the agent cannot answer, and each press costs one tool call.
- Command results are still not in the conversation history (ADR 0008). Notification views are.
- A server that sends views to old okbot versions shows its text, so the text content should stay
  readable (MCP already asks for a text copy of `structuredContent`).
- Buttons are limited to the bot's own `[[command]]`s, of the same server or listed for the
  target: a view cannot start an agent turn, a built-in command or anything a person could not
  type, and what it runs is always on screen.
