// The built-in Memory app (ADR 0019): what the bot remembers, and forgetting it.
"use strict";
(() => {
  const t = okbot.words({
    en: { title: "Memory", personal: "about you", room: "this room", everyone: "everyone", forget: "Forget",
          sure: "Forget “{title}”?", empty: "The bot remembers nothing here.", failed: "Could not do that: {why}",
          done: "Forgotten.", group: "Opened from a group: only this room's and everyone's memory is shown." },
    vi: { title: "Ghi nhớ", personal: "về bạn", room: "phòng này", everyone: "mọi người", forget: "Quên",
          sure: "Quên “{title}”?", empty: "Bot không nhớ gì ở đây.", failed: "Không làm được: {why}",
          done: "Đã quên.", group: "Mở từ nhóm: chỉ hiện ghi nhớ của phòng này và của mọi người." },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);
  let me;

  async function load() {
    const app = $("app");
    app.replaceChildren();
    try {
      const r = await okbot.call("memory.list");
      if (!r.personal) $("where").textContent = t("group");
      if (!r.items.length) { app.append(h("p", { class: "empty" }, t("empty"))); return; }
      const list = h("ul", { class: "list" });
      for (const e of r.items) {
        const actions = h("div", { class: "actions" });
        if (me.permissions.includes("memory:write") && (e.scope !== "everyone" || e.mine || me.admin)) {
          actions.append(h("button", { type: "button", onclick: async () => {
            if (!(await okbot.confirm(t("sure", { title: e.title })))) return;
            try { await okbot.call("memory.forget", { id: e.id }); $("message").textContent = t("done"); load(); }
            catch (err) { $("message").textContent = t("failed", { why: err.message }); }
          } }, t("forget")));
        }
        list.append(h("li", {},
          h("div", { class: "t" }, e.title),
          h("div", { class: "meta" }, h("span", { class: "badge" }, t(e.scope)), h("span", {}, e.updated)),
          actions,
          h("div", { class: "notes" }, e.text)));
      }
      app.append(list);
    } catch (e) {
      $("message").textContent = t("failed", { why: e.message });
    }
  }

  (async () => {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    me = await okbot.me();
    await load();
  })().catch((e) => { $("message").textContent = t("failed", { why: e.message }); });
})();
