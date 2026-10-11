#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Backend gate: Python suite + shell syntax + demo snapshot, and (with
# --rust) the native backend gate fmt/clippy/test. The Rust gate no-ops
# with a clear message when cargo is unavailable.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd -- "$repo_dir"

rust=false
for argument in "$@"; do
    case "$argument" in
        --rust) rust=true ;;
        *) printf 'usage: %s [--rust]\n' "$0" >&2; exit 2 ;;
    esac
done

python3 -m unittest discover -s tests -v
for script in scripts/*.sh; do
    bash -n "$script"
done
python3 scripts/desktop_backend.py --demo --once | python3 -c '
import json, sys
state = json.load(sys.stdin)
assert state["apps"] and state["niri"]["workspaces"]
assert state["preferences"]["language"] == "en"
print("Demo backend snapshot: valid")
'

if $rust; then
    if command -v cargo >/dev/null 2>&1; then
        (cd lme-backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test)
        printf 'Rust backend gate: passed\n'
    else
        printf 'cargo not found; skipping the Rust backend gate.\n' >&2
        printf 'Install Rust (https://rustup.rs) or run on a machine with cargo.\n' >&2
    fi
fi

printf 'Backend tests and Bash syntax checks passed.\n'
printf 'For GUI checks, use scripts/start.sh --preview in a Wayland session.\n'
