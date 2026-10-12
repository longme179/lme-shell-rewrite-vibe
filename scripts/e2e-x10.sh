#!/usr/bin/env bash
# Amendment 5 (hygiene 5): run every e2e test N times at each tier end.
# A flake becomes a KNOWN_ISSUES entry or a fix — never a retry-until-green.
#
# Usage:
#   scripts/e2e-x10.sh              # runs 1..10 (split into two commands if
#                                   # the sandbox 5-minute budget is tight:
#                                   # ~40 s per golden-suite run)
#   scripts/e2e-x10.sh START END    # e.g. `e2e-x10.sh 6 10` resumes
set -u
cd "$(dirname "$0")/.."
. scripts/env.sh

START="${1:-1}"
END="${2:-10}"
# the e2e harness spawns workspace binaries — build first (no-op when warm)
cargo build --workspace --bins --examples --quiet || exit 1
fails=0
runs=0
for i in $(seq "$START" "$END"); do
  runs=$((runs + 1))
  log="/tmp/e2e-x$i.log"
  if cargo test -p lme-shell --test golden > "$log" 2>&1; then
    echo "run $i: ok ($(grep -oE '[0-9]+ passed' "$log" | head -1))"
  else
    fails=$((fails + 1))
    echo "run $i: FAILED"
    grep -E "^test .* FAILED|^failures:" -A4 "$log" | head -14
  fi
done
echo "== e2e runs $START..$END done: $fails/$runs failed =="
# exit non-zero if anything failed so the tier gate cannot miss it
[ "$fails" -eq 0 ]
