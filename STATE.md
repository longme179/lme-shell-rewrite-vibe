# STATE.md — Phase 2 working state

For a fresh agent: read this top to bottom, then `docs/REBUILD_SPEC.md`,
then `docs/adr/0001-native-ui-stack.md`, then `docs/PHASE2_AMENDMENT_1.md`
(the owner's operative instructions) and `docs/UPSTREAM_SYNC.md`. No "as
discussed before" — everything needed is here or referenced.

## Current step

**Steps C and U complete. Steps 2.1/2.2 HELD — waiting for the owner to
re-paste the original Phase 2 specification** (lost at session handover;
to be saved verbatim as `docs/PHASE2_SPEC.md`). The amendment defines
2.1/2.2 only "as in the Phase 2 specification", so do not start them from
reconstructed scope — the repo's own transcription (this file's "Next
actions" + REBUILD_SPEC §5/§6) is a fallback the owner has not yet
endorsed. Everything else in PHASE2_AMENDMENT_1 is done, including the
baseline replication report.

## Done (chronological)

- Phase 0 (`rewrite/phase-0`): inventory, benchmarks, plan — rebased.
- Phase 1 (`rewrite/phase-1`): `lme-backend` daemon, golden parity,
  `LME_SHELL_BACKEND=rust` switch — rebased.
- Phase 2 preflight: `scripts/checkpoint.sh` + tests; Discord webhook.
- Step 2.0 (spike): mock compositor, three candidate clients,
  measurements, ADR 0001, REBUILD_SPEC, THIRD_PARTY.
- **Step C (this session, PHASE2_AMENDMENT_1)**:
  - Webhook moved to `~/.lme-webhook` (chmod 600; `.bashrc` now sources
    it — the old URL is fully removed from the machine). Plain test
    message → HTTP 204. New `scripts/send-file.sh` (exact owner text),
    tested: 1-part send, 3-part split at `CKPT_PART_BYTES=1000`,
    missing-file → exit 2.
  - `checkpoint.sh`: fail-closed secret scan (git grep stderr visible,
    real errors → **exit 5**; worktree hit → exit 3; history-only hit →
    exit 3 — all three paths tested). **Lean mode default** (BASE.txt +
    `format-patch --binary` since merge-base origin/main; 589 KB vs
    47 MB full) and **full mode** (`CKPT_FULL=1`, bundle --all); `mode:`
    line in MANIFEST.txt; per-mode RESTORE.txt. wip.patch/untracked.tar
    capture no longer `|| true`; empty wip.patch is omitted (a clean tree
    used to break `git apply` in RESTORE — found by the restore test).
  - Both modes uploaded and restored in temp dirs: lean (git am 11
    patches → subjects + full tree content identical to original) and
    full (parts reassembled → sha256 equal, bundle clone → exact commit
    hashes). ckpt-003/004 lean, ckpt-005 full.
  - Bundle bloat explained: 30 blobs > 1 MiB = 43.6 MiB of the 44.57 MiB
    pack — successive versions of ~1.6 MiB PNGs in `docs/previews/` plus
    `assets/wallpapers/liquid-dracula.png` (1.7 MiB) committed on main.
  - Process lesson recorded: an uncommitted checkpoint.sh rewrite was
    wiped by `git reset --hard` during the history-scan test — commit
    before repo surgery (the checkpoint cadence exists for a reason).
- **Step U (this session)**: see `docs/UPSTREAM_SYNC.md` for the full
  report. Fetch → origin/main `ead4446`→`ef5281c` (5 commits, 107 files).
  Rebase of `rewrite/phase-2`: ONE conflict (THIRD_PARTY.md — both sides
  had rewritten it; merged keeping upstream's attribution document + our
  Phase 2 sections). `README.md`/`ShellState.qml` auto-merged with both
  sides intact. `main` fast-forwarded; `rewrite/phase-0`/`phase-1`
  repointed to rebased equivalents. Gates re-run (numbers below).

## Commit hash map (rebase, old → new)

cf5c92e→37c2256 (Phase 0) · 4a86c55→6306804 (M1+M2) · 5d3ada4→9a9b318 (M3)
· 0d38b3c→250f8cc (M4) · 1e6cbd5→abfd158 (M5) · f2e0753→02b1835 (M6)
· d1848df→ac294bc (M7) · a9fe198→b8e438e (2.0) · d326d1e→d310ed2 (amendment
doc) · c85755b→169409c (Step C) · 63da180→9fabeea (Step C fix). New Step U
commit on top (docs + ADR acceptance). History linear, no merges.

## Gate status after Step U (against UPDATED Python backend)

- `lme-backend`: fmt ✓, clippy `-D warnings` ✓, tests **50/54**.
- Spike crates: mock-compositor ✓ (3 tests), spike-cpu ✓ (2 tests),
  spike-gpu fmt/clippy/build ✓ (no EGL in cloud — by design), spike-slint
  fmt/clippy ✓ (0 unit tests by design; scenario+PNG evidence at 2.0).
- Python suite: 147 tests OK, 18 skipped (container skips).

## Upstream parity blocker (for the owner — not fixed, per amendment rule)

`desktop_backend.py` grew a feature wave (wifi, night light, capture/OCR,
monitors, keybinds, displays, idle, notification history, feature jobs).
`lme-backend` implements the Phase 0 frozen contract, so 4 golden tests
now fail — all upstream additions, not Rust regressions:

1–3. `parity.rs` `once_{live,demo,settings}_mode_matches_python`: Python
snapshot adds top-level `capture, displays, idle, keybinds, monitor,
nightLight, notificationHistory, platform, wallpaperLibrary`;
`system.battery.acOnline`; `system.network.{device,networks[+security,
secured,enterprise,bssid],saved[]}`; ~28 new preference keys.
4. `actions_parity.rs` action 15 `lock`: Python now returns
`{"ok":true,"message":"Working…","pending":true}` (async feature job);
Rust still acks "Lock requested" synchronously.

Catch-up port ≈ Phase 1.5 scale (new probes + feature-job machinery).
Owner decision needed: schedule it as its own milestone, or accept the
Rust backend as the frozen-contract implementation until then.

## In progress

Nothing uncommitted. Next: baseline replication report (Phase 0→2.0 +
C + U) — built, replayed in a clean dir, sent via send-file.sh.

## Next actions

1. Owner re-pastes the Phase 2 spec → save verbatim as
   `docs/PHASE2_SPEC.md`, commit, reconcile any divergence from
   REBUILD_SPEC §5/§6.1 + this file's transcription.
2. Step 2.1 `lme-powermenu` (PowerMenu.qml unchanged upstream, 133
   lines): stack A pinned versions; logind D-Bus power actions behind a
   trait with mock + `--dry-run`; lock = spawn `swaylock --color 22212c`;
   logout = `niri msg action quit --skip-confirmation`; Overlay +
   Exclusive; focused output only; Esc closes; full-redraw while shown;
   spawn-on-demand; settings switch/keybinding default stays QML;
   REBUILD_SPEC §5 written in full; measurements (stripped size, idle/
   shown RSS, cold start, CPU while shown vs QML where measurable);
   `config/niri/lme-shell-native.kdl` fragment (never auto-applied).
3. Step 2.2 `lme-launcher` — MUST follow the NEW upstream behavior
   (REBUILD_SPEC §6.1: multi-mode, fuzzy ranking, favorites), after 2.1
   is reported.
4. Each step: fmt/clippy/test → commit → replication report (replayed) →
   checkpoint (lean per response, CKPT_FULL=1 per numbered step).

## Decisions

- ADR 0001 **Accepted** (owner): stack A pinned (sctk 0.21.1, tiny-skia
  0.12.0, cosmic-text 0.19.0), spawn-on-demand, full redraw while shown,
  menu=Overlay+Exclusive, launcher=Overlay+OnDemand
  (needs-real-machine-validation on focus feel).
- Checkpoint cadence (amendment): lean end of every response; CKPT_FULL=1
  end of every numbered step, before switching branches, final.
- Lean checkpoints assume linear rewrite/* history (format-patch skips
  merges) — rebase, never merge.
- Phase 1 parity gap = blocker, not silently hacked (see above).

## Environment (installed tools, quirks)

- Rust via rustup at `~/.cargo/bin` (rustc/cargo 1.99); new shells must
  `export PATH="$HOME/.cargo/bin:$PATH"`.
- Webhook: `export DISCORD_WEBHOOK_URL="$(cat ~/.lme-webhook)"` (file is
  chmod 600, outside every repo; `.bashrc` sources it for interactive
  shells — non-interactive shells must set it explicitly).
- No sudo. No libEGL, no pkg-config dev files for xkbcommon/wayland →
  user-level shims at `/home/z/work/pkgconfig-fake/` (pc files + .so
  symlinks); spike builds need
  `PKG_CONFIG_PATH=/home/z/work/pkgconfig-fake/lib/pkgconfig` and
  `RUSTFLAGS="-L /home/z/work/pkgconfig-fake/lib"`. `lme-backend` needs
  neither.
- `/usr/share/X11/xkb` exists; the mock compositor uses the vendored
  keymap (`native/spike/assets/minimal-keymap.xkb`) for determinism
  (Enter wire keycode 28, Esc 1, BackSpace 14, arrows 111/113/114/116 −8).
- Wayland runs headless via the mock compositor:
  `XDG_RUNTIME_DIR=/tmp/lme-mock-run`, `WAYLAND_DISPLAY=lme-mock-*`.
- Git identity: "Z User <z@container>" (container default; only set if
  missing, per spec). Patch author in format-patch output: "LME Rewrite
  <rewrite@localhost>" (from Phase 0/1 commits) — committer identity
  differs, so lean-restored hashes differ from original; content and
  subjects are identical (verified), full-mode restores are hash-exact.
- **Disk is tight** (9.9 GB total): spike target/ dirs are ~0.3–1.7 GB
  each; `cargo clean` them when not in use (they are gitignored,
  rebuildable; spike-slint is the slowest rebuild ~10 min clean). Stale
  build/research dirs under /tmp were cleaned this session. Check
  `df -h /` before any big build; a full replay of all 5 crates needs
  ~3–4 GB free.
- Checkpoints: `~/.lme-checkpoints/` (keeps newest 5; counter file).
  Full zips ≈ 45–47 MB → 7 parts; lean ≈ 0.6–1 MB → 1 part.
- `git rebase --continue` here needs `GIT_EDITOR=true` (dumb terminal).

## Blockers and open questions for the owner

1. **Re-paste the Phase 2 specification** (lost at handover) → then 2.1
   and 2.2 proceed.
2. Upstream parity blocker (above): catch-up port decision.
3. ckpt-001 (pre-fix, superseded) contained a harmless fake ghp_ token of
   36 'a's in its untracked.tar — noted for transparency; ckpt-002..005
   are clean.
