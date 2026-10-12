#!/usr/bin/env bash
# Runs every quality gate of docs/implementation-plan.md §1.5 and stops at the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step fmt;          cargo fmt --all -- --check
step clippy;       cargo clippy --workspace --all-targets --all-features -- -D warnings
step test;         cargo test --workspace --all-features --quiet
step docs;         RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --quiet
step architecture; python3 scripts/check-deps.py
step deny;         cargo deny check
if [[ "${OKBOT_VERIFY_MSRV:-0}" == 1 ]]; then
  step msrv;       cargo +1.89 check --workspace --all-targets --all-features
fi
printf '\nall checks passed\n'
