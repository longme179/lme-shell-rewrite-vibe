# STATE.md — Phase 2 working state

For a fresh agent: read this top to bottom, then `docs/REBUILD_SPEC.md`,
then `docs/adr/0001-native-ui-stack.md`. No "as discussed before" —
everything needed is here or referenced.

## Current step

**Step 2.0 complete — STOPPED per stop condition, awaiting owner's stack
choice** (ADR 0001 recommendation: option A). Do NOT start Step 2.1 until
the owner answers the ADR's open questions in chat (Discord is one-way).

## Done

- Phase 0 (branch `rewrite/phase-0`, cf5c92e): inventory, benchmarks, plan.
- Phase 1 (branch `rewrite/phase-1`, d1848df, 6 commits): `lme-backend` Rust
  daemon, full golden parity, `LME_SHELL_BACKEND=rust` switch; Python
  fallback default. (Phase 0/1 reports delivered to Discord.)
- Phase 2 preflight (this session):
  - Repo canonical location: `~/lme-fedora-niri-qs` (moved from
    `/home/z/work/` per spec).
  - `scripts/checkpoint.sh` created; **fixed one bug in the provided script**
    (git grep `--untracked` must precede the pattern; stray `'` on the NOTE
    line was also corrected). Tests: (a) secret-scan abort → exit 3 ✓,
    (b) upload ✓ (ckpt-001, 7 parts — contains a harmless fake ghp_ token of
    36 'a's in untracked.tar from the failed first test-a run), (c)
    restore-from-parts in temp dir + git log match ✓.
  - Discord test message sent (HTTP 204).
- Step 2.0 spike work (branch `rewrite/phase-2`, commit: see git log):
  - `native/spike/mock-compositor`: headless Wayland compositor (wl_shm,
    wl_seat+keyboard with hand-written minimal xkb keymap, zwlr_layer_shell_v1
    v5, 120 Hz pacing, PNG capture). 3 unit tests.
  - `native/spike/spike-cpu` (A: tiny-skia + cosmic-text), `spike-gpu`
    (B: glutin+femtovg, build-only in cloud), `spike-slint` (C: Slint
    software renderer + custom pump loop). All: fmt/clippy -D warnings/test
    clean; keys scenario green (9 keys + Esc) for A and C; PNG evidence for
    A and C.
  - `native/spike/run-bench.sh` + `results/measurements.md` (RSS/CPU/startup/
    size/build-time for all three).
  - `docs/adr/0001-native-ui-stack.md` (recommendation A + open questions),
    `docs/REBUILD_SPEC.md` (coverage table + backend spec), `THIRD_PARTY.md`,
    `native/spike/README.md`, research notes in
    `native/spike/results/research-notes.md`.

## In progress

Nothing (stopped at the 2.0 gate). Uncommitted state: none — this file and
all spike work are committed before each checkpoint.

## Next actions (after owner's stack decision)

1. Record the decision in ADR 0001 (status → Accepted, note the choice and
   the damage-tracking answer).
2. Step 2.1 `lme-powermenu`: read `qml/panels/PowerMenu.qml` + settings
   look; implement per ADR stack; logind D-Bus power actions behind a trait
   with mock + `--dry-run`; lock = spawn `swaylock --color 22212c`,
   logout = `niri msg action quit --skip-confirmation` (unchanged semantics);
   Overlay + Exclusive keyboard; focused output only; Esc closes; settings
   switch/keybinding (default QML); write the full power-menu section into
   `docs/REBUILD_SPEC.md`; measure cold start vs daemon (ADR records the
   choice); `config/niri/lme-shell-native.kdl` fragment (never auto-applied).
3. Step 2.2 `lme-launcher` (only after 2.1 reported): extract `lme-common`
   (surface/theming/config); XDG scan + filtering + localized names + icons
   + Actions + search/ranking + keyboard nav; Exec parsing per Desktop Entry
   spec (field codes, quoting, Terminal, Path; never through a shell; child
   survives client exit); full launcher section in REBUILD_SPEC.
