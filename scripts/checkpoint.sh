#!/usr/bin/env bash
# scripts/checkpoint.sh "<one-line note>"
# Resumable checkpoint of this repo, uploaded to Discord. Needs git, curl, tar,
# and zip or python3. Reads $DISCORD_WEBHOOK_URL; never prints it.
#
# Modes (PHASE2_AMENDMENT_1, Step C):
#   lean (default)    patches since merge-base(origin/main) + BASE.txt; the
#                     base repo is public, so only the current branch's work
#                     is saved. Requires a linear history on rewrite/* (rebase,
#                     never merge) because format-patch skips merge commits.
#   full (CKPT_FULL=1)  git bundle --all (every branch, ~7 upload parts).
# Cadence: lean at the end of every response; CKPT_FULL=1 at the end of every
# numbered step, before switching branches, and for the final checkpoint.
set -euo pipefail

NOTE="$(printf '%s' "${1:-checkpoint}" | tr -cd '[:alnum:] ._:,/#()-' | cut -c1-200)"
cd "$(git rev-parse --show-toplevel)"
STORE="${CKPT_STORE:-$HOME/.lme-checkpoints}"
PART="${CKPT_PART_BYTES:-7000000}"   # webhook upload limit is about 8 MB
mkdir -p "$STORE"
N=$(( $(cat "$STORE/counter" 2>/dev/null || echo 0) + 1 ))
ID="$(printf 'ckpt-%03d' "$N")"
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
S="$W/$ID"; mkdir -p "$S"

# 1. Secret scan — FAIL CLOSED: never hide stderr, never ignore real errors.
#    (git grep exit 0 = match found, 1 = no match (fine), >1 = error.)
PAT='discord(app)?\.com/api/webhooks/[0-9]+/|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|-----BEGIN [A-Z ]*PRIVATE KEY-----|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9]{32,}'
rc=0
HITS="$(git grep --untracked -I -l -E "$PAT")" || rc=$?
if [ "$rc" -gt 1 ]; then echo "ABORT: secret scan failed (git grep exit $rc)" >&2; exit 5; fi
if [ -z "$HITS" ]; then
  rc=0
  C="$(git log --all -p | grep -I -c -E "$PAT")" || rc=$?
  if [ "$rc" -gt 1 ]; then echo "ABORT: secret scan failed (history grep exit $rc)" >&2; exit 5; fi
  if [ "${C:-0}" -gt 0 ]; then HITS="(git history)"; fi
fi
if [ -n "$HITS" ]; then
  echo "ABORT: secret-like string in: $HITS" >&2; exit 3
fi

# 2. Committed work (mode-dependent), uncommitted diff, untracked files, docs.
if [ "${CKPT_FULL:-0}" = 1 ]; then
  MODE=full
  git bundle create -q "$S/repo.bundle" --all
else
  MODE=lean
  BASE="$(git merge-base HEAD origin/main)"; echo "$BASE" > "$S/BASE.txt"
  git format-patch -q --binary -o "$S/patches" "$BASE..HEAD"
fi
git diff HEAD --binary > "$S/wip.patch"
git ls-files -o --exclude-standard -z | tar --null -T - -cf "$S/untracked.tar"
for f in STATE.md docs/REBUILD_SPEC.md; do
  if [ -f "$f" ]; then cp "$f" "$S/$(basename "$f")"; fi
done
{
  echo "id: $ID"; echo "time: $(date -u +%FT%TZ)"; echo "note: $NOTE"
  echo "mode: $MODE"
  echo "branch: $(git rev-parse --abbrev-ref HEAD)"; echo "head: $(git rev-parse HEAD)"
  echo "--- status"; git status --short
  echo "--- log"; git log --oneline -n 15
} > "$S/MANIFEST.txt"

if [ "$MODE" = full ]; then
  cat > "$S/RESTORE.txt" <<'EOF'
Mode: full (repo.bundle carries all branches)
1. If the zip arrived in parts:  cat ckpt-NNN.zip.part* > ckpt-NNN.zip
2. unzip ckpt-NNN.zip && cd ckpt-NNN
3. git clone repo.bundle lme && cd lme
4. git checkout <branch named in ../MANIFEST.txt>   (other branches: git branch -a)
5. git apply ../wip.patch ; if ../untracked.tar exists: tar -xf ../untracked.tar
6. Read STATE.md first, then docs/REBUILD_SPEC.md, then continue.
EOF
else
  cat > "$S/RESTORE.txt" <<'EOF'
Mode: lean (patches on top of the public upstream base recorded in BASE.txt)
1. If the zip arrived in parts:  cat ckpt-NNN.zip.part* > ckpt-NNN.zip
2. unzip ckpt-NNN.zip && cd ckpt-NNN
3. git clone https://github.com/longme179/lme-fedora-niri-qs.git lme && cd lme
4. git checkout -b <branch named in ../MANIFEST.txt> "$(cat ../BASE.txt)"
5. git am ../patches/*.patch
6. git apply ../wip.patch ; if ../untracked.tar exists: tar -xf ../untracked.tar
7. Read STATE.md first, then docs/REBUILD_SPEC.md, then continue.
EOF
fi

# 3. Zip (python fallback if zip is missing); split below the upload limit.
Z="$STORE/$ID.zip"
( cd "$W" && { if command -v zip >/dev/null; then zip -qr "$Z" "$ID"; else python3 -m zipfile -c "$Z" "$ID"; fi } )
echo "$N" > "$STORE/counter"
if [ "$(wc -c < "$Z")" -le "$PART" ]; then
  parts=("$Z")
else
  split -b "$PART" -d -a 2 "$Z" "$Z.part"
  parts=("$Z".part*)
fi

# 4. Upload one part per request; stop loudly on failure and keep the local copy.
: "${DISCORD_WEBHOOK_URL:?DISCORD_WEBHOOK_URL is not set}"
i=0; total="${#parts[@]}"
for p in "${parts[@]}"; do
  i=$((i+1))
  msg="[$ID] $NOTE (part $i/$total, $(basename "$p"), $(du -h "$p" | cut -f1))"
  code="$(curl -sS --retry 3 --retry-delay 2 -o /dev/null -w '%{http_code}' \
    --form-string "payload_json={\"content\":\"$msg\"}" \
    -F "files[0]=@$p" "$DISCORD_WEBHOOK_URL")" || code=000
  case "$code" in
    200|204) ;;
    *) echo "UPLOAD FAILED: part $i/$total, HTTP $code. Local copy kept in $STORE" >&2; exit 4 ;;
  esac
  sleep 1
done

# 5. Keep only the newest 5 checkpoints locally.
for f in "$STORE"/ckpt-*; do
  [ -e "$f" ] || continue
  n="${f##*/ckpt-}"; n="${n%%.*}"
  if [ $((10#$n)) -le $((N-5)) ]; then rm -f "$f"; fi
done
echo "checkpoint $ID uploaded ($MODE mode, $total part(s))"
