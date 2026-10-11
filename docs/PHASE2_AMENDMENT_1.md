# PHASE2_AMENDMENT_1 — operator decisions on Phase 2 (accepted: Phase 2.0)

Saved verbatim from the owner's message (minus the session-setup block).
The original Phase 2 specification is saved as `docs/PHASE2_SPEC.md`.

## Status and operator decisions

Phase 2.0 is accepted. All rules of the Phase 2 specification stay in force
(scope, hard constraints, checkpoint protocol, reporting) unless amended below.
Decisions:
1. Stack = Option A: smithay-client-toolkit + tiny-skia + cosmic-text, exactly
   the versions your spike used (pin them). Clients are spawn-on-demand.
   Options B and C are not chosen. Mark ADR 0001 "Accepted" with the reasons.
2. Rendering: full redraw while the panel is visible; no damage tracking.
   Revisit only if profiling shows a problem; record the numbers if so.
3. Keyboard interactivity as found in niri's source: power menu =
   Overlay + Exclusive, launcher = Overlay + OnDemand. Mark "needs real-machine
   validation".

## Step C - Fix the checkpoint script (before anything else)

Your Step P test found that my scan was silently disabled. Lesson: a safety
check must fail closed, never swallow errors with `|| true`. Apply these edits
to scripts/checkpoint.sh, keeping everything you already fixed:
1. Scan: do not hide stderr and do not ignore real errors.
   rc=0
   HITS="$(git grep --untracked -I -l -E "$PAT")" || rc=$?
   if [ "$rc" -gt 1 ]; then echo "ABORT: secret scan failed (git grep exit $rc)" >&2; exit 5; fi
   (exit code 1 means "no match" and is fine)
2. Bundle: two modes. Default is lean, because the base repo is public and
   each full bundle was about 7 parts.
   if [ "${CKPT_FULL:-0}" = 1 ]; then
     MODE=full; git bundle create -q "$S/repo.bundle" --all
   else
     MODE=lean; BASE="$(git merge-base HEAD origin/main)"; echo "$BASE" > "$S/BASE.txt"
     git format-patch -q --binary -o "$S/patches" "$BASE..HEAD"
   fi
   Add "mode: $MODE" to MANIFEST.txt. Lean saves only the current branch.
