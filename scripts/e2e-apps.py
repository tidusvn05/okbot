#!/usr/bin/env python3
"""Opens the tasks app in a real browser (headless Chromium) through a launch link.

It checks what unit tests cannot: that the shell signs in from the `#t=` fragment, that the
sandboxed frame loads under its CSP (opaque origin, no `allow-same-origin`), that the SDK's
postMessage bridge works, and that the shell's own confirm dialog guards a cancel.

    pip install playwright && playwright install --with-deps chromium-headless-shell
    cargo build -p okbot && python3 scripts/e2e-apps.py [target/debug/okbot]

OKBOT_E2E_BROWSER picks chromium (default), firefox or webkit (Safari's engine, which iOS in-app
browsers use). OKBOT_E2E_SHOTS=<dir> saves desktop and phone screenshots there.

Exits non-zero on any failure, a CSP violation or a script error in either page.
"""

import os
import queue
import re
import socket
import subprocess
import sys
import tempfile
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

OKBOT = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/okbot").resolve()
TIMEOUT_MS = 15_000
BROWSER = os.environ.get("OKBOT_E2E_BROWSER", "chromium")
SHOTS = os.environ.get("OKBOT_E2E_SHOTS")


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# An app that tries to leave its frame: it navigates the frame to another page, which then
# tries to call the App API. The shell must close the frame instead.
LEAVER = {
    "app.toml": 'name = "leaver"\ntitle = "Leaver"\nversion = "0.1.0"\napi = 1\npermissions = ["me"]\n',
    "index.html": '<!doctype html><html><head><meta charset="utf-8"><script src="../../sdk/v1.js">'
    '</script><script src="app.js" defer></script></head><body><h1>Leaver</h1>'
    '<p id="me"></p><button id="go" type="button">Leave</button></body></html>',
    "app.js": 'okbot.me().then((m) => { document.getElementById("me").textContent = m.name; });\n'
    'document.getElementById("go").onclick = () => { location.href = "away.html"; };\n',
    "away.html": '<!doctype html><html><head><meta charset="utf-8"><script src="away.js"></script>'
    "</head><body><h1>Away</h1></body></html>",
    "away.js": 'parent.postMessage({ okbot: 1, id: 1, method: "me", params: {} }, "*");\n',
}


# Artifacts of the console's owner (the personal profile links it to person `owner`), written
# as the store writes them (ADR 0021). The page checks that its scripts run, that it cannot
# reach the network, and offers a way to navigate its frame.
PAGE = """<!doctype html><html><head><meta charset="utf-8"><style>h1{color:#4f46e5}</style></head>
<body><h1>E2E page</h1><p id="ok">scripts off</p><p id="net">trying</p>
<button id="leave" type="button" onclick="location.search='?again'">Leave</button>
<script>
document.getElementById("ok").textContent = "scripts ran";
fetch("/apps/api/session", {method: "POST"}).then(
  () => { document.getElementById("net").textContent = "reached"; },
  () => { document.getElementById("net").textContent = "blocked"; });
</script></body></html>
"""
ARTIFACTS = [
    ("a-e2epage01", "E2E page", "html", PAGE),
    ("a-e2enotes1", "E2E notes", "markdown", "# Notes\n\n- a **bold** item\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"),
    ("a-e2eflow01", "E2E flow", "mermaid", "graph TD\n  Start --> Done\n"),
]
EXTENSIONS = {"html": "html", "markdown": "md", "mermaid": "mmd"}


def seed_artifacts(bot: Path):
    import hashlib
    import json

    folder = bot / "kb" / "artifacts" / "people" / "owner"
    folder.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    for short, title, kind, content in ARTIFACTS:
        data = content.encode()
        version = {"n": 1, "at": now, "by": "console/you", "bytes": len(data),
                   "sha256": hashlib.sha256(data).hexdigest(), "note": "", "pruned": False}
        (folder / f"{short}.md").write_text(
            "---\ntype: Artifact\n"
            f"title: {json.dumps(title)}\nid: {short}\nkind: {kind}\nscope: user\n"
            f"author: \"console/you\"\ncreated: \"{now}\"\nversions: {json.dumps([version])}\n"
            f"---\n\n# {title}\n\nMade by the browser check.\n"
        )
        (folder / short).mkdir(exist_ok=True)
        (folder / short / f"v1.{EXTENSIONS[kind]}").write_bytes(data)


