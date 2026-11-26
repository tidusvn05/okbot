// The built-in Tasks app (ADR 0019): every task the bot keeps for you and this room.
"use strict";
(() => {
  const W = {
    en: {
      title: "Tasks", search: "Search", open: "Open", all: "All", done: "Done",
      any: "Every kind", todo: "To-dos", reminder: "Reminders", routine: "Routines",
      add: "Add", newTitle: "New to-do", personal: "personal", room: "this room",
      directNote: "Your own tasks and this chat's.", roomNote: "Tasks of {room}. Your own tasks are shown when you open this from a direct chat.",
      markDone: "Done", snooze: "Snooze 1h", pause: "Pause", resume: "Resume", cancel: "Cancel",
      due: "due {at}", at: "⏰ {at}", late: "overdue", more: "Show more", empty: "No tasks here.",
      saved: "Saved.", sureCancel: "Cancel “{title}”?", loading: "Loading…", count: "{n} tasks", one: "1 task",
      paused: "paused", cancelled: "cancelled", failed: "Could not do that: {why}",
    },
    vi: {
      title: "Công việc", search: "Tìm", open: "Chưa xong", all: "Tất cả", done: "Đã xong",
      any: "Mọi loại", todo: "Việc cần làm", reminder: "Nhắc nhở", routine: "Routine",
      add: "Thêm", newTitle: "Việc mới", personal: "cá nhân", room: "phòng này",
      directNote: "Việc của bạn và của chat này.", roomNote: "Việc của {room}. Việc cá nhân chỉ hiện khi bạn mở từ chat riêng.",
      markDone: "Xong", snooze: "Hoãn 1 giờ", pause: "Tạm dừng", resume: "Chạy lại", cancel: "Huỷ",
      due: "hạn {at}", at: "⏰ {at}", late: "quá hạn", more: "Xem thêm", empty: "Không có việc nào.",
      saved: "Đã lưu.", sureCancel: "Huỷ “{title}”?", loading: "Đang tải…", count: "{n} việc", one: "1 việc",
      paused: "tạm dừng", cancelled: "đã huỷ", failed: "Không làm được: {why}",
    },
  };
  const words = W[okbot.lang] ?? W.en;
  const t = (k, v = {}) => words[k].replace(/\{(\w+)\}/g, (_, x) => v[x] ?? "");
  const $ = (id) => document.getElementById(id);

  let me = null;
  let cursor = null;
  let can = { write: false };
  const state = {
    status: okbot.view.path === "/all" ? "all" : "active",
    kind: okbot.view.params.kind ?? "",
    q: "",
  };

  const option = (select, value, label) => {
    const o = document.createElement("option");
    o.value = value;
    o.textContent = label;
    select.append(o);
  };

  function when(iso) {
    const d = new Date(iso);
    return d.toLocaleString(okbot.lang === "vi" ? "vi-VN" : undefined, {
      timeZone: me?.timezone, weekday: "short", day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit",
    });
  }

  function say(text) { $("message").textContent = text ?? ""; }

  async function act(fn) {
    try {
      await fn();
      say(t("saved"));
      await load(true);
    } catch (e) {
      say(t("failed", { why: e.message }));
    }
  }

  function item(task) {
    const li = document.createElement("li");
    if (!["open", "paused"].includes(task.status)) li.className = "done";
    const title = document.createElement("div");
    title.className = "t";
    title.textContent = task.title;
    const meta = document.createElement("div");
    meta.className = "meta";
    const add = (text, cls) => {
      const s = document.createElement("span");
      s.textContent = text;
      if (cls) s.className = cls;
      meta.append(s);
    };
    add(words[task.kind] ?? task.kind, "badge");
    add(task.scope === "room" ? t("room") : t("personal"), "badge");
    if (task.remind_at) add(t("at", { at: when(task.remind_at) }) + (task.repeat ? " · " + task.repeat : ""));
    if (task.due) add(t("due", { at: when(task.due) }), task.overdue ? "late" : "");
    if (task.overdue) add(t("late"), "late");
    if (task.status === "paused") add(t("paused"));
    if (task.status === "cancelled") add(t("cancelled"));
    li.append(title, meta);
    if (task.notes) {
      const n = document.createElement("div");
      n.className = "notes";
      n.textContent = task.notes;
      li.append(n);
    }
    if (can.write && ["open", "paused"].includes(task.status)) {
      const actions = document.createElement("div");
      actions.className = "actions";
      const button = (label, fn, primary) => {
        const b = document.createElement("button");
        b.type = "button";
        b.textContent = label;
        if (primary) b.className = "primary";
        b.onclick = () => act(fn);
        actions.append(b);
      };
      if (task.kind !== "routine") button(t("markDone"), () => okbot.call("tasks.done", { id: task.id }), true);
      if (task.kind === "reminder" && task.status === "open") button(t("snooze"), () => okbot.call("tasks.snooze", { id: task.id, until: "1h" }));
      if (task.status === "open") button(t("pause"), () => okbot.call("tasks.pause", { id: task.id }));
      else button(t("resume"), () => okbot.call("tasks.resume", { id: task.id }));
      button(t("cancel"), async () => {
        if (await okbot.confirm(t("sureCancel", { title: task.title }))) await okbot.call("tasks.cancel", { id: task.id });
      });
      li.append(actions);
    }
    return li;
  }

  async function load(fresh) {
    if (fresh) { cursor = null; $("list").replaceChildren(); }
    say(t("loading"));
    try {
      const page = await okbot.call("tasks.list", {
        status: state.status, kind: state.kind || undefined, query: state.q || undefined, cursor, limit: 50,
      });
      for (const task of page.items) $("list").append(item(task));
      cursor = page.next ?? null;
      $("more").hidden = !cursor;
      say(page.total === 1 ? t("one") : page.total ? t("count", { n: page.total }) : t("empty"));
    } catch (e) {
      say(t("failed", { why: e.message }));
    }
  }

  async function start() {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    $("q").placeholder = t("search");
    option($("status"), "active", t("open"));
    option($("status"), "all", t("all"));
    option($("status"), "done", t("done"));
    $("status").value = state.status;
    for (const k of ["", "todo", "reminder", "routine"]) option($("kind"), k, k ? t(k) : t("any"));
    $("kind").value = state.kind;
    $("new-title").placeholder = t("newTitle");
    $("add-button").textContent = t("add");
    $("more").textContent = t("more");
    me = await okbot.me();
    can.write = me.permissions.includes("tasks:write");
    $("add").hidden = !can.write;
    $("where").textContent = me.context.direct ? t("directNote") : t("roomNote", { room: me.context.label });
    let timer;
    $("q").oninput = () => { clearTimeout(timer); timer = setTimeout(() => { state.q = $("q").value.trim(); load(true); }, 250); };
    $("status").onchange = () => { state.status = $("status").value; load(true); };
    $("kind").onchange = () => { state.kind = $("kind").value; load(true); };
    $("filters").onsubmit = (e) => e.preventDefault();
    $("more").onclick = () => load(false);
    $("add").onsubmit = (e) => {
      e.preventDefault();
      const title = $("new-title").value.trim();
      if (!title) return;
      const due = $("new-due").value;
      act(async () => {
        await okbot.call("tasks.add", { title, due: due || undefined });
        $("new-title").value = "";
        $("new-due").value = "";
      });
    };
    await load(true);
  }
  start().catch((e) => say(t("failed", { why: e.message })));
})();