3. RESTORE.txt gets two variants, chosen by "mode:" in MANIFEST.txt:
   FULL: git clone repo.bundle lme && cd lme && git checkout <branch>
   LEAN: git clone https://github.com/longme179/lme-fedora-niri-qs.git lme && cd lme
         git checkout -b <branch> "$(cat ../BASE.txt)" && git am ../patches/*.patch
   Both then: git apply ../wip.patch; if ../untracked.tar exists: tar -xf ../untracked.tar;
   read STATE.md first.
4. Cadence: lean at the end of every response; CKPT_FULL=1 at the end of every
   numbered step, before switching branches, and for the final checkpoint.
Re-run your tests for BOTH modes: (a) fake-token abort with exit 3, (b) upload,
(c) restore in a temp dir with `git log` matching. Also report what makes the
old full bundle large: `git count-objects -vH` and
`git rev-list --objects --all | git cat-file --batch-check='%(objectsize:disk) %(rest)' | sort -rn | head`.
Keep history linear on rewrite/* branches (rebase, never merge), because
format-patch skips merge commits.
Then create scripts/send-file.sh exactly as below and test it with a small file
and with CKPT_PART_BYTES=1000 to force a split.

## Step U - Sync with the updated upstream

The upstream repo changed after Phase 0.
1. `git fetch origin`. Write docs/UPSTREAM_SYNC.md: commits and `git diff --stat`
   since the base you used, and which changes touch the backend commands,
   settings schema, launcher, power menu, other QML panels or docs.
2. Rebase rewrite/phase-2 (it contains the Phase 0/1 work) onto origin/main.
   Never discard upstream changes. Resolve conflicts carefully. If the rebase is
   large or risky, stop and ask me. Record old -> new commit hashes in STATE.md;
   make sure every rewrite/* branch you keep points at a consistent history.
3. Re-run fmt, clippy -D warnings and tests on every crate, and the Phase 1
   parity/golden tests against the UPDATED Python backend. Report any diffs. Fix
   Phase 1 only when the fix is small and inside crates you own; otherwise list
   it as a blocker for me.
4. Update the Phase 0 inventory, REWRITE_PLAN.md, REBUILD_SPEC.md and STATE.md.
   Re-read the QML power menu and launcher before Step 2.1; if they changed,
   the spec must follow the new behavior.
5. Checkpoint with CKPT_FULL=1.

## Replication report (mandatory; this is the deliverable I care most about)

Build and send it: right after Step U (baseline, covering Phase 0 to 2.0, since
the new channel has nothing yet), after Step 2.1, after Step 2.2, and whenever
I ask. File name: lme-phase2-<step>-report.zip, containing:
1. SUMMARY.md: the process summary for a human, at most 150 lines. What you did
   in order; decisions and why; results with numbers (QML vs native RSS, cold
   start, build time, binary size); problems hit and how you fixed them; what is
   still unverified; open questions for me.
2. REPRODUCE.md: exact recipe from a clean machine to the same results. Base
   commit hash, toolchain and crate versions, system packages, commands,
   expected outputs, a justified tolerance for timing and memory numbers, and
   what cannot be reproduced in the cloud.
3. patches/: git format-patch --binary -q -o patches "$(git merge-base HEAD origin/main)..HEAD"
4. measurements/: raw outputs, the scripts used, each crate's Cargo.lock,
   `rustc -V`, `cargo -V`, and `cargo tree --depth 1` per crate.
5. docs/: ADRs, STATE.md, REBUILD_SPEC.md, PNG captures.
6. MANIFEST.txt with the sha256 of every file.
Before sending, two checks that are not optional:
- Secret scan of the staging directory:
  eval "$(grep '^PAT=' scripts/checkpoint.sh)"
  if git grep --no-index -I -q -E "$PAT" -- <staging-dir>; then echo ABORT; fi
- Replay: in a fresh temp dir, clone upstream, checkout the base commit, `git am`
  the patches, then follow REPRODUCE.md literally (build, tests, measurement
  scripts). Fix REPRODUCE.md until it works from that clean state. List anything
  that only works in your sandbox.
Send: scripts/send-file.sh <zip> "<step> report", then post the first 1800
characters of SUMMARY.md as a Discord text message, and show the same summary
in this chat. If the zip is larger than 7 MB it is split; say so and tell me how
to rejoin it (cat file.part* > file).

## Step 2.1 and Step 2.2

As in the Phase 2 specification, with the decisions above. Additional rules:
- Reuse your mock compositor and PNG capture for headless integration tests and
  golden screenshots; mark everything they cannot prove "needs real-machine
  validation".
- Match the QML versions' IPC behavior (toggle/close, one instance); read the
  QML first and do not add features.
- Report per client: stripped size, idle and shown RSS, cold start, CPU while
  shown, compared with the QML panel where measurable.
- Add each client's section to REBUILD_SPEC.md before closing the step.

## Done

Steps C, U, 2.1 and 2.2 are finished; every replication report was replayed
successfully in a clean directory and sent; a final CKPT_FULL=1 checkpoint was
uploaded and its restore tested. Do not start Phase 3, do not delete any QML,
and if the repo conflicts with this prompt, stop and ask me.

## scripts/send-file.sh (create exactly this, chmod +x, test per Step C)

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

---

## Agent note (2026-10-11, this session)

The original Phase 2 specification text was lost from the agent's context at
session handover (the continuation summary was truncated). `docs/PHASE2_SPEC.md`
is therefore NOT yet in the repo; the owner was asked to paste the spec again.
Until it is saved, Steps 2.1/2.2 are held (their scope says "as in the Phase 2
specification"). Steps C, U and the baseline replication report (all fully
specified in this amendment) proceed normally.
