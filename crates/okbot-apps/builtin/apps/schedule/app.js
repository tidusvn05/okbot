// The built-in Schedule app (ADR 0019): what fires in the coming days.
"use strict";
(() => {
  const t = okbot.words({
    en: { title: "Schedule", week: "7 days", month: "30 days", empty: "Nothing is scheduled.", failed: "Could not load: {why}",
          reminder: "reminder", routine: "routine", todo: "to-do", automation: "automation", snooze: "Snooze 1h", saved: "Saved.",
          where: "Times in {tz}." },
    vi: { title: "Lịch", week: "7 ngày", month: "30 ngày", empty: "Không có lịch nào.", failed: "Không tải được: {why}",
          reminder: "nhắc nhở", routine: "routine", todo: "việc", automation: "automation", snooze: "Hoãn 1 giờ", saved: "Đã lưu.",
          where: "Giờ theo {tz}." },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);
  let days = okbot.view.path === "/month" ? 30 : 7;
  let me;

  async function load() {
    const app = $("app");
    app.replaceChildren(
      h("form", { onsubmit: (e) => e.preventDefault() },
        h("button", { type: "button", class: days === 7 ? "primary" : "", onclick: () => { days = 7; load(); } }, t("week")),
        h("button", { type: "button", class: days === 30 ? "primary" : "", onclick: () => { days = 30; load(); } }, t("month"))));
    try {
      const r = await okbot.call("schedule.list", { days });
      $("where").textContent = t("where", { tz: r.timezone });
      if (!r.items.length) { app.append(h("p", { class: "empty" }, t("empty"))); return; }
      let day = "";
      let list = null;
      for (const item of r.items) {
        const d = okbot.when(item.at, r.timezone, { weekday: "long", day: "2-digit", month: "2-digit" });
        if (d !== day) {
          day = d;
          list = h("ul", { class: "list" });
          app.append(h("div", { class: "day" }, d), list);
        }
        const actions = h("div", { class: "actions" });
        if (item.kind === "reminder" && me.permissions.includes("tasks:write")) {
          actions.append(h("button", { type: "button", onclick: async () => {
            try { await okbot.call("tasks.snooze", { id: item.task, until: "1h" }); $("message").textContent = t("saved"); load(); }
            catch (e) { $("message").textContent = t("failed", { why: e.message }); }
          } }, t("snooze")));
        }
        list.append(h("li", {},
          h("div", { class: "t" }, okbot.when(item.at, r.timezone, { hour: "2-digit", minute: "2-digit" }) + " · " + item.title),
          h("div", { class: "meta" }, h("span", { class: "badge" }, t(item.kind)), item.repeat ? h("span", {}, item.repeat) : null),
          actions));
      }
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
