// The built-in Tasks app (ADR 0019): every task the bot keeps for you and this room.
"use strict";
(() => {
  const t = okbot.words({
    en: {
      title: "Tasks", search: "Search", open: "Open", all: "All", done: "Done",
      any: "Every kind", todo: "To-dos", reminder: "Reminders", routine: "Routines",
      anywhere: "Everyone's", mine: "Made by me", personal: "Only mine", room: "This room",
      next: "Coming first", created: "Newest", sorted: "As stored",
      add: "Add", newTitle: "New to-do", newReminder: "Remind me to…", personalBadge: "personal", roomBadge: "this room",
      directNote: "Your own tasks and this chat's. Times in {tz}.",
      roomNote: "Tasks of {room}. Your own tasks are shown when you open this from a direct chat. Times in {tz}.",
      markDone: "Done", snooze: "Snooze", in1h: "1 hour", in3h: "3 hours", tomorrow: "Tomorrow 9:00",
      edit: "Edit", save: "Save", close: "Close", pause: "Pause", resume: "Resume", cancel: "Cancel",
      fTitle: "Title", fNotes: "Notes", fDue: "Due", fRemind: "Remind at", fRepeat: "Repeat",
      repeatHint: "daily 08:00, weekdays 08:00, weekly mon,fri 09:00, monthly 1 09:00, every 3h",
      routineNote: "A routine's prompt and schedule are changed in chat.",
      due: "due {at}", at: "⏰ {at}", late: "overdue", more: "Show more", empty: "No tasks here.",
      saved: "Saved.", sureCancel: "Cancel “{title}”?", loading: "Loading…", count: "{n} tasks", one: "1 task",
      paused: "paused", cancelled: "cancelled", byOther: "by someone else", needTime: "A reminder needs a time.",
      failed: "Could not do that: {why}",
    },
    vi: {
      title: "Công việc", search: "Tìm", open: "Chưa xong", all: "Tất cả", done: "Đã xong",
      any: "Mọi loại", todo: "Việc cần làm", reminder: "Nhắc nhở", routine: "Routine",
      anywhere: "Của mọi người", mine: "Tôi tạo", personal: "Chỉ của tôi", room: "Phòng này",
      next: "Sắp đến trước", created: "Mới tạo", sorted: "Theo thứ tự lưu",
      add: "Thêm", newTitle: "Việc mới", newReminder: "Nhắc tôi…", personalBadge: "cá nhân", roomBadge: "phòng này",
      directNote: "Việc của bạn và của chat này. Giờ theo {tz}.",
      roomNote: "Việc của {room}. Việc cá nhân chỉ hiện khi bạn mở từ chat riêng. Giờ theo {tz}.",
      markDone: "Xong", snooze: "Hoãn", in1h: "1 giờ", in3h: "3 giờ", tomorrow: "9:00 ngày mai",
      edit: "Sửa", save: "Lưu", close: "Đóng", pause: "Tạm dừng", resume: "Chạy lại", cancel: "Huỷ",
      fTitle: "Tiêu đề", fNotes: "Ghi chú", fDue: "Hạn", fRemind: "Nhắc lúc", fRepeat: "Lặp lại",
      repeatHint: "daily 08:00, weekdays 08:00, weekly mon,fri 09:00, monthly 1 09:00, every 3h",
      routineNote: "Prompt và lịch của routine được đổi trong chat.",
      due: "hạn {at}", at: "⏰ {at}", late: "quá hạn", more: "Xem thêm", empty: "Không có việc nào.",
      saved: "Đã lưu.", sureCancel: "Huỷ “{title}”?", loading: "Đang tải…", count: "{n} việc", one: "1 việc",
      paused: "tạm dừng", cancelled: "đã huỷ", byOther: "người khác tạo", needTime: "Nhắc nhở cần có giờ.",
      failed: "Không làm được: {why}",
    },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);

  let me = null;
  let cursor = null;
  let can = { write: false };
  const state = {
    status: okbot.view.path === "/all" ? "all" : "active",
    kind: okbot.view.params.kind ?? "",
    place: "",
    sort: "next",
    q: "",
  };

  const option = (select, value, label) => select.append(h("option", { value }, label));
  const say = (text) => { $("message").textContent = text ?? ""; };
  const when = (iso) => okbot.when(iso, me?.timezone);
  // Times come in the bot's zone (`2026-11-27T09:00:00+07:00`): their first 16 characters are what
  // a datetime-local field shows, and the App API reads that back in the same zone.
  const local = (iso) => (iso ? iso.slice(0, 16) : "");

  function tomorrowAt9() {
    const d = new Intl.DateTimeFormat("en-CA", { timeZone: me.timezone, year: "numeric", month: "2-digit", day: "2-digit" })
      .format(new Date(Date.now() + 86400000));
    return `${d} 09:00`;
  }

  async function act(fn) {
    try {
      await fn();
      say(t("saved"));
      await load(true);
    } catch (e) {
      say(t("failed", { why: e.message }));
    }
  }

  function editor(task, li) {
    const routine = task.kind === "routine";
    const field = (label, input) => h("label", {}, h("span", {}, label), input);
    const title = h("input", { value: task.title, maxlength: 200, required: true });
    const notes = h("textarea", { rows: 3, maxlength: 2000, disabled: routine });
    notes.value = task.notes ?? "";
    const due = h("input", { type: "datetime-local", value: local(task.due) });
    const remind = h("input", { type: "datetime-local", value: local(task.remind_at) });
    const repeat = h("input", { value: task.repeat ?? "", placeholder: t("repeatHint"), disabled: routine });
    const form = h("form", { class: "edit", onsubmit: (e) => {
      e.preventDefault();
      const change = { id: task.id };
      if (title.value.trim() !== task.title) change.title = title.value.trim();
      if (!routine && notes.value !== (task.notes ?? "")) change.notes = notes.value;
      if (due.value !== local(task.due)) change.due = due.value;
      if (remind.value !== local(task.remind_at)) change.remind_at = remind.value;
      if (!routine && repeat.value.trim() !== (task.repeat ?? "")) change.repeat = repeat.value.trim();
      act(() => okbot.call("tasks.update", change));
    } },
      field(t("fTitle"), title),
      field(t("fNotes"), notes),
      field(t("fDue"), due),
      field(t("fRemind"), remind),
      field(t("fRepeat"), repeat),
      routine ? h("p", { class: "hint" }, t("routineNote")) : null,
      h("div", { class: "row" },
        h("button", { type: "submit", class: "primary" }, t("save")),
        h("button", { type: "button", onclick: () => li.replaceWith(item(task)) }, t("close"))));
    li.replaceChildren(form);
    li.classList.add("single");
    title.focus();
  }

  function item(task) {
    const active = ["open", "paused"].includes(task.status);
    const li = h("li", { class: active ? "" : "done" });
    const meta = h("div", { class: "meta" },
      h("span", { class: "badge" }, t(task.kind)),
      h("span", { class: "badge" }, task.scope === "room" ? t("roomBadge") : t("personalBadge")),
      task.remind_at ? h("span", {}, t("at", { at: when(task.remind_at) }) + (task.repeat ? " · " + task.repeat : "")) : null,
      task.due ? h("span", { class: task.overdue ? "late" : "" }, t("due", { at: when(task.due) })) : null,
      task.overdue ? h("span", { class: "late" }, t("late")) : null,
      task.status === "paused" ? h("span", {}, t("paused")) : null,
      task.status === "cancelled" ? h("span", {}, t("cancelled")) : null,
      task.scope === "room" && !task.mine ? h("span", {}, t("byOther")) : null);
    li.append(h("div", { class: "t" }, task.title), meta);
    if (task.notes) li.append(h("div", { class: "notes" }, task.notes));
    if (can.write && active) {
      const button = (label, fn, primary) =>
        h("button", { type: "button", class: primary ? "primary" : "", onclick: () => act(fn) }, label);
      const actions = h("div", { class: "actions" });
      if (task.kind !== "routine") actions.append(button(t("markDone"), () => okbot.call("tasks.done", { id: task.id }), true));
      if (task.kind !== "routine" && task.remind_at && task.status === "open") {
        const snooze = h("select", { "aria-label": t("snooze"), onchange: () => {
          const until = snooze.value === "tomorrow" ? tomorrowAt9() : snooze.value;
          snooze.value = "";
          act(() => okbot.call("tasks.snooze", { id: task.id, until }));
        } });
        option(snooze, "", t("snooze"));
        option(snooze, "1h", t("in1h"));
        option(snooze, "3h", t("in3h"));
        option(snooze, "tomorrow", t("tomorrow"));
        actions.append(snooze);
      }
      actions.append(h("button", { type: "button", onclick: () => editor(task, li) }, t("edit")));
      if (task.status === "open") actions.append(button(t("pause"), () => okbot.call("tasks.pause", { id: task.id })));
      else actions.append(button(t("resume"), () => okbot.call("tasks.resume", { id: task.id })));
      actions.append(h("button", { type: "button", onclick: async () => {
        if (await okbot.confirm(t("sureCancel", { title: task.title }))) act(() => okbot.call("tasks.cancel", { id: task.id }));
      } }, t("cancel")));
      li.append(actions);
    }
    return li;
  }

  async function load(fresh) {
    if (fresh) { cursor = null; $("list").replaceChildren(); }
    say(t("loading"));
    try {
      const place = state.place;
      const page = await okbot.call("tasks.list", {
        status: state.status, kind: state.kind || undefined, query: state.q || undefined,
        scope: place === "personal" || place === "room" ? place : undefined,
        mine: place === "mine" ? true : undefined,
        sort: state.sort || undefined, cursor, limit: 50,
      });
      for (const task of page.items) $("list").append(item(task));
      cursor = page.next ?? null;
      $("more").hidden = !cursor;
      say(page.total === 1 ? t("one") : page.total ? t("count", { n: page.total }) : t("empty"));
    } catch (e) {
      say(t("failed", { why: e.message }));
    }
  }

  function addForm() {
    const kind = $("new-kind");
    option(kind, "todo", t("todo"));
    option(kind, "reminder", t("reminder"));
    const placeholder = () => { $("new-title").placeholder = kind.value === "reminder" ? t("newReminder") : t("newTitle"); };
    kind.onchange = placeholder;
    placeholder();
    $("add-button").textContent = t("add");
    $("add").onsubmit = (e) => {
      e.preventDefault();
      const title = $("new-title").value.trim();
      if (!title) return;
      const time = $("new-time").value;
      if (kind.value === "reminder" && !time) { say(t("needTime")); return; }
      const args = kind.value === "reminder"
        ? { kind: "reminder", title, remind_at: time }
        : { kind: "todo", title, due: time || undefined };
      act(async () => {
        await okbot.call("tasks.add", args);
        $("new-title").value = "";
        $("new-time").value = "";
      });
    };
  }

  async function start() {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    me = await okbot.me();
    can.write = me.permissions.includes("tasks:write");
    $("q").placeholder = t("search");
    option($("status"), "active", t("open"));
    option($("status"), "all", t("all"));
    option($("status"), "done", t("done"));
    $("status").value = state.status;
    for (const k of ["", "todo", "reminder", "routine"]) option($("kind"), k, k ? t(k) : t("any"));
    $("kind").value = state.kind;
    option($("place"), "", t("anywhere"));
    option($("place"), "mine", t("mine"));
    if (me.context.direct) option($("place"), "personal", t("personal"));
    option($("place"), "room", t("room"));
    option($("sort"), "next", t("next"));
    option($("sort"), "created", t("created"));
    option($("sort"), "", t("sorted"));
    $("sort").value = state.sort;
    $("more").textContent = t("more");
    $("add").hidden = !can.write;
    if (can.write) addForm();
    $("where").textContent = me.context.direct
      ? t("directNote", { tz: me.timezone })
      : t("roomNote", { room: me.context.label, tz: me.timezone });
    let timer;
    $("q").oninput = () => { clearTimeout(timer); timer = setTimeout(() => { state.q = $("q").value.trim(); load(true); }, 250); };
    for (const k of ["status", "kind", "place", "sort"]) $(k).onchange = () => { state[k] = $(k).value; load(true); };
    $("filters").onsubmit = (e) => e.preventDefault();
    $("more").onclick = () => load(false);
    await load(true);
  }
  start().catch((e) => say(t("failed", { why: e.message })));
})();
