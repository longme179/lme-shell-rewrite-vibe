#!/usr/bin/env bash
# scripts/deliver.sh <label> — build, gate, export, replay, scan, zip, send.
# Label "FINAL" for the last delivery of the session.
# Usage: scripts/deliver.sh TIER-0   (or FINAL)
set -u

LABEL="${1:?usage: deliver.sh <label>}"
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

DATE="$(date +%Y%m%d)"
COMMIT="$(git rev-parse --short HEAD)"
echo "==> deliver $LABEL  (commit $COMMIT, $DATE)"

fail() { echo "DELIVERY FAILED: $*" >&2; exit 1; }

# ---------- 1. Gates ----------
echo "==> gates: fmt + clippy + build + test"
cargo fmt --all --check || fail "cargo fmt"
cargo clippy --workspace --all-targets -- -D warnings || fail "cargo clippy"
# build every workspace binary first: the golden e2e tests spawn
# mock-compositor from $CARGO_TARGET_DIR, and cargo may otherwise run the
# lme-shell tests before the mock-compositor bin target is built.
cargo build --workspace --locked || fail "cargo build"
cargo test --workspace --locked || fail "cargo test"

# ---------- 2. Export ----------
T="$(mktemp -d /tmp/lme-deliver.XXXXXX)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/lme-shell"
git archive HEAD | tar -x -C "$T/lme-shell" || fail "git archive"
(
  cd "$T/lme-shell" || exit 1
  rm -rf legacy docs/previews assets/wallpapers || exit 1
) || fail "prune export"
cat > "$T/lme-shell/EXPORT_NOTES.txt" <<EOF
LME Shell native source export — label $LABEL, commit $COMMIT, date $DATE.

Excluded from this archive (they live in the upstream repository, branch
rewrite/native and main):
- legacy/        the original QML/Quickshell + Python shell (executable spec)
- docs/previews/ QML render previews
- assets/wallpapers/ original wallpaper art

Everything else is the complete Rust workspace. Build: see README.md.
EOF

# ---------- 3. Replay (fresh target dir; frees nothing here — disk checked) ----------
# LME_REPLAY_TARGET overrides the fresh dir: the sandbox kills long-running
# foreground jobs, so a full cold replay is prewarmed in chunks ahead of
# time (same from-scratch dep set, split across commands — see STATE.md
# Environment). The export tree's own crates always build from the export.
echo "==> replay build in export"
REPLAY_TARGET="${LME_REPLAY_TARGET:-$(mktemp -d /tmp/lme-replay-target.XXXXXX)}"
(
  cd "$T/lme-shell" || exit 1
  CARGO_TARGET_DIR="$REPLAY_TARGET" CARGO_INCREMENTAL=0 \
    PKG_CONFIG_PATH="$PKG_CONFIG_PATH" RUSTFLAGS="$RUSTFLAGS" \
    cargo build --release --locked || exit 1
  # debug bins too: the e2e golden tests spawn workspace binaries
  CARGO_TARGET_DIR="$REPLAY_TARGET" CARGO_INCREMENTAL=0 \
    PKG_CONFIG_PATH="$PKG_CONFIG_PATH" RUSTFLAGS="$RUSTFLAGS" \
    cargo build --workspace --locked || exit 1
  CARGO_TARGET_DIR="$REPLAY_TARGET" CARGO_INCREMENTAL=0 \
    PKG_CONFIG_PATH="$PKG_CONFIG_PATH" RUSTFLAGS="$RUSTFLAGS" \
    cargo test --locked || exit 1
) || { rm -rf "$REPLAY_TARGET"; fail "replay build/test in export"; }
rm -rf "$REPLAY_TARGET"

# ---------- 4. Secret scan of the export (fail closed) ----------
echo "==> secret scan (export)"
eval "$(grep '^PAT=' scripts/checkpoint.sh)" || fail "extract PAT from checkpoint.sh"
cd "$T/lme-shell" || fail "cd export"
git grep --no-index -I -q -E "$PAT"; rc=$?
if [ "$rc" -eq 0 ]; then fail "secret scan: pattern FOUND in export (rc=0)"; fi
if [ "$rc" -gt 1 ]; then fail "secret scan: git grep error rc=$rc"; fi
echo "    export clean (rc=1)"
cd "$REPO_ROOT" || fail "cd back"

# ---------- 5. Zip + README copy ----------
ZIP="$T/lme-shell-src-$LABEL-$DATE.zip"
( cd "$T" && zip -qr "$ZIP" lme-shell ) || fail "zip"
cp "$REPO_ROOT/README.md" "$T/README.md" || fail "copy README"

# ---------- 6. Send ----------
echo "==> sending via send-file.sh"
export DISCORD_WEBHOOK_URL="${DISCORD_WEBHOOK_URL:-$(cat ~/.lme-webhook 2>/dev/null || true)}"
[ -n "${DISCORD_WEBHOOK_URL:-}" ] || fail "no webhook (DISCORD_WEBHOOK_URL / ~/.lme-webhook)"

# Coverage counts from the plan (kept honest by hand at each tier end)
DONE=$(grep -c '| done' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)
PART=$(grep -c '| partial' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)
MISS=$(grep -c '| missing' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)
EXT=$(grep -c '| external' "$REPO_ROOT/docs/NATIVE_PLAN.md" || true)

bash "$REPO_ROOT/scripts/send-file.sh" "$ZIP" || fail "send zip"
bash "$REPO_ROOT/scripts/send-file.sh" "$T/README.md" || fail "send README"

SUMMARY=$(head -c 1800 <<EOF
[$LABEL] LME Shell native source — commit $COMMIT ($DATE)
Coverage (NATIVE_PLAN rows): done=$DONE partial=$PART missing=$MISS external=$EXT
Build: cargo build --release --locked (workspace, offline-check via --locked)
Top limitations: see README "Known limitations" (real-machine validation list).
Split parts: reassemble with: cat lme-shell-src-*.zip.part* > lme-shell-src.zip
EOF
)
curl -s -o /dev/null -w '%{http_code}' -X POST \
  -H 'Content-Type: application/json' \
  -d "$(python3 -c 'import json,sys; print(json.dumps({"content": sys.argv[1]}))' "$SUMMARY")" \
  "$DISCORD_WEBHOOK_URL" | grep -q '^20' || fail "send summary message"
echo "==> summary message sent"

echo "==> DELIVERY $LABEL COMPLETE (zip: $ZIP)"
# Show the summary in the agent transcript too:
echo "--- summary ---"; echo "$SUMMARY"