class Chat:
    """`okbot chat` with the echo agent, driven through stdin."""

    def __init__(self, bot: Path, port: int):
        # Its own okbot home, so the machine's list of bots is left alone.
        env = {**os.environ, "OKBOT_HOME": str(bot / ".okbot-home")}
        subprocess.run(
            [OKBOT, "init", "--name", "E2E", "--agent", "claude", "--profile", "personal",
             "--platform", "none", "--yes"],
            cwd=bot,
            env=env,
            check=True,
            capture_output=True,
        )
        leaver = bot / "leaver-src"
        leaver.mkdir()
        for name, text in LEAVER.items():
            (leaver / name).write_text(text)
        subprocess.run(
            [OKBOT, "app", "add", str(leaver), "--yes"],
            cwd=bot,
            env=env,
            check=True,
            capture_output=True,
        )
        seed_artifacts(bot)
        self.port = port
        self.proc = subprocess.Popen(
            [OKBOT, "chat", "--agent", "echo", "--http", f"127.0.0.1:{port}"],
            cwd=bot,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        self.lines: queue.Queue[str] = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            self.lines.put(line)

    def link(self, app: str) -> str:
        self.proc.stdin.write(f"/app {app}\n")
        self.proc.stdin.flush()
        pattern = re.compile(rf"http://127\.0\.0\.1:{self.port}/apps/#t=[A-Za-z0-9_-]+")
        seen = []
        while True:
            try:
                line = self.lines.get(timeout=20)
            except queue.Empty:
                sys.exit("no launch link from okbot chat:\n" + "".join(seen))
            seen.append(line)
            if m := pattern.search(line):
                return m.group(0)

    def close(self):
        self.proc.terminate()
        self.proc.wait(timeout=10)


def main() -> int:
    problems: list[str] = []
    with tempfile.TemporaryDirectory() as bot:
        chat = Chat(Path(bot), free_port())
        try:
            link = chat.link("tasks")
            with sync_playwright() as p:
                browser = getattr(p, BROWSER).launch()
                page = browser.new_page(locale="en-US")
                page.set_default_timeout(TIMEOUT_MS)
                page.on(
                    "console",
                    lambda m: m.type == "error"
                    and problems.append(f"console ({m.location.get('url') or 'no url'}): {m.text}"),
                )
                page.on("pageerror", lambda e: problems.append(f"script error: {e}"))
                page.goto(link)

                # The ticket leaves the address bar once it is exchanged.
                page.wait_for_function("!location.hash.includes('t=')")
                expect(page.locator("#bar")).to_be_visible()
                frame = page.frame_locator("iframe")
                expect(frame.locator("#add-button")).to_have_text(re.compile(r"\S"))

                # A write through the SDK bridge.
                frame.locator("#new-title").fill("Browser check")
                frame.locator("#add-button").click()
                item = frame.locator("#list li", has_text="Browser check")
                expect(item).to_be_visible()
                if SHOTS:
                    Path(SHOTS).mkdir(parents=True, exist_ok=True)
                    page.screenshot(path=f"{SHOTS}/tasks-{BROWSER}-desktop.png")
                    phone = browser.new_page(viewport={"width": 390, "height": 844})
                    phone.goto(chat.link("tasks"))
                    expect(
                        phone.frame_locator("iframe").locator("#list li", has_text="Browser check")
                    ).to_be_visible()
                    phone.screenshot(path=f"{SHOTS}/tasks-{BROWSER}-phone.png")
                    phone.close()

                # Cancel asks in the shell's own dialog (outside the frame); declining keeps it.
                item.get_by_role("button", name="Cancel").click()
                dialog = page.get_by_role("dialog")
                expect(dialog).to_be_visible()
                dialog.get_by_role("button", name="Cancel").click()
                expect(dialog).to_be_hidden()
                expect(item).to_be_visible()

                # Editing in place: the new title is saved through tasks.update.
                item.get_by_role("button", name="Edit").click()
                frame.locator("form.edit input").first.fill("Browser check edited")
                frame.locator("form.edit").get_by_role("button", name="Save").click()
                expect(frame.locator("#list li", has_text="Browser check edited")).to_be_visible()
                print("  edited a task in place")

                # Done goes through, and the list updates.
                item.get_by_role("button", name="Done").click()
                expect(frame.locator("#list li", has_text="Browser check")).to_have_count(0)

                # Every built-in app loads in the frame without a CSP or script error.
                links = page.locator("#apps a")
                for i in range(links.count()):
                    name = links.nth(i).inner_text()
                    links.nth(i).click()
                    expect(links.nth(i)).to_have_attribute("aria-current", "page")
                    expect(page.frame_locator("iframe").locator("h1")).not_to_be_empty()
                    page.wait_for_load_state("networkidle")
                    print(f"  opened {name}")

                # The schedule adds a repeating reminder and shows it on its calendar.
                page.locator("#apps a", has_text="Schedule").click()
                schedule = page.frame_locator("iframe")
                schedule.locator("input[placeholder]").fill("Stand-up")
                # Tomorrow, so it falls in the first 7 days (the test bot's zone is UTC).
                tomorrow = (datetime.now(timezone.utc) + timedelta(days=1)).strftime("%Y-%m-%d")
                schedule.locator("input[type=datetime-local]").fill(f"{tomorrow}T09:30")
                schedule.locator("select").select_option("daily")
                schedule.get_by_role("button", name="Add").click()
                expect(schedule.locator(".list li", has_text="Stand-up").first).to_be_visible()
                schedule.get_by_role("button", name="30 days").click()
                schedule.get_by_role("button", name="Calendar").click()
                expect(schedule.locator(".cal")).to_be_visible()
                if SHOTS:
                    page.screenshot(path=f"{SHOTS}/schedule-{BROWSER}-desktop.png")
                print("  added a reminder and opened the calendar")

                # Artifacts: a page runs its scripts in the viewer without network, Markdown is
                # rendered by the app, Mermaid draws, a download asks first, and a viewer that
                # navigates itself is closed.
                page.locator("#apps a", has_text="Artifacts").click()
                arts = page.frame_locator("iframe")
                expect(arts.locator(".list li")).to_have_count(len(ARTIFACTS))
                arts.locator(".list li", has_text="E2E page").click()
                viewer = arts.frame_locator("iframe.viewer")
                expect(viewer.locator("#ok")).to_have_text("scripts ran")
                expect(viewer.locator("#net")).to_have_text("blocked")
                expect(arts.locator(".banner")).to_contain_text("Never enter passwords")
                expect(arts.locator("#app")).not_to_contain_text("null")
                if SHOTS:
                    page.screenshot(path=f"{SHOTS}/artifact-{BROWSER}-page.png")
                arts.get_by_role("button", name="Download").click()
                with page.expect_download() as download:
                    page.locator(".dialog button.primary").click()
                # WebKit swaps spaces for underscores in a suggested name.
                if download.value.suggested_filename not in ("E2E page.html", "E2E_page.html"):
                    problems.append(f"download named {download.value.suggested_filename!r}")
                viewer.locator("#leave").click()
                expect(arts.locator("iframe.viewer")).to_have_count(0)
                expect(arts.locator(".banner")).to_contain_text("another page")
                arts.locator("a.back").click()
                arts.locator(".list li", has_text="E2E notes").click()
                expect(arts.locator(".md table td").first).to_have_text("1")
                expect(arts.locator(".md strong")).to_have_text("bold")
                arts.locator("a.back").click()
                arts.locator(".list li", has_text="E2E flow").click()
                flow = arts.frame_locator("iframe.viewer")
                expect(flow.locator("body[data-ready='1'] #out svg")).to_be_visible()
                expect(flow.locator("#out")).to_contain_text("Done")
                if SHOTS:
                    page.screenshot(path=f"{SHOTS}/artifact-{BROWSER}-mermaid.png")
                print("  artifacts: page, download, navigation, markdown and mermaid")

                # An app that navigates its frame away is closed, and its new page hears nothing.
                page.locator("#apps a", has_text="Leaver").click()
                leaver = page.frame_locator("iframe")
                expect(leaver.locator("#me")).not_to_be_empty()
                leaver.locator("#go").click()
                expect(page.locator("iframe")).to_have_count(0)
                expect(page.locator("#status")).to_contain_text("another page")
                print("  a frame that navigated away was closed")

                # The ticket was single use: opening the same link again does not sign in.
                second = browser.new_page()
                second.goto(link)
                expect(second.locator("#bar")).to_be_hidden()
                expect(second.locator("#status")).not_to_have_text("…")
                browser.close()
        except Exception as error:  # noqa: BLE001 - report every failure the same way
            problems.append(f"{type(error).__name__}: {error}")
        finally:
            chat.close()
    # The second page's refused sign-in is expected to log a failed request.
    problems = [p for p in problems if "401" not in p and "403" not in p]
    # The artifact page's fetch is meant to be refused (connect-src 'none'). WebKit reports it
    # without the page's URL, naming only the address the page tried.
    problems = [
        p for p in problems
        if not ("connect" in p.lower() and ("/apps/view/" in p or "/apps/api/session" in p))
    ]
    # WebKit (WPE) styles `<select>` with an internal stylesheet and reports the page's CSP as
    # refusing it; pages render correctly. Chromium and Firefox still catch real inline styles.
    if BROWSER == "webkit":
        problems = [p for p in problems if "Refused to apply a stylesheet" not in p]
    for problem in problems:
        print(problem, file=sys.stderr)
    if not problems:
        print(f"apps e2e ({BROWSER}): ok")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
