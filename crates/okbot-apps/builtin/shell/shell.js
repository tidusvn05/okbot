// okbot app shell (ADR 0019). Trusted code: it holds the session and is the only thing that
// talks to the App API. Apps run in a sandboxed frame and reach the API through postMessage.
"use strict";
(() => {
  const T = {
    en: {
      signing: "Signing in…", expired: "This link has expired or was already used.",
      again: "Send {cmd} to the bot for a new one.", signedOut: "You are signed out.",
      open: "Open an app from the chat: press its button, or send {cmd}.",
      logout: "Log out", direct: "Direct chat", unverified: "unverified",
      leave: "Leave okbot and open {host}?", cancel: "Cancel", go: "Open", ok: "OK",
      confirm: "Continue", error: "Something went wrong. Try again from the chat.",
      choose: "Choose an app above.", none: "No apps are available here.",
    },
    vi: {
      signing: "Đang đăng nhập…", expired: "Liên kết đã hết hạn hoặc đã được dùng.",
      again: "Gửi {cmd} cho bot để lấy liên kết mới.", signedOut: "Bạn đã đăng xuất.",
      open: "Mở app từ chat: bấm nút của app, hoặc gửi {cmd}.",
      logout: "Đăng xuất", direct: "Chat riêng", unverified: "chưa kiểm duyệt",
      leave: "Rời okbot và mở {host}?", cancel: "Huỷ", go: "Mở", ok: "OK",
      confirm: "Tiếp tục", error: "Có lỗi xảy ra. Hãy mở lại từ chat.",
      choose: "Chọn một app ở trên.", none: "Không có app nào ở đây.",
    },
  };
  const lang = (navigator.language || "en").slice(0, 2) === "vi" ? "vi" : "en";
  let words = T[lang];
  const t = (key, vars = {}) =>
    words[key].replace(/\{(\w+)\}/g, (_, k) => vars[k] ?? "");

  const $ = (id) => document.getElementById(id);
  const status = (title, detail) => {
    const main = $("main");
    main.replaceChildren();
    const p = document.createElement("p");
    p.id = "status";
    p.setAttribute("role", "status");
    if (title) {
      const s = document.createElement("strong");
      s.textContent = title;
      p.append(s);
    }
    if (detail) p.append(detail);
    main.append(p);
  };

  let session = null;
  let frame = null;
  let current = null; // { app, view, params }

  // The ticket is in the fragment, which browsers never send to servers; drop it at once.
  const hash = new URLSearchParams(location.hash.slice(1));
  const ticket = hash.get("t");
  if (ticket) history.replaceState(null, "", location.pathname + location.search);

  async function post(path, body, headers = {}) {
    const response = await fetch(path, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", ...headers },
      body: JSON.stringify(body ?? {}),
    });
    let data = null;
    try { data = await response.json(); } catch { data = null; }
    return { ok: response.ok, status: response.status, data };
  }

  async function api(method, params, app) {
    const r = await post("api/v1/" + encodeURIComponent(method), params, {
      "x-okbot-csrf": session.csrf,
      "x-okbot-app": app,
      "x-okbot-context": String(session.context),
    });
    if (r.status === 401) {
      session = null;
      status(t("signedOut"), t("open", { cmd: "/app" }));
      $("bar").hidden = true;
      throw { code: "signed_out", message: "signed out" };
    }
    if (!r.ok || !r.data || r.data.error) {
      throw r.data?.error ?? { code: "failed", message: "HTTP " + r.status };
    }
    return r.data.result;
  }

  function dialog(text, buttons) {
    return new Promise((resolve) => {
      const back = document.createElement("div");
      back.className = "dialog";
      const box = document.createElement("div");
      box.setAttribute("role", "dialog");
      box.setAttribute("aria-modal", "true");
      const p = document.createElement("p");
      p.textContent = text;
      const row = document.createElement("div");
      row.className = "row";
      for (const [label, value, primary] of buttons) {
        const b = document.createElement("button");
        b.type = "button";
        b.textContent = label;
        if (primary) b.className = "primary";
        b.onclick = () => { back.remove(); resolve(value); };
        row.append(b);
      }
      box.append(p, row);
      back.append(box);
      document.body.append(back);
      row.lastChild?.focus();
    });
  }

  function renderBar() {
    $("bar").hidden = false;
    $("logout").textContent = t("logout");
    const who = $("who");
    who.textContent = session.account.name + " · " + session.account.platform;
    const nav = $("apps");
    nav.replaceChildren();
    for (const app of session.apps) {
      const a = document.createElement("a");
      a.href = "#app=" + encodeURIComponent(app.name);
      a.textContent = app.title;
      if (current && app.name === current.app) a.setAttribute("aria-current", "page");
      a.onclick = (e) => { e.preventDefault(); show({ app: app.name, view: app.view, params: {} }); };
      nav.append(a);
    }
    // On a narrow screen the current app may be off to the side.
    nav.querySelector("[aria-current]")?.scrollIntoView({ block: "nearest", inline: "nearest" });
    const select = $("context");
    select.replaceChildren();
    for (const c of session.contexts) {
      const o = document.createElement("option");
      o.value = String(c.id);
      o.textContent = c.direct ? t("direct") : c.label;
      if (c.id === session.context) o.selected = true;
      select.append(o);
    }
    select.hidden = session.contexts.length < 2;
  }

  function show(open) {
    current = open;
    const app = session.apps.find((a) => a.name === open.app);
    renderBar();
    if (!app) {
      status(session.apps.length ? t("choose") : t("none"));
      return;
    }
    const who = $("who");
    if (app.unverified) {
      const u = document.createElement("span");
      u.className = "unverified";
      u.textContent = "· " + app.name + " " + t("unverified");
      who.append(u);
    }
    const params = new URLSearchParams({ view: open.view || app.view, lang });
    for (const [k, v] of Object.entries(open.params || {})) params.set("p_" + k, v);
    frame = document.createElement("iframe");
    frame.title = app.title;
    // No allow-same-origin: the app gets an opaque origin, no cookies, no API of its own.
    frame.setAttribute("sandbox", "allow-scripts allow-forms");
    frame.setAttribute("referrerpolicy", "no-referrer");
    frame.src = "frame/" + encodeURIComponent(app.name) + "/index.html#" + params;
    $("main").replaceChildren(frame);
    document.title = app.title + " · " + session.bot;
  }

  window.addEventListener("message", async (event) => {
    if (!frame || event.source !== frame.contentWindow) return;
    const m = event.data;
    if (!m || m.okbot !== 1 || typeof m.method !== "string") return;
    const reply = (body) => frame.contentWindow.postMessage({ okbot: 1, id: m.id, ...body }, "*");
    const app = current.app;
    try {
      if (m.method === "shell.openLink") {
        let url;
        try { url = new URL(String(m.params?.url)); } catch { throw { code: "invalid", message: "not a URL" }; }
        if (url.protocol !== "https:") throw { code: "invalid", message: "only https links" };
        const go = await dialog(t("leave", { host: url.host }), [[t("cancel"), false], [t("go"), true, true]]);
        if (go) window.open(url.href, "_blank", "noopener,noreferrer");
        reply({ result: go });
        return;
      }
      if (m.method === "shell.confirm") {
        const text = String(m.params?.text ?? "").slice(0, 500);
        const yes = await dialog(text, [[t("cancel"), false], [t("confirm"), true, true]]);
        reply({ result: yes });
        return;
      }
      if (m.method === "shell.title") {
        document.title = String(m.params?.title ?? "").slice(0, 80) + " · " + session.bot;
        reply({ result: true });
        return;
      }
      if (m.method === "mcp.call") {
        // Only the person approves: whatever the app sends, `confirmed` is the shell's.
        const params = { ...(m.params ?? {}) };
        delete params.confirmed;
        let result = await api("mcp.call", params, app);
        if (result && typeof result.confirm === "string") {
          const yes = await dialog(result.confirm, [[t("cancel"), false], [t("confirm"), true, true]]);
          if (!yes) throw { code: "declined", message: "declined" };
          result = await api("mcp.call", { ...params, confirmed: true }, app);
        }
        reply({ result });
        return;
      }
      reply({ result: await api(m.method, m.params ?? {}, app) });
    } catch (error) {
      reply({ error: { code: error.code ?? "failed", message: error.message ?? String(error) } });
    }
  });

  $("logout").onclick = async () => {
    await post("api/logout", {}, { "x-okbot-csrf": session?.csrf ?? "" });
    session = null;
    $("bar").hidden = true;
    status(t("signedOut"), t("open", { cmd: "/app" }));
  };

  $("context").onchange = async (e) => {
    session.context = Number(e.target.value);
    if (current) show(current);
  };

  async function start() {
    status(t("signing"));
    const r = await post("api/session", ticket ? { ticket } : {});
    if (!r.ok) {
      const code = r.data?.error?.code;
      if (code === "expired") status(t("expired"), t("again", { cmd: r.data.error.command ?? "/app" }));
      else if (code === "signed_out") status(t("signedOut"), t("open", { cmd: "/app" }));
      else status(t("error"), r.data?.error?.message ?? "");
      return;
    }
    session = r.data;
    if (session.locale && T[session.locale.slice(0, 2)]) words = T[session.locale.slice(0, 2)];
    const wanted = session.open ?? (hash.get("app") ? { app: hash.get("app"), view: "", params: {} } : null);
    if (wanted) show(wanted);
    else {
      renderBar();
      status(session.apps.length ? t("choose") : t("none"));
    }
  }
  start().catch(() => status(t("error")));
})();
