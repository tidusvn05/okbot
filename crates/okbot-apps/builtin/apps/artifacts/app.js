// The built-in artifacts app (ADR 0021): the pages, documents, tables and diagrams the assistant
// made, listed, shown, downloaded, renamed and deleted. HTML, SVG and Mermaid are shown in
// okbot's viewer: a frame of its own, without network or access to this app.
"use strict";
(() => {
  const t = okbot.words({
    en: {
      title: "Artifacts", search: "Search", empty: "Nothing here yet. Ask the assistant for a page, a table or a diagram.",
      back: "← All artifacts", failed: "Could not do that: {why}", any: "Any kind", everywhere: "Mine and this room's",
      personal: "Mine", room: "This room's", group: "Opened from a group: only this room's artifacts are shown.",
      v: "v{n}", updated: "updated {when}", rename: "Rename", del: "Delete", download: "Download", save: "Save",
      cancel: "Cancel", sure: "Delete “{title}” with all its versions?", deleted: "Deleted.", renamed: "Renamed.",
      banner: "Made by the assistant. Never enter passwords here.", left: "The page tried to open another page, so it was closed.",
      reload: "Show it again", versions: "Versions", pruned: "removed (older than the versions kept)",
      rows: "Showing the first {n} rows.", filter: "Filter rows", newer: "This is version {n}; the newest is {last}.",
      newest: "Show the newest",
    },
    vi: {
      title: "Tài liệu đã tạo", search: "Tìm", empty: "Chưa có gì. Hãy nhờ trợ lý làm một trang, một bảng hay một sơ đồ.",
      back: "← Tất cả", failed: "Không làm được: {why}", any: "Mọi loại", everywhere: "Của tôi và của phòng này",
      personal: "Của tôi", room: "Của phòng này", group: "Mở từ nhóm: chỉ hiện tài liệu của phòng này.",
      v: "v{n}", updated: "cập nhật {when}", rename: "Đổi tên", del: "Xoá", download: "Tải về", save: "Lưu",
      cancel: "Huỷ", sure: "Xoá “{title}” cùng mọi phiên bản?", deleted: "Đã xoá.", renamed: "Đã đổi tên.",
      banner: "Do trợ lý tạo. Đừng bao giờ nhập mật khẩu ở đây.", left: "Trang này đã cố mở một trang khác nên đã bị đóng.",
      reload: "Hiện lại", versions: "Phiên bản", pruned: "đã xoá (cũ hơn số phiên bản được giữ)",
      rows: "Đang hiện {n} dòng đầu.", filter: "Lọc dòng", newer: "Đây là phiên bản {n}; mới nhất là {last}.",
      newest: "Xem bản mới nhất",
    },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);
  const say = (text) => { $("message").textContent = text ?? ""; };
  const KINDS = ["html", "markdown", "svg", "mermaid", "csv", "json", "text"];
  const MAX_ROWS = 5000;
  const filters = { query: "", kind: "", scope: "" };
  let me;
  let tz;

  const size = (n) => (n < 1024 ? n + " B" : n < 1 << 20 ? (n / 1024).toFixed(1) + " KB" : (n / (1 << 20)).toFixed(1) + " MB");
  const when = (iso) => okbot.when(iso, tz);
  const canWrite = () => me.permissions.includes("artifacts:write");

  async function list() {
    say("");
    okbot.setTitle(t("title"));
    let timer;
    const search = h("input", { type: "search", placeholder: t("search"), value: filters.query, oninput: (e) => {
      clearTimeout(timer);
      timer = setTimeout(() => { filters.query = e.target.value.trim(); list(); }, 250);
    } });
    const kind = h("select", { onchange: (e) => { filters.kind = e.target.value; list(); } },
      h("option", { value: "" }, t("any")), ...KINDS.map((k) => h("option", { value: k, selected: filters.kind === k }, k)));
    const scope = h("select", { onchange: (e) => { filters.scope = e.target.value; list(); } },
      ...[["", t("everywhere")], ["personal", t("personal")], ["room", t("room")]].map(([v, label]) =>
        h("option", { value: v, selected: filters.scope === v }, label)));
    const parts = [h("form", { onsubmit: (e) => e.preventDefault() }, search, kind, scope)];
    try {
      const r = await okbot.call("artifacts.list", {
        query: filters.query || undefined, kind: filters.kind || undefined, scope: filters.scope || undefined,
      });
      $("where").textContent = r.personal ? "" : t("group");
      scope.hidden = !r.personal;
      const ul = h("ul", { class: "list" });
      for (const a of r.items) {
        ul.append(h("li", { class: "single click", "data-id": a.id, onclick: () => show(a.id) },
          h("div", { class: "t" }, a.title),
          h("div", { class: "meta" },
            h("span", { class: "badge" }, a.kind),
            h("span", {}, a.scope === "room" ? t("room") : t("personal")),
            h("span", {}, t("v", { n: a.version })),
            h("span", {}, t("updated", { when: when(a.updated) })),
            h("span", {}, size(a.bytes))),
          a.summary ? h("div", { class: "notes" }, a.summary) : null));
      }
      parts.push(r.items.length ? ul : h("p", { class: "empty" }, t("empty")));
    } catch (e) { say(t("failed", { why: e.message })); }
    const typing = document.activeElement === search;
    $("app").replaceChildren(...parts.filter(Boolean));
    if (typing) search.focus();
  }

  // The viewer frame: sandboxed, without allow-same-origin, and closed when it loads a second
  // page (a link or a script navigating it), as the shell does for apps.
  async function viewer(id, v) {
    const box = h("div", {});
    const open = async () => {
      const r = await okbot.call("artifacts.open", { id, v });
      const frame = h("iframe", { class: "viewer", sandbox: "allow-scripts", referrerpolicy: "no-referrer", title: r.title });
      let loads = 0;
      frame.addEventListener("load", () => {
        loads += 1;
        if (loads > 1) {
          frame.remove();
          box.replaceChildren(h("p", { class: "banner" }, t("left")),
            h("button", { type: "button", onclick: () => open().catch((e) => say(t("failed", { why: e.message }))) }, t("reload")));
        }
      });
      frame.src = r.url;
      box.replaceChildren(h("p", { class: "banner" }, t("banner")), frame);
    };
    await open();
    return box;
  }

  function csvRows(text) {
    const rows = [];
    let row = [];
    let cell = "";
    let quoted = false;
    const sep = (text.split("\n", 1)[0].match(/;/g) || []).length > (text.split("\n", 1)[0].match(/,/g) || []).length ? ";" : ",";
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      if (quoted) {
        if (c === '"' && text[i + 1] === '"') { cell += '"'; i++; }
        else if (c === '"') quoted = false;
        else cell += c;
      } else if (c === '"' && cell === "") quoted = true;
      else if (c === sep) { row.push(cell); cell = ""; }
      else if (c === "\n" || c === "\r") {
        if (c === "\r" && text[i + 1] === "\n") i++;
        row.push(cell); cell = "";
        if (row.length > 1 || row[0] !== "") rows.push(row);
        row = [];
        if (rows.length > MAX_ROWS) break;
      } else cell += c;
    }
    if (cell !== "" || row.length) { row.push(cell); rows.push(row); }
    return rows;
  }

  function table(text) {
    const all = csvRows(text);
    const head = all.shift() ?? [];
    const body = all.slice(0, MAX_ROWS);
    let order = { col: -1, dir: 1 };
    let query = "";
    const tbody = h("tbody");
    const ths = head.map((name, k) => h("th", { scope: "col", onclick: () => {
      order = { col: k, dir: order.col === k ? -order.dir : 1 };
      ths.forEach((th, j) => th.setAttribute("aria-sort", j === k ? (order.dir > 0 ? "ascending" : "descending") : "none"));
      fill();
    } }, name));
    const num = (s) => (/^-?[\d.,]+$/.test(s.trim()) ? parseFloat(s.replace(/,/g, "")) : NaN);
    function fill() {
      let rows = body.filter((r) => !query || r.some((c) => c.toLowerCase().includes(query)));
      if (order.col >= 0) {
        const k = order.col;
        rows = [...rows].sort((a, b) => {
          const x = a[k] ?? "", y = b[k] ?? "";
          const nx = num(x), ny = num(y);
          return order.dir * (!isNaN(nx) && !isNaN(ny) ? nx - ny : x.localeCompare(y));
        });
      }
      tbody.replaceChildren(...rows.map((r) => h("tr", {}, ...head.map((_, k) => h("td", {}, r[k] ?? "")))));
    }
    fill();
    return h("div", {},
      h("form", { onsubmit: (e) => e.preventDefault() },
        h("input", { type: "search", class: "grow", placeholder: t("filter"), oninput: (e) => { query = e.target.value.trim().toLowerCase(); fill(); } })),
      h("div", { class: "table-wrap" }, h("table", {}, h("thead", {}, h("tr", {}, ...ths)), tbody)),
      all.length > MAX_ROWS ? h("p", { class: "hint" }, t("rows", { n: MAX_ROWS })) : null);
  }

  function tree(text) {
    let value;
    try { value = JSON.parse(text); } catch { return h("pre", { class: "text" }, text); }
    let budget = 5000;
    const node = (key, v, depth) => {
      budget -= 1;
      const label = key === null ? [] : [h("span", { class: "k" }, JSON.stringify(key)), ": "];
      if (v !== null && typeof v === "object") {
        const entries = Array.isArray(v) ? v.map((x, i) => [i, x]) : Object.entries(v);
        const d = h("details", { open: depth < 2 },
          h("summary", {}, ...label, Array.isArray(v) ? `[${entries.length}]` : `{${entries.length}}`));
        for (const [k, x] of entries) {
          if (budget <= 0) { d.append(h("div", { class: "leaf" }, "…")); break; }
          d.append(node(Array.isArray(v) ? null : k, x, depth + 1));
        }
        return d;
      }
      const cls = typeof v === "string" ? "s" : "n";
      return h("div", { class: "leaf" }, ...label, h("span", { class: cls }, JSON.stringify(v)));
    };
    return h("div", { class: "tree" }, node(null, value, 0));
  }

  function content(kind, text) {
    if (kind === "markdown") {
      const div = h("div", { class: "md" });
      div.append(window.markdown.render(text, { openLink: (url) => okbot.openLink(url) }));
      return div;
    }
    if (kind === "csv") return table(text);
    if (kind === "json") return tree(text);
    return h("pre", { class: "text" }, text);
  }

  async function show(id, v) {
    say("");
    let r;
    try { r = await okbot.call("artifacts.get", { id, v }); }
    catch (e) { say(t("failed", { why: e.message })); return list(); }
    const a = r.artifact;
    okbot.setTitle(a.title);
    const title = h("h2", {}, a.title);
    const actions = h("div", { class: "row" },
      h("button", { type: "button", onclick: () => okbot.download({ id: a.id, v: r.v }).catch((e) => say(t("failed", { why: e.message }))) }, t("download")));
    if (canWrite()) {
      actions.append(
        h("button", { type: "button", onclick: () => rename(a, title, r.v) }, t("rename")),
        h("button", { type: "button", onclick: async () => {
          if (!(await okbot.confirm(t("sure", { title: a.title })))) return;
          try { await okbot.call("artifacts.delete", { id: a.id }); say(t("deleted")); list(); }
          catch (e) { say(t("failed", { why: e.message })); }
        } }, t("del")));
    }
    const versions = h("ul", { class: "versions", "aria-label": t("versions") },
      ...a.versions.map((x) => h("li", {}, h("button", {
        type: "button", disabled: x.pruned, "aria-current": x.n === r.v ? "true" : undefined,
        title: x.pruned ? t("pruned") : [when(x.at), size(x.bytes), x.note].filter(Boolean).join(" · "),
        onclick: () => show(a.id, x.n),
      }, t("v", { n: x.n })))));
    const parts = [
      h("a", { class: "back", onclick: () => list() }, t("back")),
      title,
      h("div", { class: "meta" },
        h("span", { class: "badge" }, a.kind),
        h("span", {}, a.scope === "room" ? t("room") : t("personal")),
        h("span", {}, t("updated", { when: when(a.updated) })),
        h("span", {}, size(a.bytes))),
      a.summary ? h("p", { class: "hint" }, a.summary) : null,
      actions,
      a.versions.length > 1 ? versions : null,
      r.v !== a.version ? h("p", { class: "hint" }, t("newer", { n: r.v, last: a.version }), " ",
        h("a", { class: "back", onclick: () => show(a.id) }, t("newest"))) : null,
    ];
    try { parts.push(a.viewer ? await viewer(a.id, r.v) : content(a.kind, r.content ?? "")); }
    catch (e) { say(t("failed", { why: e.message })); }
    $("app").replaceChildren(...parts.filter(Boolean));
  }

  function rename(a, title, v) {
    const input = h("input", { class: "grow", maxlength: "80", value: a.title, required: true });
    const form = h("form", { onsubmit: async (e) => {
      e.preventDefault();
      try {
        await okbot.call("artifacts.rename", { id: a.id, title: input.value.trim() });
        await show(a.id, v);
        say(t("renamed"));
      } catch (err) { say(t("failed", { why: err.message })); }
    } }, input, h("button", { type: "submit" }, t("save")),
      h("button", { type: "button", onclick: () => form.replaceWith(title) }, t("cancel")));
    title.replaceWith(form);
    input.focus();
  }

  (async () => {
    $("title").textContent = t("title");
    me = await okbot.me();
    tz = me.timezone;
    const { path, params } = okbot.view;
    if (path === "/a" && params.id) await show(params.id, params.v ? Number(params.v) : undefined);
    else await list();
  })().catch((e) => say(t("failed", { why: e.message })));
})();
