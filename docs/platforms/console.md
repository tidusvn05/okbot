# Console

`okbot chat` runs the bot in the terminal through the console channel (`console:local`). It needs
no platform account, so it is the quickest way to try a bot directory, and with the console
commands below it can stand in for group rooms, several users and other platforms' limits.

```sh
okbot chat                         # you, in a direct chat
okbot chat --like zalo             # Zalo's formatting, 2000-unit limit and typing; no buttons
okbot chat --strict                # admins, roles, rooms and [[mcp]] rules as `okbot run` applies them
okbot chat --agent echo            # no agent CLI: every message is answered with itself
okbot chat --fresh --replies replies.toml < scenario.txt   # a test; see "Scripts"
```

In a terminal, the console has line editing: arrow keys, Ctrl-R, and a history in
`<state dir>/console_history`; answers are printed above the line being typed. Ctrl-C clears the
line, Ctrl-D quits. Piped input is read as plain lines. When the bot has commands of its own
(`[[command]]`), the console lists them at start.

## Console commands

Lines that start with `:` are for the console. Everything else, including the bot's own `/`
commands, goes to the bot. Start a line with `::` to send text that begins with a colon.

| Command | Does |
|---|---|
| `:as <user>` | Writes as another user: their own direct chat, or the current room. |
| `:room <name>` | Moves to a group room. The bot answers when mentioned (`@okbot`, using `--name`). |
| `:dm` | Back to the current user's direct chat. |
| `:thread [name]` | Writes in a thread of the room; no name leaves it. Needs a preset with threads. |
| `:attach <path or url>` | Attaches a file (image, audio or file, by extension) to the next message. |
| `:restricted` | Sends a message whose content the platform withheld (Zalo's protected users). |
| `:press <n> [id=value ...]` | Presses button `n` of the newest prompt in this conversation, with form values. |
| `:paste` | The lines up to `:end` are one message (code, lists). |
| `:wait [seconds]` | Waits until the bot has handled everything sent so far, or shows a prompt (default limit 300 s). |
| `:expect <text>` | Waits as `:wait`, then checks that the bot's output since the last message (or button press) contains `text`. |
| `:expect-not <text>` | The same, checking that it does not. |
| `:where` | Shows the user, room and preset. |
| `:help` | Lists the commands. |

Output for anything but your own direct chat starts with a tag line: `[#team]`, `[#team/t1]`,
`[dm alice]`. Presets that edit messages also show ids, so `[edit out-3]` replaces `[out-3]`;
`[typing…]`, `[prompt out-5]` and `[prompt out-5 closed]` show the rest of the platform's calls.

## Presets (`--like`)

| Preset | Dialect | Limit | Threads | Edits | Typing | Buttons |
|---|---|---|---|---|---|---|
| `console` | CommonMark | 100 000 chars | – | – | – | with forms |
| `zalo` | ZaloMd | 2000 UTF-16 | – | – | yes | – (text `/approve`) |
| `gchat` | Google Chat text | 4096 chars | yes | yes | – | no forms |
| `discord` | Discord Markdown | 2000 chars | yes | yes | yes | with forms |
| `slack` | mrkdwn | 3000 chars | yes | yes | – | no forms |
| `telegram` | Telegram HTML | 4096 UTF-16 | yes | yes | yes | no forms |

The presets copy each adapter's capabilities; they test rendering, splitting and the runtime's
choices (a "thinking…" message that is edited, text answers instead of buttons), not the
platform's API.

## Who you are

The user from `--user` (default `you`) is the terminal's own user: on a personal bot they are the
owner. Users from `:as` are ordinary accounts `console:local/<user>`; on a personal bot they get
"This is a private assistant" until they pair.

Without `--strict`, `okbot chat` in a bot directory gives every console user all `[[mcp]]`
servers, roles and rooms, because you own the directory. With `--strict` it loads the whole
configuration as `okbot run` does (secrets included), except that the rate limit is off. List
console accounts like any other in `okbot.toml`:

```toml
[bot]
admins = ["console:local/you"]

[roles]
sales = ["console:local/alice"]
```

## Scripts

A scenario is a text file of input lines. `:expect` checks the answers; `okbot chat` exits once
the input ends and the last answer is out, with status 1 if any check failed, so a scenario is a
test for CI. `--fresh` starts from an empty state in a temporary directory each time.

A real agent answers differently every run and costs money. For tests, answer from rules instead:
`--agent echo` repeats each message, and `--replies <file>` follows a TOML file. The first rule
whose `when` is in the message (ignoring case) answers; a message no rule matches is echoed.

```toml
[[reply]]
when = "price"
text = "The price is 10."

[[reply]]
when = "deploy"                    # call tools first, through okbot's gateway, as an agent would
calls = [{ server = "ops", tool = "deploy", args = { env = "prod" } }]
text = "Result: {result}"          # {result}: the last call's text; {message}: the message

[[reply]]
when = "slow"
delay_ms = 3000                    # shows "thinking…" and typing on presets that have them
text = "done"

[[reply]]
when = "crash"
fail = "the agent crashed"         # the turn fails
```

Tool calls go through the gateway like a real agent's, so roles, rooms, tool lists,
confirmations and the audit log apply. A scenario that tests a confirmation, in a bot directory
whose `[[mcp]] ops` has `confirm = "always"` and `roles = ["admins"]`:

```text
please deploy
:expect Allow
:press 1
:expect deployed to prod
:as alice
please deploy
:expect-not deployed to prod
```

```sh
okbot chat --fresh --strict --replies replies.toml < deploy.txt
```

A turn waiting for a button press is not finished, so `:wait` and `:expect` also come back when a
prompt appears; press it next.

## HTTP endpoints

`okbot chat` opens a loopback port for `/interact` when a signed `[[mcp]]` server needs it. To
post to `[[notify]]` routes (with `--strict`, which loads them) or to reach `/interact` at a fixed
address, pass `--http 127.0.0.1:8090`. Use another port than the bot's own `okbot run`.
