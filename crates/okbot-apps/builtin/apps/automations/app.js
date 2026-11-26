// The built-in Automations app (ADR 0019, 0013): what each automation does and did.
"use strict";
(() => {
  const t = okbot.words({
    en: { title: "Automations", empty: "No [[automation]] in okbot.toml.", failed: "Could not load: {why}", paused: "paused",
          acted: "{n} run(s) with new items in the last 24 h (cap {max} a day)", none: "no runs yet", items: "{n} item(s)",
          note: "Automations come from okbot.toml; change them there." },
    vi: { title: "Tự động hoá", empty: "Không có [[automation]] nào trong okbot.toml.", failed: "Không tải được: {why}", paused: "tạm dừng",
          acted: "{n} lần chạy có mục mới trong 24 giờ qua (tối đa {max} lần/ngày)", none: "chưa chạy lần nào", items: "{n} mục",
          note: "Automation lấy từ okbot.toml; sửa ở đó." },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);

  (async () => {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    $("where").textContent = t("note");
    const me = await okbot.me();
    const r = await okbot.call("automations.list");
    if (!r.items.length) { $("app").append(h("p", { class: "empty" }, t("empty"))); return; }
    const list = h("ul", { class: "list" });
    for (const a of r.items) {
      const runs = a.runs.length
        ? h("div", { class: "notes" }, a.runs.map((run) =>
            h("div", { class: run.error ? "warn" : "" },
              okbot.when(run.at, me.timezone) + " · " + run.action + " · " + t("items", { n: run.items }) + (run.error ? " — " + run.error : ""))))
        : h("div", { class: "notes" }, t("none"));
      list.append(h("li", { class: "single" },
        h("div", { class: "t" }, a.name + (a.paused ? " (" + t("paused") + ")" : "")),
        h("div", { class: "meta" },
          h("span", { class: "badge" }, a.trigger),
          a.source ? h("span", {}, a.source) : null,
          h("span", {}, "→ " + a.action),
          h("span", {}, t("acted", { n: a.acted_last_24h, max: a.max_runs_per_day }))),
        runs));
    }
    $("app").append(list);
  })().catch((e) => { $("message").textContent = t("failed", { why: e.message }); });
})();
