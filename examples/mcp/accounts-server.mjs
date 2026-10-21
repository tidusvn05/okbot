#!/usr/bin/env node
// A reference `[[mcp]]` server that acts on each person's own account (docs/extending/accounts.md),
// in plain JavaScript with no dependencies.
//
// okbot sends a signed run token naming the person who asked (`auth = "signed"`, ADR 0007). This
// server keeps one OAuth token per person, so the agent works with *that person's* access to the
// service, never a shared account, and never sees a credential:
//
//   connect_account  → a one-time link (direct chats only) to authorize the service
//   whoami           → the person's profile at the service, with their token
//   disconnect       → forgets their token
//
// Configure with environment variables, for example for GitHub:
//   OKBOT_MCP_SECRET     the [[mcp]] token_env value okbot has (shared secret)
//   OAUTH_AUTHORIZE_URL  https://github.com/login/oauth/authorize
//   OAUTH_TOKEN_URL      https://github.com/login/oauth/access_token
//   OAUTH_CLIENT_ID / OAUTH_CLIENT_SECRET / OAUTH_SCOPE ("read:user")
//   PROFILE_URL          https://api.github.com/user
//   PUBLIC_URL           how people's browsers reach this server (the OAuth callback), e.g.
//                        https://accounts.example.com; register PUBLIC_URL/oauth/callback
//   PORT                 8791 (okbot calls http://127.0.0.1:8791/mcp)
//
// Tokens are kept in memory here; store them encrypted in a real deployment.
import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import { createServer } from "node:http";

const env = (name, fallback) => process.env[name] ?? fallback;
const SECRET = env("OKBOT_MCP_SECRET", "");
const PORT = Number(env("PORT", "8791"));
const PUBLIC_URL = env("PUBLIC_URL", `http://127.0.0.1:${PORT}`).replace(/\/$/, "");
if (!SECRET) {
  console.error("set OKBOT_MCP_SECRET to the secret in okbot's [[mcp]] token_env");
  process.exit(1);
}

/** person (`discord:main/1234`) → OAuth access token */
const tokens = new Map();
/** state → { person, expires } for links being followed */
const pending = new Map();

const b64url = (buf) => Buffer.from(buf).toString("base64url");

/** The claims of an okbot run token, or null (ADR 0007: HMAC-SHA256 over "okbot1.<claims>"). */
function verify(header) {
  const token = (header ?? "").replace(/^Bearer /, "");
  const dot = token.lastIndexOf(".");
  if (!token.startsWith("okbot1.") || dot < 0) return null;
  const signed = token.slice(0, dot);
  const expected = b64url(createHmac("sha256", SECRET).update(signed).digest());
  const given = token.slice(dot + 1);
  if (given.length !== expected.length || !timingSafeEqual(Buffer.from(given), Buffer.from(expected))) {
    return null;
  }
  const claims = JSON.parse(Buffer.from(signed.slice("okbot1.".length), "base64url").toString());
  return claims.exp > Date.now() / 1000 ? claims : null;
}

const text = (t, isError = false) => ({ content: [{ type: "text", text: t }], isError });

const tools = [
  {
    name: "connect_account",
    description: "Give the person a link to connect their own account (direct chats only).",
    inputSchema: { type: "object", properties: {} },
    annotations: { readOnlyHint: false, destructiveHint: false },
  },
  {
    name: "whoami",
    description: "The person's profile at the service, read with their own account.",
    inputSchema: { type: "object", properties: {} },
    annotations: { readOnlyHint: true },
  },
  {
    name: "disconnect",
    description: "Forget the person's connected account.",
    inputSchema: { type: "object", properties: {} },
    annotations: { readOnlyHint: false, destructiveHint: true },
  },
];

