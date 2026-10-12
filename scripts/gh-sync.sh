#!/usr/bin/env bash
# scripts/gh-sync.sh [--selftest]  Push work to the backup repo. Never prints the token.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
URL="https://github.com/longme179/lme-shell-rewrite-vibe.git"
TOKEN="$HOME/.lme-gh-token"; ASK="$HOME/.lme-askpass.sh"
[ -s "$TOKEN" ] || { echo "gh-sync: $TOKEN missing" >&2; exit 6; }
if [ ! -x "$ASK" ]; then
  printf '%s\n' '#!/bin/sh' 'case "$1" in Username*) echo x-access-token ;; *) cat "$HOME/.lme-gh-token" ;; esac' > "$ASK"
  chmod 700 "$ASK"
fi
G() { GIT_ASKPASS="$ASK" GIT_TERMINAL_PROMPT=0 git "$@"; }

if [ "${1:-}" = "--selftest" ]; then
  REF="refs/heads/selftest/$(date -u +%Y%m%d%H%M%S)"
  EMPTY="$(git hash-object -t tree -w /dev/null)"
  C="$(git commit-tree "$EMPTY" -m selftest)"
  G push "$URL" "+$C:$REF"
  G push "$URL" ":$REF"
  echo "gh-sync selftest OK"; exit 0
fi

BR="$(git rev-parse --abbrev-ref HEAD)"
[ "$BR" != "HEAD" ] || { echo "gh-sync: detached HEAD" >&2; exit 2; }
eval "$(grep '^PAT=' scripts/checkpoint.sh)"

# Snapshot of the working tree (tracked and untracked, honoring .gitignore) on a side ref.
IDX="$(mktemp -u)"; LOG="$(mktemp)"; trap 'rm -f "$IDX" "$LOG"' EXIT
GIT_INDEX_FILE="$IDX" git read-tree HEAD
GIT_INDEX_FILE="$IDX" git add -A
TREE="$(GIT_INDEX_FILE="$IDX" git write-tree)"
SNAP="$(git commit-tree "$TREE" -p HEAD -m "wip snapshot of $BR $(date -u +%FT%TZ)")"

# Secret scan, fail closed: 0 = found, 1 = clean, anything else = error.
rc=0; git grep -I -l -E "$PAT" "$SNAP" >/dev/null || rc=$?
[ "$rc" -eq 1 ] || { echo "ABORT: secret scan on snapshot (rc=$rc)" >&2; exit 3; }
git log --all -p > "$LOG"
rc=0; grep -I -q -E "$PAT" "$LOG" || rc=$?
[ "$rc" -eq 1 ] || { echo "ABORT: secret scan on history (rc=$rc)" >&2; exit 3; }

G push "$URL" "refs/heads/rewrite/*:refs/heads/rewrite/*"
G push "$URL" "+$SNAP:refs/heads/wip/$BR"
echo "gh-sync: pushed rewrite/* and wip/$BR @ $(git rev-parse --short HEAD)"
