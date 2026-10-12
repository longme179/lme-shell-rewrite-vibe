# RESUME.md — exact bootstrap for a fresh sandbox

Written per Amendment 4 (2026-10-13). A "fresh sandbox" means the working
directory ~/lme-fedora-niri-qs is gone or broken. This file is the fast path;
the full rules live in the files it points to.

## 0. Read order (after recovery, before any code)

1. docs/MISSION_CONTINUATION.md (2026-10-12 continuation)
2. docs/MISSION_AMENDMENT_2.md (GitHub backup + recovery rules)
3. docs/MISSION_AMENDMENT_3.md (sandbox survival + Step 0)
4. docs/MISSION_AMENDMENT_4.md (keep going + work order) — wins on conflicts
5. STATE.md (current step, Environment quirks, Delivery record)
6. docs/NATIVE_PLAN.md (the coverage table — the source of truth for what
   is done/partial/missing)
7. docs/DECISIONS.md (D-numbered lessons; read the D20+ tail at minimum)
8. docs/ZBUS_NOTES.md (before writing ANY D-Bus code)

## 1. Credentials (from the owner, never in any file)

- GitHub token: export as shown by the owner's session setup (checkpoint.sh
  reads its own copy — do not print it).
- Webhook: `export DISCORD_WEBHOOK_URL="$(cat ~/.lme-webhook)"` — the file is
  created by the owner, chmod 600, OUTSIDE every repo.
- If neither is present and the owner did not resend them: write
  "RESET: no credentials" in the chat and stop.

## 2. Recover the repo (Recovery path b of Amendment 2, condensed)

    git init ~/lme-fedora-niri-qs && cd ~/lme-fedora-niri-qs
    git remote add origin https://github.com/longme179/lme-fedora-niri-qs.git
    git fetch origin
    git fetch https://github.com/longme179/lme-shell-rewrite-vibe.git \
      'refs/heads/rewrite/*:refs/heads/rewrite/*' 'refs/heads/wip/*:refs/remotes/vibe-wip/*'
    git checkout rewrite/native
    # if refs/remotes/vibe-wip/rewrite/native is AHEAD of rewrite/native:
    git restore --source=refs/remotes/vibe-wip/rewrite/native --staged --worktree .
    git add -A && git commit -m "restore wip snapshot"

(If path b fails, fall back to paths c/d/e in docs/MISSION_AMENDMENT_2.md —
repo bundles in the backup repo main, then source-export reconstruction.)

## 3. Sandbox environment (every shell)

    cd ~/lme-fedora-niri-qs && . scripts/env.sh

That sets PATH, CARGO_TARGET_DIR=$HOME/target, CARGO_INCREMENTAL=0 and the
pkg-config shims (see STATE.md "Environment"). The shims live in
/home/z/work/pkgconfig-fake and are NOT committed — if missing, rebuild them
per DECISIONS D18 (handwritten wayland.pc/xkbcommon.pc + .so symlinks onto
the system runtime libs).

    rustup component add clippy rustfmt   # after a fresh sandbox reset
    export DISCORD_WEBHOOK_URL="$(cat ~/.lme-webhook)"

## 4. Hard sandbox rules (Amendment 3, permanent)

- cargo runs FOREGROUND only; the sandbox kills background jobs.
- Keep every command under ~5 minutes; `cargo fetch` first, then build per
  crate (`cargo build -p lme-backend`, then `-p lme-shell`). Workspace
  release build ~4.5 min — if in doubt, split.
- `scripts/gh-sync.sh` after every commit / edit group, ~10-minute cadence.
- Checkpoints: `scripts/checkpoint.sh "<note>"` (lean). Deliveries:
  `scripts/deliver.sh <label>` — now staged/resumable (Amendment 4); if a
  stage times out, just re-run the same command, it skips finished stages.

## 5. Verify the tree

    cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
    cargo build --workspace --locked && cargo test --workspace --locked

(137+ tests green at TIER-1-COMPLETE, commit 1df00cc/fda0ed5.)

## 6. Continue

Open STATE.md "Current step" and docs/NATIVE_PLAN.md missing rows; the work
order is in docs/MISSION_AMENDMENT_4.md §"Work order". Never declare the
session complete before FINAL is delivered.
