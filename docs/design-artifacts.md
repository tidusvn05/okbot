# Design — artifacts: pages and files the agent makes in chat

Status: Accepted (2026-11-27). Decision record: [ADR 0021](adr/0021-artifacts.md). Builds on apps
(ADR 0019, [`design-apps.md`](design-apps.md)) and the app hints of
[`design-app-hints.md`](design-app-hints.md).

## 0. Summary

People ask the bot for things that do not fit a chat message: a report page, a comparison table,
a checklist, a small calculator, a diagram. Today the agent pastes them as text, and they scroll
away. With **artifacts** the agent saves the result as a file (HTML, Markdown, SVG, Mermaid, CSV,
JSON or plain text). Chat shows a short summary and an **Open** button. The built-in **artifacts** app lists
them, shows them, keeps their versions and lets people rename and delete them. Asking again ("make
the table wider") saves a new version of the same artifact.

```
👤  make a one-page comparison of the three suppliers
🤖  Saved "Supplier comparison" (v1): A is cheapest, B delivers fastest, C has the best terms.
    [Open Supplier comparison]
```

## 1. Requirements

| | Requirement |
|---|---|
| R1 | The agent creates and updates artifacts with tools. It never writes files itself (hard rule 2) |
| R2 | Artifacts are files in the bundle (hard rule 5). SQLite holds nothing new |
| R3 | Who may see an artifact follows the same owner folders and `Scope` as tasks and memory (hard rule 4) |
| R4 | Agent-written HTML is untrusted: it runs only in a sandbox with no network, no cookies and no App API |
| R5 | Every version is kept up to a limit; sizes are bounded per file, per person and per bot |
| R6 | Works on every platform: a button where there are buttons, a `/app artifacts …` hint elsewhere |
| R7 | Every write is in `okbot audit` |

## 2. Model

```
Artifact
  id          a-<10 base32 chars>        stable across versions
  title       ≤ 80 chars, in the user's language
  kind        html | markdown | svg | mermaid | csv | json | text
  summary     ≤ 500 chars: what it is; this is what knowledge search finds
  scope       user | room                 like tasks: personal ones need a direct chat
  created_by  person or account
  versions    [{n, at, by, bytes, sha256, note}]
  tags        optional
```

### 2.1 On disk

```
kb/artifacts/<owner folder>/a-7k2m9q4x2c.md   OKF frontmatter (type: Artifact, title, kind,
                                              versions, …) + summary as body
kb/artifacts/<owner folder>/a-7k2m9q4x2c/v1.html
kb/artifacts/<owner folder>/a-7k2m9q4x2c/v2.html
```

- `<owner folder>` is the same path as for tasks and memory: `people/<person>`,
  `users/<channel>/<user>` or `rooms/<channel>/<room>`. It comes from `paths.rs`, with `artifacts/`
  in place of `memory/`. The metadata file sits beside the version folder rather than inside it,
  because `index.md` names are the folder indexes OKF keeps.
- `kb/.okbaseignore` gains `artifacts/**/a-*/`. Only the metadata file is indexed, so `kb_query
  type=Artifact` finds artifacts by title and summary. Raw HTML never feeds back into answers as
  knowledge, so a page cannot inject text into later prompts that way.
- An edit by hand (an editor, git) works like for tasks: the store re-reads the files.

### 2.2 Port

`okbot-core` gains `ArtifactStore`, an `#[async_trait]` trait like `TaskStore`. `okbot-knowledge`
implements it on the bundle. Every method takes the `Requester` and builds the scope from it:

```rust
async fn list(&self, who: &Requester, filter: &ArtifactFilter) -> Result<Vec<ArtifactMeta>>;
async fn get(&self, who: &Requester, id: &str, version: Option<u32>) -> Result<Option<ArtifactFile>>;
async fn save(&self, who: &Requester, new: NewVersion) -> Result<ArtifactMeta>;   // new or next version
async fn rename(&self, who: &Requester, id: &str, title: &str) -> Result<()>;
async fn delete(&self, who: &Requester, id: &str) -> Result<()>;
```

Writes are atomic: write to a temporary file, then rename. `index.md` is written last.

## 3. The agent's tools

They are on the `bot` MCP server, beside `task_add`. Read-only runs (automations, ADR 0013) get
only the readers.

| Tool | Does |
|---|---|
| `artifact_save {title, kind, content, summary, scope?, id?, note?}` | new artifact, or with `id` a new version. Returns `{id, version, app: {app: "artifacts", view: "/a", params: {id}}}` |
| `artifact_list {query?}` | the requester's and the room's artifacts, newest first |
| `artifact_get {id, version?}` | one version's content, cut at 64 KB with a notice |

Instructions, added to the `bot` server's:

> artifact_save: when the answer is a document, a page, a table or a diagram the user will keep,
> reuse or print, or is longer than a chat message should be. Write a complete file (for HTML: one
> page with inline CSS and JS, no external URLs, which are blocked). To change an artifact, save a
> new version with its id. In your answer, summarise it in two or three lines; the bot adds the
> Open button.

- **Scope.** As for tasks: `user` needs a direct chat, otherwise the tool answers `NOT_HERE`.
  `room` is shared with the chat.
- **Sizes.** `content` ≤ `[artifacts] max_bytes` (default 1 MB). Before writing, the store checks
  the owner's total (`quota_person`, default 50 MB) and the bot's (`quota_bot`, default 1 GB).
- **Versions.** Only the newest `keep_versions` (default 10) are kept. Older ones are deleted
  when a new one is saved, and their entries stay in `index.md` marked `pruned`.
- **The button comes from okbot, not the agent.** The runtime sees each `artifact_save` of the turn
  (the knowledge adapter reports it through the run grant) and attaches **Open <title>** to the
  reply, at most 2. This works with interactive replies off too, on every platform with buttons.
  Elsewhere the reply ends with `/app artifacts <id>`. The agent can still add one with `show`.
- **Audit.** Each save, rename and delete is a `ToolCallRecord` with server `bot`. Writes from the
  app are audited with command `app:artifacts`, as other app writes are.

## 4. The artifacts app

A built-in app, permissions `me`, `artifacts:read`, `artifacts:write`.

| View | Shows |
|---|---|
| `/` | list: title, kind, place (mine / this room), updated, size; search and filters |
| `/a` (`id`, `v`) | one artifact: the viewer, the version list, rename, delete, download |

App API v1 gains `artifacts.list`, `artifacts.get` (metadata and, for text kinds, the content),
`artifacts.open` (a viewer URL, §5), `artifacts.rename`, `artifacts.delete`. `artifacts:write`
covers rename and delete only. Apps do not create artifacts in v1: the agent and people in chat do.

Rendering by kind:

| Kind | In the app |
|---|---|
| markdown | rendered by the app's own renderer, raw HTML in it shown as text |
| csv | a table (first 5,000 rows) with sort and filter |
| json | a collapsible tree |
| text | preformatted |
| svg, html, mermaid | in the viewer frame (§5) |

## 5. The viewer: running untrusted HTML

The app frame's CSP has `default-src 'none'` and no `frame-src`. Its `srcdoc` children inherit that
policy, which blocks the inline styles and scripts HTML artifacts need. So HTML and SVG are
served from a route of their own:

```
GET /apps/view/<token>        token: 32 random bytes from artifacts.open, valid 10 minutes,
                              bound to one artifact version
```

Response headers:

```
Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';
  img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none';
  form-action 'none'; base-uri 'none'; frame-ancestors <bot origin>;
  sandbox allow-scripts
Cache-Control: no-store
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
Cross-Origin-Resource-Policy: same-origin
```

- **No network.** `connect-src 'none'` and no remote `img-src`, `font-src` or `script-src`. An
  artifact cannot fetch, beacon or load a tracking pixel.
- **No cookies, no API.** `sandbox` without `allow-same-origin` gives it an opaque origin. The
  viewer frame gets no `MessageChannel` port, so it cannot call the App API.
- **Navigation.** A frame can still navigate itself (a link, `location = …`, `<meta refresh>`) and
  put text in the URL. The artifacts app counts the viewer frame's loads. On a second load it
  removes the frame and says so, as the shell does for apps (ADR 0019). A page of the agent's can
  carry only what the agent wrote into it, and the same people already read that in chat. This is
  listed as a known risk.
- **Scripts.** On by default, because charts, sorting and small calculators are the point.
  `[artifacts] scripts = false` serves `script-src 'none'` instead.
- **Only the artifacts app may frame it.** Its manifest is built-in, and okbot adds
  `frame-src 'self'` to the app frame's CSP for that built-in only. Registry apps cannot.
- **Download.** The shell, not the sandbox, offers a download through a trusted dialog. It calls
  `artifacts.open` with `download: true` and gets `Content-Disposition: attachment`.

## 6. Configuration

```toml
[artifacts]
enabled = true            # default: on when apps are on
max_bytes = "1MB"
quota_person = "50MB"
quota_bot = "1GB"
keep_versions = 10
scripts = true
```

`okbot artifact list|show|export|rm [--bot <name>]` lets the operator see and clean up from the
terminal. `okbot doctor` reports use against the quotas.

## 7. Chat

- `/artifacts`: the 10 newest here, with Open buttons (summary first, design-apps §4.1).
- `/app artifacts <id>`: open one.
- The `artifacts` app hint (design-app-hints §6): "pages, tables, documents or diagrams made for
  the person; to find, view or reopen one".
- `artifact_list` results with more than 10 items carry `see_also` for the app.

## 8. Phases

| Phase | Delivers |
|---|---|
| A1 | `ArtifactStore`, files and `.okbaseignore`, the `artifact_*` tools, Open button on the reply, limits, audit |
| A2 | the artifacts app, `artifacts.*` App API, the viewer route, frame-navigation check |
| A3 | `/artifacts`, `okbot artifact …`, doctor, `see_also`, docs and guide pack, e2e in `scripts/e2e-apps.py` |
| Later | share links for people outside the chat (needs its own decision: public, read-only, expiring) |

## 9. Risks

| Risk | Mitigation |
|---|---|
| A prompt injection makes the agent write a page that leaks data by navigating | no network from the viewer; the second load closes it; the content is what the agent already wrote for the same audience |
| A page that looks like a login form (phishing in the bot's name) | the app shows a fixed banner above the viewer: "Made by the assistant. Never enter passwords here."; `form-action 'none'` |
| Disk filled by large or many artifacts | per-file, per-person and per-bot quotas; version pruning; `okbot artifact rm` |
| Raw HTML indexed and fed back to the agent | `.okbaseignore` keeps version files out of the index |
| A personal artifact shown in a group | the same owner folders and `NOT_HERE` rule as tasks |

## 10. Resolved questions (2026-11-27)

1. Scripts in HTML artifacts are **on by default**; `[artifacts] scripts = false` turns them off.
2. Artifacts live in the bundle, at `kb/artifacts/`.
3. **Mermaid is in v1** as a seventh kind, `mermaid`. The viewer serves the diagram source in a
   page that loads okbot's bundled Mermaid build (`/apps/sdk/mermaid.min.js`, MIT, vendored with its
   license and version). `script-src` of that page adds `'self'` for that one file. Mermaid runs
   with `securityLevel: "strict"`, so labels cannot carry HTML or click handlers.
