// The built-in Accounts app (ADR 0019, 0012): which services you connected.
"use strict";
(() => {
  const t = okbot.words({
    en: { title: "Accounts", failed: "Could not load: {why}", empty: "This bot has no services to connect.", off: "Connecting accounts is not set up on this bot.",
          yours: "your account", shared: "the bot's shared account", on: "connected", not: "not connected",
          how: "Send {cmd} to the bot in a direct chat.", group: "Open this from a direct chat with the bot." },
    vi: { title: "Tài khoản", failed: "Không tải được: {why}", empty: "Bot không có dịch vụ nào để kết nối.", off: "Bot chưa bật kết nối tài khoản.",
          yours: "tài khoản của bạn", shared: "tài khoản chung của bot", on: "đã kết nối", not: "chưa kết nối",
          how: "Gửi {cmd} cho bot trong chat riêng.", group: "Hãy mở từ chat riêng với bot." },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);

  (async () => {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    const me = await okbot.me();
    if (!me.context.direct) { $("app").append(h("p", { class: "empty" }, t("group"))); return; }
    const r = await okbot.call("connections.list");
    if (!r.enabled) { $("app").append(h("p", { class: "empty" }, t("off"))); return; }
    if (!r.items.length) { $("app").append(h("p", { class: "empty" }, t("empty"))); return; }
    const list = h("ul", { class: "list" });
    for (const c of r.items) {
      const cmd = c.connected ? (c.personal ? c.disconnect : null) : (c.personal ? c.connect : null);
      list.append(h("li", { class: "single" },
        h("div", { class: "t" }, c.server),
        h("div", { class: "meta" },
          h("span", { class: "badge" }, c.personal ? t("yours") : t("shared")),
          h("span", { class: c.connected ? "ok" : "warn" }, c.connected ? t("on") : t("not"))),
        cmd ? h("div", { class: "notes" }, t("how", { cmd })) : null));
    }
    $("app").append(list);
  })().catch((e) => { $("message").textContent = t("failed", { why: e.message }); });
})();
