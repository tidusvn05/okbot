// Draws a Mermaid artifact in okbot's artifact viewer (ADR 0021). The page has no network and
// an opaque origin; Mermaid runs in strict mode, so labels carry no HTML and no click handlers.
"use strict";
(async () => {
  const source = document.getElementById("src").textContent;
  const out = document.getElementById("out");
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  try {
    mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: dark ? "dark" : "default" });
    const { svg } = await mermaid.render("diagram", source);
    out.innerHTML = svg; // sanitised by Mermaid (strict)
  } catch (e) {
    const p = document.createElement("p");
    p.textContent = "This diagram could not be drawn: " + (e && e.message ? e.message : String(e));
    const pre = document.createElement("pre");
    pre.textContent = source;
    out.replaceChildren(p, pre);
  }
  document.body.dataset.ready = "1";
})();
