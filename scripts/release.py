#!/usr/bin/env python3
"""Release helpers for okbot (the procedure is in docs/releasing.md).

    scripts/release.py version 0.5.0       set the workspace version and every internal dependency
    scripts/release.py order               print the crates in publishing order
    scripts/release.py pending             print the crates whose version is not on crates.io yet
    scripts/release.py publish [--dry-run] publish every crate to crates.io, in order
    scripts/release.py trustpub OWNER/REPO [--workflow F] [--environment E] [--crates a,b] [--only]
                                           set up crates.io Trusted Publishing for the crates

`publish` can be run again after a failure: versions already on crates.io are skipped. When
crates.io rate-limits new crates (HTTP 429, about one new crate every 10 minutes after a short
burst), it waits until the time crates.io gives and retries.

`trustpub` needs a crates.io API token with the `trusted-publishing` scope: CRATES_IO_TOKEN, or
the token saved by `cargo login`.
It lets the GitHub workflow publish the crates (by default this workspace's) without any stored
token; `--only` also makes crates.io refuse every other way of publishing them. It works for any
repository: `--crates` names the crates, `--workflow` and `--environment` default to
`release.yml` and `crates-io`. Running it again changes nothing.
"""

import email.utils
import json
import os
import re
import subprocess
import sys
import time
import tomllib
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


def index_versions(name):
    """Versions of `name` on crates.io (from the sparse index), or None for a crate it does not
    have yet."""
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
            return None
        raise
    return {json.loads(line).get("vers") for line in lines if line.strip()}


def published(name, version):
    """Whether crates.io already has `name` at `version`."""
    return version in (index_versions(name) or set())


def pending():
    """Prints the crates still to publish. With GITHUB_OUTPUT set, also writes `count` and `new`
    (crates crates.io does not have at all: Trusted Publishing cannot create them)."""
    order, version = publish_order()
    todo, new = [], []
    for name in order:
        versions = index_versions(name)
        if versions is None:
            new.append(name)
        if version not in (versions or set()):
            todo.append(name)
    for name in todo:
        print(f"{name} {version}" + (" (new crate)" if name in new else ""))
    if not todo:
        print(f"every crate is on crates.io at {version}")
    github_output = os.environ.get("GITHUB_OUTPUT")
    if github_output:
        with open(github_output, "a") as out:
            out.write(f"count={len(todo)}\nnew={' '.join(new)}\n")


USER_AGENT = "okbot-release (https://github.com/tidusvn05/okbot)"


def crates_io(method, path, token, body=None):
    request = urllib.request.Request(
        f"https://crates.io/api/v1{path}",
        method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={
            "Authorization": token,
            "User-Agent": USER_AGENT,
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")
        sys.exit(f"crates.io {method} {path}: HTTP {error.code} {detail}")


def cargo_token():
    """The crates.io token saved by `cargo login`, if any."""
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    for name in ("credentials.toml", "credentials"):
        path = home / name
        if path.is_file():
            with open(path, "rb") as f:
                return tomllib.load(f).get("registry", {}).get("token", "")
    return ""


def trustpub(args):
    token = os.environ.get("CRATES_IO_TOKEN") or cargo_token()
    if not token:
        sys.exit("run `cargo login` (or set CRATES_IO_TOKEN) with a crates.io token that has the "
                 "`trusted-publishing` scope")
    if not args or "/" not in args[0]:
        sys.exit(__doc__)
    owner, repo = args[0].split("/", 1)
    options = {"--workflow": "release.yml", "--environment": "crates-io", "--crates": None}
    only = False
    rest = args[1:]
    while rest:
        flag = rest.pop(0)
        if flag == "--only":
            only = True
        elif flag in options and rest:
            options[flag] = rest.pop(0)
        else:
            sys.exit(__doc__)
    crates = (options["--crates"].split(",") if options["--crates"] else publish_order()[0])
    environment = options["--environment"] or None
    for name in crates:
        existing = crates_io("GET", f"/trusted_publishing/github_configs?crate={name}", token)
        same = [
            c for c in existing.get("github_configs", [])
            if c["repository_owner"].lower() == owner.lower()
            and c["repository_name"].lower() == repo.lower()
            and c["workflow_filename"] == options["--workflow"]
            and c.get("environment") == environment
        ]
        if same:
            print(f"= {name}: already trusts {owner}/{repo} {options['--workflow']}")
        else:
            crates_io("POST", "/trusted_publishing/github_configs", token, {
                "github_config": {
                    "crate": name,
                    "repository_owner": owner,
                    "repository_name": repo,
                    "workflow_filename": options["--workflow"],
                    "environment": environment,
                }
            })
            print(f"+ {name}: trusts {owner}/{repo} {options['--workflow']}"
                  + (f" (environment {environment})" if environment else ""))
        if only:
            crates_io("PATCH", f"/crates/{name}", token, {"crate": {"trustpub_only": True}})
            print(f"  {name}: Trusted Publishing only")


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
    elif args == ["pending"]:
        pending()
    elif args[:1] == ["publish"] and args[1:] in ([], ["--dry-run"]):
        publish(dry_run=args[1:] == ["--dry-run"])
    elif args[:1] == ["trustpub"]:
        trustpub(args[1:])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
