// The built-in knowledge app (ADR 0019): documents visible to the person, searched and read.
"use strict";
(() => {
  const ADD = false;
  const t = okbot.words({
    en: { title: "Knowledge", search: "Search", empty: "Nothing here yet.", back: "← Back",
          failed: "Could not do that: {why}", add: "Add", placeholder: "A line for today's notes", saved: "Noted.",
          cut: "(cut: the document is longer)", group: "Opened from a group: only this room's and shared documents are shown." },
    vi: { title: "Tri thức", search: "Tìm", empty: "Chưa có gì.", back: "← Quay lại",
          failed: "Không làm được: {why}", add: "Thêm", placeholder: "Một dòng cho ghi chú hôm nay", saved: "Đã ghi.",
          cut: "(đã cắt bớt: tài liệu dài hơn)", group: "Mở từ nhóm: chỉ hiện tài liệu của phòng này và tài liệu chung." },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);
  const say = (text) => { $("message").textContent = text ?? ""; };
  let query = "";
  let me;

  async function open(id) {
    try {
      const d = await okbot.call("knowledge.get", { id });
      $("app").replaceChildren(
        h("a", { class: "back", onclick: () => list() }, t("back")),
        h("h2", {}, d.info.title),
        h("div", { class: "meta" }, d.info.updated ? h("span", {}, d.info.updated) : null,
          ...d.info.tags.map((tag) => h("span", { class: "badge" }, tag))),
        h("div", { class: "doc" }, d.text),
        d.truncated ? h("p", { class: "message" }, t("cut")) : null);
    } catch (e) { say(t("failed", { why: e.message })); }
  }

  async function list() {
    say("");
    const app = $("app");
    let timer;
    const search = h("input", { type: "search", placeholder: t("search"), value: query, oninput: (e) => {
      clearTimeout(timer);
      timer = setTimeout(() => { query = e.target.value.trim(); list(); }, 250);
    } });
    const parts = [h("form", { onsubmit: (e) => e.preventDefault() }, search)];
    if (ADD && me.permissions.includes("notes:write")) {
      const input = h("input", { class: "grow", maxlength: "2000", placeholder: t("placeholder"), required: true });
      parts.push(h("form", { onsubmit: async (e) => {
        e.preventDefault();
        const text = input.value.trim();
        if (!text) return;
        try { await okbot.call("notes.add", { text }); input.value = ""; say(t("saved")); list(); }
        catch (err) { say(t("failed", { why: err.message })); }
      } }, input, h("button", { type: "submit" }, t("add"))));
    }
    try {
      const r = await okbot.call("knowledge.search", { query: query || undefined });
      if (!r.personal) $("where").textContent = t("group");
      const ul = h("ul", { class: "list" });
      for (const d of r.items) {
        ul.append(h("li", { class: "single click", onclick: () => open(d.id) },
          h("div", { class: "t" }, d.title),
          h("div", { class: "meta" }, d.updated ? h("span", {}, d.updated) : null, h("span", {}, d.id))));
      }
      parts.push(r.items.length ? ul : h("p", { class: "empty" }, t("empty")));
    } catch (e) { say(t("failed", { why: e.message })); }
    // Keep the cursor in the search box while the person types; never grab focus on load.
    const typing = document.activeElement === search;
    app.replaceChildren(...parts);
    if (typing) search.focus();
  }

  (async () => {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    me = await okbot.me();
    await list();
  })().catch((e) => say(t("failed", { why: e.message })));
})();
