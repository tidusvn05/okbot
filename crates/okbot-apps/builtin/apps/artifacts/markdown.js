// A small Markdown renderer for the artifacts app. It builds DOM nodes only: text is never
// parsed as HTML, raw HTML in the source shows as text, links open through okbot's shell and
// pictures show their description (the app has no network).
"use strict";
window.markdown = (() => {
  const el = (tag, ...children) => {
    const e = document.createElement(tag);
    for (const c of children) e.append(c instanceof Node ? c : document.createTextNode(String(c)));
    return e;
  };

  // Inline: `code`, **strong**, *em* / _em_, ~~del~~, [text](https://…), ![alt](…), <https://…>.
  const INLINE = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([\s\S]+?)\*\*|__([\s\S]+?)__|~~([\s\S]+?)~~|\*([^*\s][^*]*?)\*|(?<![\w])_([^_\s][^_]*?)_(?![\w])|!\[([^\]]*)\]\(([^)\s]*)[^)]*\)|\[([^\]]+)\]\(([^)\s]+)[^)]*\)|<(https:\/\/[^>\s]+)>/g;

  function inline(text, openLink) {
    const out = [];
    let last = 0;
    for (const m of text.matchAll(INLINE)) {
      if (m.index > last) out.push(document.createTextNode(text.slice(last, m.index)));
      last = m.index + m[0].length;
      if (m[2] !== undefined) out.push(el("code", m[2].trim() || m[2]));
      else if (m[3] !== undefined || m[4] !== undefined) out.push(el("strong", ...inline(m[3] ?? m[4], openLink)));
      else if (m[5] !== undefined) out.push(el("del", ...inline(m[5], openLink)));
      else if (m[6] !== undefined || m[7] !== undefined) out.push(el("em", ...inline(m[6] ?? m[7], openLink)));
      else if (m[8] !== undefined || m[9] !== undefined) out.push(el("em", "[" + (m[8] || "image") + "]"));
      else out.push(link(m[10] ?? m[12], m[11] ?? m[12], openLink));
    }
    if (last < text.length) out.push(document.createTextNode(text.slice(last)));
    return out;
  }

  function link(label, href, openLink) {
    if (!/^https:\/\//i.test(href)) return el("span", ...inline(label, openLink));
    const a = el("a", ...inline(label, openLink));
    a.setAttribute("role", "link");
    a.tabIndex = 0;
    a.title = href;
    const go = (e) => { e.preventDefault(); openLink(href); };
    a.addEventListener("click", go);
    a.addEventListener("keydown", (e) => { if (e.key === "Enter") go(e); });
    return a;
  }

  const cells = (line) => line.trim().replace(/^\|/, "").replace(/\|$/, "").split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));

  function render(source, { openLink = () => {} } = {}) {
    const root = document.createDocumentFragment();
    const lines = source.replace(/\r\n?/g, "\n").split("\n");
    let i = 0;
    let para = [];
    const flush = () => {
      if (para.length) root.append(el("p", ...inline(para.join("\n"), openLink)));
      para = [];
    };
    while (i < lines.length) {
      const line = lines[i];
      let m;
      if (/^\s*$/.test(line)) { flush(); i++; continue; }
      if ((m = line.match(/^ {0,3}(```+|~~~+)\s*([\w+-]*)/))) {
        flush();
        const fence = m[1];
        const body = [];
        i++;
        while (i < lines.length && !lines[i].trimStart().startsWith(fence)) body.push(lines[i++]);
        i++;
        const code = el("code", body.join("\n"));
        if (m[2]) code.dataset.lang = m[2];
        root.append(el("pre", code));
        continue;
      }
      if ((m = line.match(/^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/))) {
        flush();
        root.append(el("h" + m[1].length, ...inline(m[2], openLink)));
        i++;
        continue;
      }
      if (/^ {0,3}([-*_])(\s*\1){2,}\s*$/.test(line)) { flush(); root.append(el("hr")); i++; continue; }
      if (/^ {0,3}>/.test(line)) {
        flush();
        const body = [];
        while (i < lines.length && /^ {0,3}>/.test(lines[i])) body.push(lines[i++].replace(/^ {0,3}> ?/, ""));
        root.append(el("blockquote", render(body.join("\n"), { openLink })));
        continue;
      }
      if (line.includes("|") && i + 1 < lines.length && /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(lines[i + 1])) {
        flush();
        const head = cells(line);
        const align = cells(lines[i + 1]).map((c) => (c.endsWith(":") ? (c.startsWith(":") ? "center" : "right") : ""));
        i += 2;
        const table = el("table");
        const tr = el("tr", ...head.map((c) => el("th", ...inline(c, openLink))));
        table.append(el("thead", tr));
        const tbody = el("tbody");
        while (i < lines.length && lines[i].includes("|") && lines[i].trim()) {
          const row = el("tr");
          cells(lines[i++]).forEach((c, k) => {
            const td = el("td", ...inline(c, openLink));
            if (align[k]) td.style.textAlign = align[k];
            row.append(td);
          });
          tbody.append(row);
        }
        table.append(tbody);
        const wrap = el("div", table);
        wrap.className = "table-wrap";
        root.append(wrap);
        continue;
      }
      if ((m = line.match(/^(\s*)([-*+]|\d{1,9}[.)])\s+/))) {
        flush();
        root.append(list(lines, i, openLink, (next) => { i = next; }));
        continue;
      }
      para.push(line);
      i++;
    }
    flush();
    return root;
  }

  // A list from line `start`, deeper indentation making nested lists; calls `done` with the
  // first line after it.
  function list(lines, start, openLink, done) {
    const first = lines[start].match(/^(\s*)([-*+]|\d{1,9}[.)])\s+/);
    const indent = first[1].length;
    const ordered = /\d/.test(first[2]);
    const root = el(ordered ? "ol" : "ul");
    if (ordered && parseInt(first[2], 10) !== 1) root.start = parseInt(first[2], 10);
    let i = start;
    let item = null;
    while (i < lines.length) {
      const line = lines[i];
      const m = line.match(/^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/);
      if (m && m[1].length === indent) {
        let text = m[3];
        let box = null;
        const task = text.match(/^\[([ xX])\]\s+(.*)$/);
        if (task) {
          box = el("input");
          box.type = "checkbox";
          box.disabled = true;
          box.checked = task[1] !== " ";
          text = task[2];
        }
        item = el("li");
        if (box) item.append(box, " ");
        item.append(...inline(text, openLink));
        root.append(item);
        i++;
      } else if (m && m[1].length > indent && item) {
        item.append(list(lines, i, openLink, (next) => { i = next; }));
      } else if (!m && item && /^\s{2,}\S/.test(line)) {
        item.append(document.createTextNode(" "), ...inline(line.trim(), openLink));
        i++;
      } else break;
    }
    done(i);
    return root;
  }

  return { render };
})();
