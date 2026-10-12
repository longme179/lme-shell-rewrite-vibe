#!/usr/bin/env bash
# scripts/deliver.sh <label> — staged, RESUMABLE delivery (Amendment 4).
# Label "FINAL" for the last delivery of the session.
#
# Stages (each idempotent, each skipped if its marker exists; a timeout just
# means re-running the same command — finished stages are skipped and cargo
# resumes from the persistent replay target dir):
#   gates        fmt + clippy + build + test in the WARM main target dir
#   export       git archive → /tmp/lme-deliver-<label>-<commit>/lme-shell
#   replay-fetch cargo fetch --locked in the export (network phase alone)
#   replay-build cargo test --release --locked --no-run in the export, in the
#                ONE dedicated replay target dir (release-only, Amendment 4)
#   replay-test  cargo test --release --locked in the export; on pass the
#                replay target dir is DELETED immediately (disk back)
#   scanzip      secret scan (fail closed) + zip + README copy
#   send         zip + README + summary via webhook, then full cleanup
#
# Aborts early when free disk < 3 GB (checked before every disk-heavy stage).
# scripts/deliver.sh --clean removes state + export + replay dirs.
set -u

LABEL="${1:-}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/target}"
export CARGO_INCREMENTAL=0
# The sandbox has no system wayland/xkb dev files: use the user-level shims
# (see STATE.md / scripts/env.sh). On a real Fedora machine with the dnf
# packages from README these are no-ops.
export PKG_CONFIG_PATH="/home/z/work/pkgconfig-fake/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export RUSTFLAGS="-L /home/z/work/pkgconfig-fake/lib ${RUSTFLAGS:-}"

REPLAY_TARGET="$HOME/deliver-replay-target"   # the ONE dedicated dir
MIN_FREE_GB=3

fail() { echo "DELIVERY FAILED: $*" >&2; exit 1; }

# ---- state ------------------------------------------------------------------
STATE_DIR="$REPO_ROOT/.deliver-state"
mkdir -p "$STATE_DIR"

if [ "$LABEL" = "--clean" ]; then
  echo "==> deliver --clean: removing state, export dirs and replay target"
  rm -rf "$STATE_DIR" /tmp/lme-deliver-* "$REPLAY_TARGET"
  echo "    done"; exit 0
fi
[ -n "$LABEL" ] || { echo "usage: deliver.sh <label> | --clean" >&2; exit 1; }

COMMIT="$(git rev-parse --short HEAD)"
DATE="$(date +%Y%m%d)"
RUN="$STATE_DIR/$LABEL-$COMMIT"          # new commit => new run (fresh start)
mkdir -p "$RUN"
EXPORT_PARENT="/tmp/lme-deliver-$LABEL-$COMMIT"
EXPORT_DIR="$EXPORT_PARENT/lme-shell"

done_marker() { [ -f "$RUN/stage-$1.done" ]; }
mark_done()  { : > "$RUN/stage-$1.done"; }

free_gb() { df -B1G --output=avail / | tail -1 | tr -dc '0-9'; }
need_disk() {  # need_disk <stage-name>
  local avail; avail="$(free_gb)"
  [ "$avail" -ge "$MIN_FREE_GB" ] || fail "stage $1: only ${avail}G free (<${MIN_FREE_GB}G). Free disk (deliver.sh --clean, rm stale target dirs) and re-run — stages resume."
  echo "    [disk] ${avail}G free (>=${MIN_FREE_GB}G) — ok for $1"
}