async function call(name, claims) {
  const person = claims.user;
  switch (name) {
    case "connect_account": {
      // In a group, someone else could follow the link and attach *their* account to this person.
      if (!claims.direct) return text("Ask me in a direct message to connect your account.", true);
      const state = b64url(randomBytes(24));
      pending.set(state, { person, expires: Date.now() + 10 * 60_000 });
      const url = new URL(env("OAUTH_AUTHORIZE_URL", ""));
      url.searchParams.set("client_id", env("OAUTH_CLIENT_ID", ""));
      url.searchParams.set("redirect_uri", `${PUBLIC_URL}/oauth/callback`);
      url.searchParams.set("scope", env("OAUTH_SCOPE", ""));
      url.searchParams.set("state", state);
      url.searchParams.set("response_type", "code");
      return text(`Open this link within 10 minutes to connect your account:\n${url}`);
    }
    case "whoami": {
      const token = tokens.get(person);
      if (!token) return text("Not connected yet: ask me to connect your account.", true);
      const res = await fetch(env("PROFILE_URL", ""), {
        headers: { authorization: `Bearer ${token}`, accept: "application/json", "user-agent": "okbot-example" },
      });
      if (!res.ok) return text(`The service answered ${res.status}.`, true);
      const profile = await res.json();
      return text(`Connected as ${profile.login ?? profile.email ?? profile.name ?? "an unnamed user"}.`);
    }
    case "disconnect":
      return text(tokens.delete(person) ? "Disconnected." : "Nothing was connected.");
    default:
      return null;
  }
}

async function rpc(msg, claims) {
  const ok = (result) => ({ jsonrpc: "2.0", id: msg.id, result });
  switch (msg.method) {
    case "initialize":
      return ok({
        protocolVersion: msg.params?.protocolVersion ?? "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "accounts-example", version: "1" },
      });
    case "ping":
      return ok({});
    case "tools/list":
      return ok({ tools });
    case "tools/call": {
      const result = await call(msg.params?.name, claims);
      return result
        ? ok(result)
        : { jsonrpc: "2.0", id: msg.id, error: { code: -32602, message: "unknown tool" } };
    }
    default:
      return { jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "method not found" } };
  }
}

/** The OAuth redirect: exchanges the code and keeps the token for the person in `state`. */
async function callback(url, res) {
  const state = url.searchParams.get("state") ?? "";
  const link = pending.get(state);
  pending.delete(state);
  if (!link || link.expires < Date.now()) {
    res.writeHead(400).end("This link has expired; ask the bot for a new one.");
    return;
  }
  const exchange = await fetch(env("OAUTH_TOKEN_URL", ""), {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      client_id: env("OAUTH_CLIENT_ID", ""),
      client_secret: env("OAUTH_CLIENT_SECRET", ""),
      code: url.searchParams.get("code") ?? "",
      redirect_uri: `${PUBLIC_URL}/oauth/callback`,
      grant_type: "authorization_code",
    }),
  });
  const body = await exchange.json().catch(() => ({}));
  if (!body.access_token) {
    res.writeHead(502).end("The service did not give a token.");
    return;
  }
  tokens.set(link.person, body.access_token);
  console.error(`connected an account for ${link.person}`);
  res.writeHead(200, { "content-type": "text/plain; charset=utf-8" }).end("Connected. You can go back to the chat.");
}

createServer(async (req, res) => {
  const url = new URL(req.url, PUBLIC_URL);
  if (req.method === "GET" && url.pathname === "/oauth/callback") return callback(url, res);
  if (url.pathname !== "/mcp") return res.writeHead(404).end();
  if (req.method === "DELETE") return res.writeHead(200).end();
  if (req.method !== "POST") return res.writeHead(405).end();
  const claims = verify(req.headers.authorization);
  if (!claims) return res.writeHead(401).end();
  let raw = "";
  for await (const chunk of req) raw += chunk;
  const msg = JSON.parse(raw || "{}");
  if (msg.id === undefined) return res.writeHead(202).end(); // a notification
  const reply = await rpc(msg, claims);
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(reply));
}).listen(PORT, "127.0.0.1", () => console.error(`accounts example on http://127.0.0.1:${PORT}/mcp`));
