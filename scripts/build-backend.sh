#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the native Rust backend (release profile) for LME_SHELL_BACKEND=rust.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd -- "$repo_dir/lme-backend"
cargo build --release
printf 'Built lme-backend/target/release/lme-backend\n'
printf 'Select it for the shell with LME_SHELL_BACKEND=rust (see lme-backend/README.md).\n'
