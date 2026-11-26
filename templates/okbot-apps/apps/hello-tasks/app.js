// A sample okbot app: no network, no cookies; everything goes through `okbot.call`.
"use strict";
(async () => {
  const t = okbot.words({
    en: { hello: "Hello, {name}!", count: "You have {n} open task(s) here." },
    vi: { hello: "Chào {name}!", count: "Bạn có {n} việc chưa xong ở đây." },
  });
  const me = await okbot.me();
  document.getElementById("hello").textContent = t("hello", { name: me.name });
  const page = await okbot.call("tasks.list", { status: "active", limit: 1 });
  document.getElementById("count").textContent = t("count", { n: page.total });
})().catch((e) => {
  document.getElementById("count").textContent = e.message;
});
