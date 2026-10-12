#!/usr/bin/env node
// A reference okbot bridge adapter (protocol 1, docs/bridge.md), in plain JavaScript.
//
// It "connects" to an imaginary platform: right after `initialize` it reports one inbound message
// (settings.greeting), and it prints every message okbot sends to stderr, which okbot logs.
// Replace the marked parts with your platform's SDK.
import { createInterface } from "node:readline";

let channel = "";
let settings = {};
let nextMessage = 1;

const write = (msg) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...msg }) + "\n");
const reply = (id, result) => write({ id, result });
const fail = (id, message, code = -32000) => write({ id, error: { code, message } });
const notify = (method, params) => write({ method, params });

function inbound(room, user, text) {
  // PLATFORM: call this for each message your platform delivers.
  notify("inbound", {
    event: {
      event_id: `demo-${Date.now()}-${Math.random()}`,
      conversation: { channel, space: null, room, thread: null },
      author: { id: user, display_name: user, is_bot: false },
      text,
      attachments: [],
      mentions_bot: false,
      is_direct: true,
      restricted: false,
      message_id: null,
      sent_at: new Date().toISOString(),
    },
  });
}

const handlers = {
  initialize(params) {
    channel = params.channel;
    settings = params.settings ?? {};
    // PLATFORM: connect here.
    setTimeout(() => inbound("demo-room", "demo-user", settings.greeting ?? "hello"), 100);
    return { protocol: 1, name: "echo-channel" };
  },
  send({ target, text }) {
    // PLATFORM: post `text` to target.conversation.room.
    console.error(`send to ${target.conversation.room}: ${text}`);
    return { message_id: `m${nextMessage++}` };
  },
  edit({ message_id, text }) {
    console.error(`edit ${message_id}: ${text}`);
    return {};
  },
  typing() {
    return {};
  },
  shutdown() {
    setTimeout(() => process.exit(0), 10);
    return {};
  },
};

createInterface({ input: process.stdin }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const handler = handlers[msg.method];
  if (msg.id === undefined) return; // okbot sends no notifications today
  if (!handler) return fail(msg.id, `unknown method ${msg.method}`, -32601);
  try {
    reply(msg.id, handler(msg.params ?? {}));
  } catch (e) {
    fail(msg.id, String(e));
  }
});