echo "==> deliver $LABEL  (commit $COMMIT, $DATE, run state: $RUN)"
ls "$RUN"/*.done 2>/dev/null | while read -r m; do echo "    already done: $(basename "$m" .done)"; done

# ---- stage: gates -------------------------------------------------------------
if done_marker gates; then
  echo "==> [gates] skipped (done)"
else
  echo "==> [gates] fmt + clippy + build + test (warm main target dir)"
  cargo fmt --all --check || fail "cargo fmt"
  cargo clippy --workspace --all-targets -- -D warnings || fail "cargo clippy"
  # build every workspace binary first: the golden e2e tests spawn
  # mock-compositor from $CARGO_TARGET_DIR, and cargo may otherwise run the
  # lme-shell tests before the mock-compositor bin target is built.
  cargo build --workspace --locked || fail "cargo build"
  cargo test --workspace --locked || fail "cargo test"
  mark_done gates
fi

# ---- stage: export ------------------------------------------------------------
if done_marker export; then
  echo "==> [export] skipped (done): $EXPORT_DIR"
else
  need_disk export
  echo "==> [export] git archive → $EXPORT_DIR"
  rm -rf "$EXPORT_PARENT"
  mkdir -p "$EXPORT_DIR"
  git archive HEAD | tar -x -C "$EXPORT_DIR" || fail "git archive"
  (
    cd "$EXPORT_DIR" || exit 1
    rm -rf legacy docs/previews assets/wallpapers || exit 1
  ) || fail "prune export"
  cat > "$EXPORT_DIR/EXPORT_NOTES.txt" <<EOF
LME Shell native source export — label $LABEL, commit $COMMIT, date $DATE.

Excluded from this archive (they live in the upstream repository, branch
rewrite/native and main):
- legacy/        the original QML/Quickshell + Python shell (executable spec)
- docs/previews/ QML render previews
- assets/wallpapers/ original wallpaper art

Everything else is the complete Rust workspace. Build: see README.md.
EOF
  mark_done export
fi
[ -d "$EXPORT_DIR" ] || fail "export dir vanished — re-run (export stage will redo)"

# ---- stage: replay-fetch ------------------------------------------------------
if done_marker replay-fetch; then
  echo "==> [replay-fetch] skipped (done)"
else
  echo "==> [replay-fetch] cargo fetch --locked in export (network phase alone)"
  ( cd "$EXPORT_DIR" && cargo fetch --locked ) || fail "replay fetch"
  mark_done replay-fetch
fi

# ---- stage: replay-build (release-only, resumable across timeouts) ------------
if done_marker replay-build; then
  echo "==> [replay-build] skipped (done)"
else
  need_disk replay-build
  echo "==> [replay-build] build bins+examples + test --no-run (target: $REPLAY_TARGET)"
  # cargo test --no-run alone does NOT produce the plain bin paths the e2e
  # harness spawns (target/<profile>/mock-compositor) nor the examples
  # (fake_sni/fake_mpris/fake_bluez) — build them explicitly first.
  ( cd "$EXPORT_DIR" && CARGO_TARGET_DIR="$REPLAY_TARGET" CARGO_INCREMENTAL=0 \
      cargo build --release --locked --workspace --bins --examples ) \
    || fail "replay release build of bins/examples (re-run deliver.sh — cargo resumes from $REPLAY_TARGET)"
  ( cd "$EXPORT_DIR" && CARGO_TARGET_DIR="$REPLAY_TARGET" CARGO_INCREMENTAL=0 \
      cargo test --release --locked --no-run ) \
    || fail "replay release test build (re-run deliver.sh — cargo resumes from $REPLAY_TARGET)"
  mark_done replay-build
fi

# ---- stage: replay-test (release-only; deletes the target dir on pass) --------
if done_marker replay-test; then
  echo "==> [replay-test] skipped (done)"
else
  need_disk replay-test
  echo "==> [replay-test] cargo test --release --locked in export"
  ( cd "$EXPORT_DIR" && CARGO_TARGET_DIR="$REPLAY_TARGET" CARGO_INCREMENTAL=0 \
      cargo test --release --locked ) \
    || { rm -rf "$REPLAY_TARGET"; fail "replay test in export"; }
  # Amendment 4: delete the replay target dir RIGHT AFTER the replay passes.
  rm -rf "$REPLAY_TARGET"
  mark_done replay-test
fi

# ---- stage: scanzip -----------------------------------------------------------
ZIP=""
if done_marker scanzip; then
  echo "==> [scanzip] skipped (done)"
  ZIP="$EXPORT_PARENT/lme-shell-src-$LABEL-$DATE.zip"
else
  need_disk scanzip
  echo "==> [scanzip] secret scan (export, fail closed) + zip + README"
  eval "$(grep '^PAT=' scripts/checkpoint.sh)" || fail "extract PAT from checkpoint.sh"
  ( cd "$EXPORT_DIR" && git grep --no-index -I -q -E "$PAT" ); rc=$?
  [ "$rc" -eq 0 ] && fail "secret scan: pattern FOUND in export (rc=0)"
  [ "$rc" -gt 1 ] && fail "secret scan: git grep error rc=$rc"
  echo "    export clean (rc=1)"
  ZIP="$EXPORT_PARENT/lme-shell-src-$LABEL-$DATE.zip"
  ( cd "$EXPORT_PARENT" && zip -qr "$ZIP" lme-shell ) || fail "zip"
  cp "$REPO_ROOT/README.md" "$EXPORT_PARENT/README.md" || fail "copy README"
  mark_done scanzip
fi
[ -f "$ZIP" ] || fail "zip vanished ($ZIP) — re-run (scanzip stage will redo)"

# ---- stage: send --------------------------------------------------------------
if done_marker send; then
  echo "==> [send] skipped (done)"
else
  echo "==> [send] via send-file.sh"
  export DISCORD_WEBHOOK_URL="${DISCORD_WEBHOOK_URL:-$(cat ~/.lme-webhook 2>/dev/null || true)}"
  [ -n "${DISCORD_WEBHOOK_URL:-}" ] || fail "no webhook (DISCORD_WEBHOOK_URL / ~/.lme-webhook)"

  # Coverage counts from the plan (kept honest by hand at each tier end)
  DONE=$(grep -c '| done' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)
  PART=$(grep -c '| partial' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)
  MISS=$(grep -c '| missing' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)
  EXT=$(grep -c '| external' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)

  bash "$REPO_ROOT/scripts/send-file.sh" "$ZIP" || fail "send zip"
  mark_done send   # written between zip-send and summary so a retry at most re-sends the summary
  bash "$REPO_ROOT/scripts/send-file.sh" "$EXPORT_PARENT/README.md" || fail "send README"

  SUMMARY=$(head -c 1800 <<EOF
[$LABEL] LME Shell native source — commit $COMMIT ($DATE)
Coverage (NATIVE_PLAN rows): done=$DONE partial=$PART missing=$MISS external=$EXT
Build: cargo build --release --locked (workspace, offline-check via --locked)
Replay: cargo test --release --locked green in the export tree (staged deliver).
Top limitations: see README "Known limitations" (real-machine validation list).
Split parts: reassemble with: cat lme-shell-src-*.zip.part* > lme-shell-src.zip
EOF
)
  curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H 'Content-Type: application/json' \
    -d "$(python3 -c 'import json,sys; print(json.dumps({"content": sys.argv[1]}))' "$SUMMARY")" \
    "$DISCORD_WEBHOOK_URL" | grep -q '^20' || fail "send summary message"
  echo "==> summary message sent"
fi

# ---- cleanup on full success ---------------------------------------------------
rm -rf "$EXPORT_PARENT" "$RUN"
echo "==> DELIVERY $LABEL COMPLETE (state cleaned; replay target already deleted after replay)"