4. Integration: `scripts/install-native.sh` (opt-in, user-level),
   `docs/NATIVE_CLIENTS.md` (architecture, dry-run, one-command rollback,
   real-machine validation checklist).
5. Each step: fmt/clippy/test clean → commit → Discord report →
   `scripts/checkpoint.sh "note"`.

## Decisions

- ADR 0001 (Proposed): recommend sctk 0.21 + tiny-skia + cosmic-text; niri
  keyboard modes verified (menu=Overlay+Exclusive, launcher=Overlay+OnDemand
  with focus-on-map). Full rationale + rejected alternatives in the ADR.
- Power actions via systemd-logind D-Bus (Phase 2 kickoff spec) instead of
  `systemctl` CLI; lock/logout semantics unchanged.
- layer-shika rejected (AGPL, early-dev); iced_layershell not spiked
  (client_system build dep); Slint integration costs documented (pump loop,
  manual request_redraw, no font API, GPL-3.0-only election).
- Phase 1 decisions unchanged (see `docs/REWRITE_PLAN.md`).

## Measurements (container, release, mock compositor @120 Hz)

| | A cpu | B gpu | C slint |
|---|---|---|---|
| RSS idle/anim (MB) | 7.8/8.7 | cloud n/a | 7.4/8.6 |
| CPU% idle/anim | 0.0 / 9.3 (@120 Hz) | cloud n/a | 1.4 / 5.8 (@62 Hz) |
| cold start bind→map | 7–9 ms | cloud n/a | 2–3 ms |
| binary stripped | 4.6 MB | 2.8 MB | 9.0 MB |
| clean build | 106 s | 53 s | 149 s |

Raw: `native/spike/results/measurements.md`. All runtime numbers are
**needs real-machine validation**.

## Blockers and open questions for the owner

1. ADR 0001 open questions: stack choice (A/B/C) and, if A, damage-tracking
   from the start vs full-redraw-while-shown.
2. Note: ckpt-001 (uploaded before the scan fix) contains the harmless fake
   token file in its untracked.tar — listed for transparency.
3. `~/.bashrc` contains `DISCORD_WEBHOOK_URL` (appended end-of-file; a
   non-interactive `source` stops at the early-return — set it explicitly per
   command or extract via the sed one-liner used in this session, never
   printing it).

## Environment (installed tools, quirks)

- Rust via rustup at `~/.cargo/bin` (cargo/rustc 1.99); **new shells must
  `export PATH="$HOME/.cargo/bin:$PATH"`**.
- No sudo. No libEGL, no pkg-config dev files for xkbcommon/wayland →
  user-level shims at `/home/z/work/pkgconfig-fake/` (pc files + .so
  symlinks); builds need `PKG_CONFIG_PATH=/home/z/work/pkgconfig-fake/lib/pkgconfig`
  and `RUSTFLAGS="-L /home/z/work/pkgconfig-fake/lib"` (xkbcommon links
  against the real `libxkbcommon.so.0` present in the image).
- `/usr/share/X11/xkb` exists; the mock nonetheless uses the vendored
  hand-written keymap (`native/spike/assets/minimal-keymap.xkb`) for
  determinism. NOTE: wire keycode for Enter is 28 (not 36); Esc=1,
  BackSpace=14, arrows 111/113/114/116 − 8.
- Wayland runs headless via the mock compositor:
  `XDG_RUNTIME_DIR=/tmp/lme-mock-run`, `WAYLAND_DISPLAY=lme-mock-*`.
- Git identity: container default "Z User <z@container>" (exists; per spec
  only set if missing). Commits in Phase 0/1/2 use it.
- `df`: ~2.5 GB free; the five `target/` dirs total ~1.6 GB (gitignored).
- Checkpoints: `~/.lme-checkpoints/` (keeps newest 5); upload limit parts of
  7 MB; the full-repo bundle zips to ~45 MB → 7 parts per checkpoint
  (history includes the binary assets on main — expected).
- Phase 1 environment notes still apply for `lme-backend`
  (`docs/REWRITE_PLAN.md`, lme-backend/README.md).
