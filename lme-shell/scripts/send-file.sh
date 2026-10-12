#!/usr/bin/env bash
# scripts/send-file.sh <file> "<note>"  Upload a file to Discord, splitting if needed.
set -euo pipefail
F="${1:?usage: send-file.sh <file> <note>}"
NOTE="$(printf '%s' "${2:-file}" | tr -cd '[:alnum:] ._:,/#()-' | cut -c1-200)"
PART="${CKPT_PART_BYTES:-7000000}"
: "${DISCORD_WEBHOOK_URL:?DISCORD_WEBHOOK_URL is not set}"
[ -f "$F" ] || { echo "no such file: $F" >&2; exit 2; }
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
if [ "$(wc -c < "$F")" -le "$PART" ]; then
  parts=("$F")
else
  split -b "$PART" -d -a 2 "$F" "$W/$(basename "$F").part"
  parts=("$W"/*.part*)
fi
i=0; total="${#parts[@]}"
for p in "${parts[@]}"; do
  i=$((i+1))
  code="$(curl -sS --retry 3 --retry-delay 2 -o /dev/null -w '%{http_code}' \
    --form-string "payload_json={\"content\":\"$NOTE (part $i/$total, $(basename "$p"))\"}" \
    -F "files[0]=@$p" "$DISCORD_WEBHOOK_URL")" || code=000
  case "$code" in
    200|204) ;;
    *) echo "UPLOAD FAILED: part $i/$total, HTTP $code" >&2; exit 4 ;;
  esac
  sleep 1
done
echo "sent $(basename "$F") ($total part(s))"
