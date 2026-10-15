#!/usr/bin/env python3
"""Release helpers for okbot (the procedure is in docs/releasing.md).

    scripts/release.py version 0.5.0       set the workspace version and every internal dependency
    scripts/release.py order               print the crates in publishing order
    scripts/release.py publish [--dry-run] publish every crate to crates.io, in order

`publish` can be run again after a failure: versions already on crates.io are skipped. When
crates.io rate-limits new crates (HTTP 429, about one new crate every 10 minutes after a short
burst), it waits until the time crates.io gives and retries.
"""

import email.utils
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SEMVER = re.compile(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$")


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def publish_order():
    """Publishable workspace crates, dependencies (dev-dependencies too) before dependents."""
    packages = {p["name"]: p for p in metadata()["packages"] if p.get("publish") != []}
    deps = {
        name: {d["name"] for d in p["dependencies"] if d["name"] in packages and d["name"] != name}
        for name, p in packages.items()
    }
    order, done = [], set()
    while len(order) < len(packages):
        ready = sorted(n for n in packages if n not in done and deps[n] <= done)
        if not ready:
            sys.exit(f"dependency cycle among {sorted(set(packages) - done)}")
        order.extend(ready)
        done.update(ready)
    version = {p["version"] for p in packages.values()}
    if len(version) != 1:
        sys.exit(f"crates have different versions: {sorted(version)}")
    return order, version.pop()


def set_version(version):
    if not SEMVER.match(version):
        sys.exit(f"`{version}` is not a version like 0.5.0 or 0.6.0-dev")
    path = ROOT / "Cargo.toml"
    text = path.read_text()
    # [workspace.package] version
    text, n = re.subn(
        r'(\[workspace\.package\][^\[]*?\nversion = )"[^"]*"', rf'\1"{version}"', text, count=1
    )
    if n != 1:
        sys.exit("no version in [workspace.package]")
    # internal crates in [workspace.dependencies]: `okbot-x = { path = "crates/…", version = "…" }`
    text, n = re.subn(
        r'(\{ path = "(?:crates|templates)/[^"]+", version = )"[^"]*"', rf'\1"{version}"', text
    )
    path.write_text(text)
    subprocess.run(["cargo", "update", "--workspace", "--quiet"], cwd=ROOT, check=True)
    print(f"version {version} ({n} internal dependencies)")


def published(name, version):
    """Whether crates.io already has `name` at `version` (from the sparse index)."""
    lower = name.lower()
    if len(lower) <= 2:
        prefix = str(len(lower))
    elif len(lower) == 3:
        prefix = f"3/{lower[0]}"
    else:
        prefix = f"{lower[:2]}/{lower[2:4]}"
    url = f"https://index.crates.io/{prefix}/{lower}"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            lines = response.read().decode().splitlines()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise
    return any(json.loads(line).get("vers") == version for line in lines if line.strip())


def retry_after(output):
    """Seconds to wait from a crates.io 429 message ("Please try again after <date>")."""
    match = re.search(r"try again after ([^.]+?(?:GMT|UTC|\+0000))", output)
    if match:
        try:
            when = email.utils.parsedate_to_datetime(match.group(1))
            return max(30, int((when - datetime.now(timezone.utc)).total_seconds()) + 15)
        except (TypeError, ValueError):
            pass
    return 600


def publish(dry_run):
    order, version = publish_order()
    if dry_run:
        # Before the first publish the dependencies are not on crates.io: cargo packages the whole
        # workspace against a local registry instead.
        subprocess.run(["cargo", "publish", "--workspace", "--dry-run", "--locked"],
                       cwd=ROOT, check=True)
        print(f"packaged {len(order)} crates at {version}")
        return
    if "-" in version:
        sys.exit(f"refusing to publish the pre-release version {version}; run `version` first")
    for name in order:
        if published(name, version):
            print(f"= {name} {version} is already on crates.io")
            continue
        command = ["cargo", "publish", "--locked", "-p", name]
        while True:
            print(f"> {' '.join(command)}", flush=True)
            run = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
            output = run.stdout + run.stderr
            if run.returncode == 0:
                break
            if "429" in output or "Too Many Requests" in output:
                wait = retry_after(output)
                print(f"  rate-limited by crates.io; retrying in {wait // 60} min {wait % 60} s",
                      flush=True)
                time.sleep(wait)
                continue
            if "already exists" in output:
                print(f"= {name} {version} is already on crates.io")
                break
            sys.stderr.write(output)
            sys.exit(f"publishing {name} failed")
    print(f"published {len(order)} crates at {version}")


def main():
    args = sys.argv[1:]
    if args[:1] == ["version"] and len(args) == 2:
        set_version(args[1])
    elif args == ["order"]:
        order, version = publish_order()
        print(f"{version}: " + " ".join(order))
    elif args[:1] == ["publish"] and args[1:] in ([], ["--dry-run"]):
        publish(dry_run=args[1:] == ["--dry-run"])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
