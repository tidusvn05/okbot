# Acting on each person's own account

A bot that reads someone's calendar, issues or repositories should do it with **that person's** access, not with a
shared account that can see everything. okbot does not store third-party credentials itself; a signed `[[mcp]]`
server can, because every request names the person who asked ([ADR 0007](../adr/0007-signed-identity-and-prompts.md)).

The pattern, implemented by [`examples/mcp/accounts-server.mjs`](../../examples/mcp/accounts-server.mjs) (Node, no
dependencies):

1. The server verifies the run token on every request and reads `user` (`discord:main/1234`) and `direct`.
2. `connect_account` creates a random, one-time `state` bound to `user`, valid for ten minutes, and returns the
   provider's authorization link. The agent relays the link; the person opens it in a browser.
3. The provider redirects to the server's `/oauth/callback`; the server exchanges the code and stores the token
   under the `user` the `state` was bound to.
4. Every other tool looks up the token of the token's `user` and calls the provider with it. A person without a
   token gets "not connected yet", never someone else's data.

```toml
[[mcp]]
name = "accounts"
url = "http://127.0.0.1:8791/mcp"
token_env = "ACCOUNTS_MCP_SECRET"     # the server's OKBOT_MCP_SECRET
auth = "signed"
confirm = "destructive"               # disconnecting asks the person first
```

```sh
OKBOT_MCP_SECRET=… PUBLIC_URL=https://accounts.example.com \
OAUTH_AUTHORIZE_URL=https://github.com/login/oauth/authorize \
OAUTH_TOKEN_URL=https://github.com/login/oauth/access_token \
OAUTH_CLIENT_ID=… OAUTH_CLIENT_SECRET=… OAUTH_SCOPE=read:user \
PROFILE_URL=https://api.github.com/user \
node examples/mcp/accounts-server.mjs
```

The server listens on loopback for okbot; publish only `/oauth/callback` through your TLS reverse proxy at
`PUBLIC_URL`, and register `PUBLIC_URL/oauth/callback` as the OAuth app's callback.

## Rules that keep it safe

- **Link only in direct chats.** In a group, anyone could open the link and attach *their* account to the person
  who asked. The example refuses `connect_account` unless the token says `direct`.
- **The token never reaches the agent or the chat.** Tools return results, not credentials. The agent sees the
  authorization link, which is useless without the person's login and expires after one use.
- **Keep tokens encrypted** at rest and scoped to what the bot needs; offer `disconnect`. Refresh tokens belong in
  the server, not in okbot.
- **Combine with okbot's rules.** `roles`, `rooms` and `tools` decide who may use which tool at all
  ([README](README.md#who-where-and-which-tools)); the per-person token decides what the tool can reach for
  that person. `okbot audit` shows who called what.
- **Secrets typed in chat** (API keys, one-time codes) should go through a prompt form instead
  ([prompts](README.md#prompts-ask-the-person-not-the-agent)), which never reaches the agent or the history.
