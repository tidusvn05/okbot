// The built-in Schedule app (ADR 0019): what fires in the coming days, as an agenda or a calendar.
"use strict";
(() => {
  const t = okbot.words({
    en: { title: "Schedule", week: "7 days", month: "30 days", agenda: "Agenda", calendar: "Calendar", allDays: "All days",
          empty: "Nothing is scheduled.", emptyDay: "Nothing on this day.", failed: "Could not do that: {why}",
          reminder: "reminder", routine: "routine", todo: "to-do", automation: "automation",
          snooze: "Snooze 1h", move: "Change time", pause: "Pause", save: "Save", close: "Close", saved: "Saved.",
          newTitle: "Remind me to…", add: "Add", once: "Once", daily: "Every day", weekdays: "Weekdays", weekly: "Every week",
          monthly: "Every month", needTime: "Pick a time.", next: "next of a repeating one", more: "+{n} more",
          surePause: "Pause “{title}”? It stays in Tasks, where you can resume it.",
          where: "Times in {tz}.", mon: "Mon", tue: "Tue", wed: "Wed", thu: "Thu", fri: "Fri", sat: "Sat", sun: "Sun" },
    vi: { title: "Lịch", week: "7 ngày", month: "30 ngày", agenda: "Danh sách", calendar: "Lịch", allDays: "Mọi ngày",
          empty: "Không có lịch nào.", emptyDay: "Ngày này không có gì.", failed: "Không làm được: {why}",
          reminder: "nhắc nhở", routine: "routine", todo: "việc", automation: "automation",
          snooze: "Hoãn 1 giờ", move: "Đổi giờ", pause: "Tạm dừng", save: "Lưu", close: "Đóng", saved: "Đã lưu.",
          newTitle: "Nhắc tôi…", add: "Thêm", once: "Một lần", daily: "Hằng ngày", weekdays: "Ngày làm việc", weekly: "Hằng tuần",
          monthly: "Hằng tháng", needTime: "Hãy chọn giờ.", next: "lần tới của lịch lặp", more: "+{n} nữa",
          surePause: "Tạm dừng “{title}”? Việc vẫn nằm trong Công việc, bạn có thể chạy lại ở đó.",
          where: "Giờ theo {tz}.", mon: "T2", tue: "T3", wed: "T4", thu: "T5", fri: "T6", sat: "T7", sun: "CN" },
  });
  const { h } = okbot;
  const $ = (id) => document.getElementById(id);
  const DOW = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
  let days = okbot.view.path === "/month" ? 30 : 7;
  let mode = "agenda";
  let picked = null;
  let me;
  let tz = "UTC";

  const say = (text) => { $("message").textContent = text ?? ""; };
  // Times come in the bot's zone (`2026-11-27T09:00:00+07:00`): the date is the first 10
  // characters, and the first 16 are what a datetime-local field shows and the API reads back.
  const dayOf = (iso) => iso.slice(0, 10);
  const today = () => new Intl.DateTimeFormat("en-CA", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit" }).format(new Date());
  const addDays = (date, n) => {
    const d = new Date(date + "T00:00:00Z");
    d.setUTCDate(d.getUTCDate() + n);
    return d.toISOString().slice(0, 10);
  };
  const dow = (date) => new Date(date + "T00:00:00Z").getUTCDay();
  const clock = (iso) => iso.slice(11, 16);

  async function act(fn) {
    try {
      await fn();
      say(t("saved"));
      await load();
    } catch (e) {
      say(t("failed", { why: e.message }));
    }
  }

  function actions(item, first, li) {
    const box = h("div", { class: "actions" });
    if (!item.task || !me.permissions.includes("tasks:write") || !first) return box;
    if (item.kind === "reminder") {
      box.append(h("button", { type: "button", onclick: () => act(() => okbot.call("tasks.snooze", { id: item.task, until: "1h" })) }, t("snooze")));
    }
    box.append(h("button", { type: "button", onclick: () => {
      const input = h("input", { type: "datetime-local", value: item.at.slice(0, 16), required: true });
      li.replaceChildren(h("form", { class: "row", onsubmit: (e) => {
        e.preventDefault();
        act(() => okbot.call("tasks.update", { id: item.task, remind_at: input.value }));
      } }, h("span", { class: "t" }, item.title), input,
        h("button", { type: "submit", class: "primary" }, t("save")),
        h("button", { type: "button", onclick: () => load() }, t("close"))));
      input.focus();
    } }, t("move")));
    box.append(h("button", { type: "button", onclick: async () => {
      if (await okbot.confirm(t("surePause", { title: item.title }))) act(() => okbot.call("tasks.pause", { id: item.task }));
    } }, t("pause")));
    return box;
  }

  function agenda(items, root) {
    if (!items.length) { root.append(h("p", { class: "empty" }, picked ? t("emptyDay") : t("empty"))); return; }
    const seen = new Set();
    let day = "";
    let list = null;
    for (const item of items) {
      const d = dayOf(item.at);
      if (d !== day) {
        day = d;
        list = h("ul", { class: "list" });
        root.append(h("div", { class: "day" }, okbot.when(item.at, tz, { weekday: "long", day: "2-digit", month: "2-digit" })), list);
      }
      const first = !item.task || !seen.has(item.task);
      if (item.task) seen.add(item.task);
      const li = h("li", {});
      li.append(
        h("div", { class: "t" }, clock(item.at) + " · " + item.title),
        h("div", { class: "meta" }, h("span", { class: "badge" }, t(item.kind)),
          item.repeat ? h("span", {}, item.repeat) : null,
          item.task && !first ? h("span", {}, t("next")) : null),
        actions(item, first, li));
      list.append(li);
    }
  }

  function calendar(items, root) {
    const byDay = new Map();
    for (const item of items) {
      const d = dayOf(item.at);
      if (!byDay.has(d)) byDay.set(d, []);
      byDay.get(d).push(item);
    }
    const first = today();
    const last = addDays(first, days - 1);
    // Weeks run Monday to Sunday.
    const end = addDays(last, (7 - dow(last)) % 7);
    const grid = h("div", { class: "cal" });
    for (const k of ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]) grid.append(h("div", { class: "cal-head" }, t(k)));
    for (let d = addDays(first, -((dow(first) + 6) % 7)); d <= end; d = addDays(d, 1)) {
      const inRange = d >= first && d <= last;
      const list = byDay.get(d) ?? [];
      const cell = h("button", {
        type: "button",
        class: "cal-day" + (inRange ? "" : " out") + (d === first ? " today" : "") + (d === picked ? " picked" : "") + (list.length ? " has" : ""),
        disabled: !inRange,
        "aria-label": d,
        onclick: () => { picked = picked === d ? null : d; render(); },
      }, h("span", { class: "n" }, String(Number(d.slice(8, 10)))));
      for (const item of list.slice(0, 3)) cell.append(h("span", { class: "ev " + item.kind }, clock(item.at) + " " + item.title));
      if (list.length > 3) cell.append(h("span", { class: "ev more" }, t("more", { n: list.length - 3 })));
      grid.append(cell);
    }
    root.append(grid);
    if (picked) {
      root.append(h("div", { class: "row" }, h("button", { type: "button", onclick: () => { picked = null; render(); } }, t("allDays"))));
      agenda(byDay.get(picked) ?? [], root);
    }
  }

  function addForm() {
    const title = h("input", { class: "grow", maxlength: 200, placeholder: t("newTitle"), required: true });
    const time = h("input", { type: "datetime-local", required: true });
    const repeat = h("select", {});
    for (const k of ["", "daily", "weekdays", "weekly", "monthly"]) repeat.append(h("option", { value: k }, t(k || "once")));
    return h("form", { onsubmit: (e) => {
      e.preventDefault();
      if (!time.value) { say(t("needTime")); return; }
      const [date, hm] = time.value.split("T");
      const rule = {
        "": undefined,
        daily: `daily ${hm}`,
        weekdays: `weekdays ${hm}`,
        weekly: `weekly ${DOW[dow(date)]} ${hm}`,
        monthly: `monthly ${Number(date.slice(8, 10))} ${hm}`,
      }[repeat.value];
      act(() => okbot.call("tasks.add", { kind: "reminder", title: title.value.trim(), remind_at: time.value, repeat: rule }));
    } }, title, time, repeat, h("button", { type: "submit" }, t("add")));
  }

  let items = [];

  function render() {
    const app = $("app");
    const toggle = (label, on, f) => h("button", { type: "button", class: on ? "primary" : "", onclick: f }, label);
    app.replaceChildren(h("form", { onsubmit: (e) => e.preventDefault() },
      toggle(t("week"), days === 7, () => { days = 7; picked = null; load(); }),
      toggle(t("month"), days === 30, () => { days = 30; picked = null; load(); }),
      toggle(t("agenda"), mode === "agenda", () => { mode = "agenda"; picked = null; render(); }),
      toggle(t("calendar"), mode === "calendar", () => { mode = "calendar"; render(); })));
    if (me.permissions.includes("tasks:write")) app.append(addForm());
    if (mode === "calendar") calendar(items, app);
    else agenda(items, app);
  }

  async function load() {
    try {
      const r = await okbot.call("schedule.list", { days });
      tz = r.timezone;
      items = r.items;
      $("where").textContent = t("where", { tz });
      render();
    } catch (e) {
      say(t("failed", { why: e.message }));
    }
  }

  (async () => {
    $("title").textContent = t("title");
    okbot.setTitle(t("title"));
    me = await okbot.me();
    tz = me.timezone;
    await load();
  })().catch((e) => say(t("failed", { why: e.message })));
})();
