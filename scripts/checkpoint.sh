#!/usr/bin/env bash
# scripts/checkpoint.sh "<one-line note>"
# Resumable checkpoint of this repo, uploaded to Discord. Needs git, curl, tar,
# and zip or python3. Reads $DISCORD_WEBHOOK_URL; never prints it.
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

# 1. Secret scan: file names only; abort before anything leaves the machine.
PAT='discord(app)?\.com/api/webhooks/[0-9]+/|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|-----BEGIN [A-Z ]*PRIVATE KEY-----|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9]{32,}'
HITS="$(git grep --untracked -I -l -E "$PAT" 2>/dev/null || true)"
if [ -z "$HITS" ]; then
  C="$(git log --all -p 2>/dev/null | grep -I -c -E "$PAT" || true)"
  if [ "${C:-0}" -gt 0 ]; then HITS="(git history)"; fi
fi
if [ -n "$HITS" ]; then
  echo "ABORT: secret-like string in: $HITS" >&2; exit 3
fi

# 2. Committed work (all branches), uncommitted diff, untracked files, state docs.
git bundle create -q "$S/repo.bundle" --all
git diff HEAD --binary > "$S/wip.patch" || true
git ls-files -o --exclude-standard -z | tar --null -T - -cf "$S/untracked.tar" 2>/dev/null || true
for f in STATE.md docs/REBUILD_SPEC.md; do
  if [ -f "$f" ]; then cp "$f" "$S/$(basename "$f")"; fi
done
{
  echo "id: $ID"; echo "time: $(date -u +%FT%TZ)"; echo "note: $NOTE"
  echo "branch: $(git rev-parse --abbrev-ref HEAD)"; echo "head: $(git rev-parse HEAD)"
  echo "--- status"; git status --short
  echo "--- log"; git log --oneline -n 15
} > "$S/MANIFEST.txt"
cat > "$S/RESTORE.txt" <<'EOF'
1. If the zip arrived in parts:  cat ckpt-NNN.zip.part* > ckpt-NNN.zip
2. unzip ckpt-NNN.zip && cd ckpt-NNN
3. git clone repo.bundle lme && cd lme
4. git checkout <branch named in ../MANIFEST.txt>   (other branches: git branch -a)
5. git apply ../wip.patch ; if ../untracked.tar exists: tar -xf ../untracked.tar
6. Read STATE.md first, then docs/REBUILD_SPEC.md, then continue.
EOF

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
echo "checkpoint $ID uploaded ($total part(s))"
