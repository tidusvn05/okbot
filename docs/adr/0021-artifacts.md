# 0021 — Artifacts: files the agent makes, kept in the bundle, shown in a sandbox

Status: Accepted (2026-11-27)

## Context
Answers that are pages, tables, documents or diagrams do not fit a chat message, and pasted text
scrolls away. Apps (ADR 0019) give us signed-in pages behind chat summaries. The agent is
untrusted, though (hard rule 2): it has no file access, and what it writes can carry a prompt
injection. Full design: [`docs/design-artifacts.md`](../design-artifacts.md).

## Decision
- **Artifacts are files in the bundle**, at `kb/artifacts/<owner folder>/<id>/`: an `index.md` with
  OKF frontmatter and a summary, and one file per version. They use the owner folders and `Scope`
  of tasks and memory. Only `index.md` is indexed (`.okbaseignore`).
- **The agent writes through tools** (`artifact_save`, `artifact_list`, `artifact_get` on the `bot`
  server), never through files. `okbot-core` gains an `ArtifactStore` port, implemented by
  `okbot-knowledge`. Read-only runs only read.
- **okbot adds the Open button**, not the agent, for each artifact saved in the turn. This works
  with interactive replies off.
- **A built-in `artifacts` app** lists, shows, renames and deletes them (`artifacts:read`,
  `artifacts:write`).
- **HTML and SVG run in a viewer of their own** at `/apps/view/<token>`. The token comes from the
  App API, lasts 10 minutes and is bound to one version. The viewer has an opaque origin, no
  network, no cookies and no App API port, and it allows inline scripts unless
  `[artifacts] scripts = false`. Mermaid diagrams are rendered there by a bundled Mermaid build in
  strict mode. Only the built-in artifacts app may frame it, and it closes the
  frame when the page navigates.
- **Bounded:** size per file, per person and per bot, and a number of kept versions. Every write
  is audited.

## Consequences
- What the agent makes is kept, versioned, searchable, and editable by hand like other bundle files.
- The App API v1 grows with `artifacts.*` methods (additions only). The app frame CSP gains
  `frame-src 'self'` for one built-in app.
- A page can still navigate its frame and carry text it was written with. The viewer closes it on
  the second load. The text is what the agent already showed the same people, so this is accepted
  and documented.
- Share links for people outside the chat are left out and need their own decision.
