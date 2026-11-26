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


class Chat:
    """`okbot chat` with the echo agent, driven through stdin."""

    def __init__(self, bot: Path, port: int):
        subprocess.run(
            [OKBOT, "init", "--name", "E2E", "--agent", "claude", "--profile", "personal",
             "--platform", "none", "--yes"],
            cwd=bot,
            check=True,
            capture_output=True,
        )
        self.port = port
        self.proc = subprocess.Popen(
            [OKBOT, "chat", "--agent", "echo", "--http", f"127.0.0.1:{port}"],
            cwd=bot,
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
                    lambda m: m.type == "error" and problems.append(f"console: {m.text}"),
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
    for problem in problems:
        print(problem, file=sys.stderr)
    if not problems:
        print(f"apps e2e ({BROWSER}): ok")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
