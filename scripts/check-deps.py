#!/usr/bin/env python3
"""Enforce the crate dependency rules of docs/implementation-plan.md §2.2.

Reads `cargo metadata` and fails when an okbot crate depends on an okbot crate it must not use.
Dev-dependencies are checked too, except that any crate may use `okbot-testkit` for tests.
"""
import json
import subprocess
import sys

CORE = "okbot-core"
TESTKIT = "okbot-testkit"
CLI = "okbot"

# crate -> okbot crates it may depend on (normal and build dependencies).
ALLOWED = {
    CORE: set(),
    "okbot-config": {CORE},
    "okbot-render": {CORE},
    "okbot-chart": {CORE},
    "okbot-store": {CORE},
    "okbot-agent": {CORE},
    "okbot-knowledge": {CORE},
    "okbot-runtime": {CORE},
    "okbot-peer": {CORE},
    TESTKIT: {CORE},
}


def allowed(crate: str) -> set[str] | None:
    if crate == CLI:
        return None  # the composition root may use everything
    if crate.startswith("okbot-channel-"):
        return {CORE, "okbot-render"}
    return ALLOWED.get(crate)


def main() -> int:
    meta = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"])
    )
    errors = []
    for pkg in meta["packages"]:
        name = pkg["name"]
        if not name.startswith("okbot"):
            continue
        rules = allowed(name)
        if rules is None and name != CLI:
            errors.append(f"{name}: no dependency rule; add it to scripts/check-deps.py")
            continue
        for dep in pkg["dependencies"]:
            target = dep["name"]
            if not target.startswith("okbot") or rules is None:
                continue
            if dep["kind"] == "dev" and target == TESTKIT:
                continue
            if target not in rules:
                errors.append(f"{name} -> {target} ({dep['kind'] or 'normal'}) is not allowed")
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    if not errors:
        print("crate dependency rules: ok")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
