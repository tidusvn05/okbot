// okbot App SDK v1 (ADR 0019). Load it from an app page:
//   <script src="../../sdk/v1.js"></script>
// The page runs in a sandboxed frame without network access; every call goes to okbot's shell,
// which checks the app's permissions and calls the App API as the signed-in person.
"use strict";
(() => {
  const pending = new Map();
  const listeners = new Map();
  let seq = 0;
  const hash = new URLSearchParams(location.hash.slice(1));
  const params = {};
  for (const [k, v] of hash) if (k.startsWith("p_")) params[k.slice(2)] = v;

  // The shell hands this page a MessageChannel port once; every call and answer goes through it.
  let port = null;
  const queue = [];

  function receive(m) {
    if (!m || m.okbot !== 1) return;
    if (m.id && pending.has(m.id)) {
      const { resolve, reject } = pending.get(m.id);
      pending.delete(m.id);
      if (m.error) {
        const e = new Error(m.error.message);
        e.code = m.error.code;
        reject(e);
      } else resolve(m.result);
    } else if (m.event && listeners.has(m.event)) {
      for (const f of listeners.get(m.event)) f(m.data);
    }
  }

  window.addEventListener("message", (event) => {
    // The frame may only be embedded by okbot's shell (frame-ancestors), so the parent is it.
    if (port || event.source !== window.parent) return;
    if (!event.data || event.data.okbot !== 1 || !event.data.port || !event.ports[0]) return;
    port = event.ports[0];
    port.onmessage = (e) => receive(e.data);
    for (const m of queue.splice(0)) port.postMessage(m);
  });

  function call(method, args) {
    return new Promise((resolve, reject) => {
      const id = ++seq;
      pending.set(id, { resolve, reject });
      const m = { okbot: 1, id, method, params: args ?? {} };
      if (port) port.postMessage(m);
      else queue.push(m);
    });
  }

  window.okbot = Object.freeze({
    /** Calls an App API method, such as "tasks.list". */
    call,
    /** Who is signed in, where from, the locale and time zone. */
    me: () => call("me"),
    /** Asks the person in okbot's own dialog; resolves to true or false. */
    confirm: (text) => call("shell.confirm", { text }),
    /** Opens an https link after the person agrees to leave. */
    openLink: (url) => call("shell.openLink", { url }),
    /** Offers an artifact's file for download, after the person agrees in okbot's dialog
     *  (needs `artifacts:read`); resolves to true when it was saved. */
    download: ({ id, v } = {}) => call("shell.download", { id, v }),
    /** Sets the page title. */
    setTitle: (title) => call("shell.title", { title }),
    /** The view chat opened ("/open") and its parameters. */
    view: Object.freeze({ path: hash.get("view") || "/", params: Object.freeze(params) }),
    /** The interface language chosen by the shell ("en", "vi"). */
    lang: hash.get("lang") || "en",
    /** Builds an element: `h("li", {class: "x", onclick}, "text", child)`. Text is never HTML. */
    h(tag, props, ...children) {
      const el = document.createElement(tag);
      for (const [k, v] of Object.entries(props ?? {})) {
        if (v === undefined || v === null || v === false) continue;
        if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
        else if (k === "class") el.className = v;
        else el.setAttribute(k, v === true ? "" : String(v));
      }
      for (const c of children.flat()) {
        if (c === undefined || c === null || c === false) continue;
        el.append(c instanceof Node ? c : document.createTextNode(String(c)));
      }
      return el;
    },
    /** `t(key, vars)` over `{en: {...}, vi: {...}}` in the shell's language. */
    words(dict) {
      const w = dict[hash.get("lang") || "en"] ?? dict.en;
      return (key, vars = {}) => (w[key] ?? dict.en[key] ?? key).replace(/\{(\w+)\}/g, (_, k) => vars[k] ?? "");
    },
    /** A local date and time for an RFC 3339 string, in `timeZone` when given. */
    when(iso, timeZone, options) {
      const lang = (hash.get("lang") || "en") === "vi" ? "vi-VN" : undefined;
      return new Date(iso).toLocaleString(lang, {
        timeZone, ...(options ?? { weekday: "short", day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" }),
      });
    },
    /** Listens to shell events. */
    on(name, f) {
      if (!listeners.has(name)) listeners.set(name, []);
      listeners.get(name).push(f);
    },
  });
})();
