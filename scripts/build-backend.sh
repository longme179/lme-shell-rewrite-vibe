#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the native Rust backend (release profile) for LME_SHELL_BACKEND=rust.
# Workspace member; the binary lands in $CARGO_TARGET_DIR/release/lme-backend.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd -- "$repo_dir"
: "${CARGO_TARGET_DIR:=$HOME/target}"
export CARGO_TARGET_DIR
cargo build --release -p lme-backend
printf 'Built %s/release/lme-backend\n' "$CARGO_TARGET_DIR"
printf 'Select it for the shell with LME_SHELL_BACKEND=rust (see crates/lme-backend/README.md).\n'
