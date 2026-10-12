# STATE.md — native rewrite working state

For a fresh agent: read this top to bottom, then `docs/NATIVE_PLAN.md` (the
coverage table), then `docs/adr/0002-daemon-architecture.md`, then
`docs/DECISIONS.md`. Operative specs: the one-shot native-rewrite mission
(2026-10-11) + docs/MISSION_CONTINUATION.md (2026-10-12) +
docs/MISSION_AMENDMENT_2.md (GitHub backup, 2026-10-12). Earlier Phase 2
documents remain as history.

## Current step

**Session 2026-10-12 (post-reset recovery + TIER-1B in progress).** Sandbox
survived between chats; branch was at 99bda82 with the Step-0a mock-pointer
edits half-applied (a multi-edit died mid-way: State/dispatch edits landed,
main() had not). Per Amendment 3: gh-sync ran FIRST (pushed 99bda82 +
wip snapshot), then main() was completed (pointer script parsing with ';'
separator, --capture-at, State init, tick(&dh), SUMMARY with delivered fps /
late / dropped / pointer_events, seat v8 for axis_value120) and committed.
Step-0a work items (Amendment 3 findings a–e) follow, then Step 0b (paced
fps/p99 report), then the continuation-prompt steps.

**Executing:** Step 0a finish (pointer e2e tests + findings a–e) → Step 0b
(vsync-paced fps/dropped/p99 report in docs/PERF.md) → Step 1 Tier 1 finish
(1.9 notifications daemon, 1.11 tray, 1.2 MPRIS feed, niri actions) →
TIER-1B delivery → Tier 2 in the stated order → TIER-2 → Tier 3 → TIER-3 →
Tier 4 → FINAL. Checkpoints lean; CKPT_FULL=1 only at FINAL. gh-sync runs
from checkpoint.sh after every checkpoint AND directly every ~10 min of work
(Amendment 3 cadence).

TIER-1 (previous session) was SENT: lme-shell-src-TIER-1-20261011.zip,
commit 6169ddc, replayed clean. Coverage at delivery: done=34 partial=1
missing=17 external=2 (docs/NATIVE_PLAN.md is the live table).

## Done (this session, branch rewrite/native)

1. Docs: ADR 0002, NATIVE_PLAN (full coverage table), DECISIONS D1–D17.
2. Restructure: legacy/ isolation (qml, python, lifecycle scripts, tests,
   bench), research/ spikes, crates/ workspace (lme-shell, lme-ui,
   lme-backend, mock-compositor), assets/fonts/Roboto-VF.ttf.
3. `lme-ui` toolkit: geom/color/draw display list, CPU renderer (tiny-skia),
   cosmic-text shaping + caches, Theme.qml port, 9-slice shadows, tween
   animator, widget set (boxes, Label, Icon, Button, Switch, Slider,
   TextInput, VirtualList, FocusScope), Wayland core (layer-shell windows,
   seat key/pointer, SlotPool buffers, ping-driven render OUTSIDE dispatch,
   with_shell re-entrancy, perf logging), hicolor PNG icon lookup.
4. `lme-shell` daemon: calloop loop (wayland, IPC socket, inotify
   settings/palette, minute clock chain, 30 s battery sysfs chain, niri
   event-stream bridge, OSD expiry timers), msg CLI, bar with ported module
   rows, power menu (confirmations, Esc), launcher (apps/=/:/@ modes, fuzzy
   ranking ported verbatim, emoji index, local calculator), OSD/status
   toasts, power executors (dry-run + logind/swaylock/niri).
5. Mock compositor extended: `--key-events name@ms` injection, exclusive
   keyboard focus tracking, per-axis set_size configure.
6. Verification: 7 e2e golden tests (bar/power/launcher PNGs with tolerance
   compare — deterministic; Esc-close via injected keys; launcher typing;
   OSD expiry; IPC surface), idle/anim benches (docs/PERF.md),
   docs/screens/ (3 PNGs ≤32 KB each).
7. Bugs found by tests and fixed: swash emoji format (D14), animation loop
   (D15), bar surface sizing (D16), python test-tree staging after the
   legacy/ move.

## Gate status (HEAD)

`cargo fmt` ✓ · `cargo clippy --workspace --all-targets -D warnings` ✓ ·
`cargo test --workspace`: **93 passed, 0 failed, 4 ignored** (lme-backend
goldens ignored with upstream-drift reasons — docs/UPSTREAM_SYNC.md).

## Measurements (release build, sandbox cloud CPU; docs/PERF.md)

Idle 60 s: 2 CPU ticks, 9 wakeups (0.15/s), RSS 11.6 MB, cold start
bind→map 10 ms. Reveal animation: 38 frames/460 ms, render median 2.2 ms,
p95 2.5 ms, p99 3.6 ms; zero renders after completion. 115+ fps claim NOT
made (mock pacing insufficient) — flagged real-machine.

## Environment (unchanged quirks + new)

- `. scripts/env.sh` for every shell: PATH, CARGO_TARGET_DIR=$HOME/target,
  CARGO_INCREMENTAL=0, pkg-config shims at /home/z/work/pkgconfig-fake
  (wayland/xkbcommon for sctk builds; real Fedora machines don't need them).
  The shim (handwritten .pc + .so symlinks onto the system .so.0 runtime
  libs) is NOT committed — see DECISIONS D18.
- **Sandbox lesson (Amendment 3): run cargo in the FOREGROUND.** Background
  jobs and children of the parent shell get killed in this sandbox, and the
  crates.io index update dies when backgrounded. Keep each command under
  ~5 minutes: `cargo fetch` first, then `cargo build -p <crate>` (workspace
  release build ~4.5 min — stay foreground, split if needed).
- Toolchain: `rustup component add clippy rustfmt` needed after a sandbox
  reset (shims live in ~/.cargo/bin, already on PATH via env.sh).
- Release build of the workspace: ~4.5 min; disk ~3.0 GB free after.
- Webhook: `export DISCORD_WEBHOOK_URL="$(cat ~/.lme-webhook)"` (chmod 600,
  outside every repo). Test message HTTP 204 confirmed this session.
- Backup sync: `scripts/gh-sync.sh` after every group of related edits and
  before every long build (takes seconds, nothing hits Discord — Amendment 3
  cadence). checkpoint.sh keeps its own cadence (it runs gh-sync too).
- Checkpoints: `scripts/checkpoint.sh "<note>"` lean default; CKPT_FULL=1
  final only. `scripts/send-file.sh` for deliveries. `scripts/deliver.sh
  <label>` runs gates + export + replay + scan + zip + send.
- Golden regen: `LME_GOLDEN_REGEN=1 cargo test -p lme-shell --test golden`.
- Bench: `scripts/bench-native.sh` (uses the RELEASE binaries).

## Delivery record

- Label TIER-1, commit 6169ddc, 2026-10-11. Zip 1 part (well under the 7 MB
  split threshold). NATIVE_PLAN counts at delivery: done=34 partial=1
  missing=17 external=2.
- Export excludes legacy/, docs/previews/, assets/wallpapers/ (EXPORT_NOTES
  inside the zip says so); docs/screens/ kept (3 PNGs).
- Backend parity tests that need the legacy reference tree skip cleanly in
  the export (require_legacy_reference!) — they run fully in the repo.

## Next actions (for the next session, in priority order)

1. Tier 1.9: notification daemon — zbus `org.freedesktop.Notifications`
   server (#[zbus::interface]) + toast surface + history + DND; wire the bar
   bell module. zbus 5.19 already builds in the workspace.
2. Tier 1.11: tray — StatusNotifierWatcher + DBusMenu (largest remaining
   Tier 1 item; real-machine validation required).
3. Tier 1.2 remainder: MPRIS (media module), probes for resources module.
4. Tier 0.9: fractional-scale-v1 + viewporter (extend the mock with the
   globals to test headlessly).
5. Tier 2 in order: control center, audio/brightness actions (lme-backend
   probes exist), clipboard UI, weather UI, agenda, activity island.
6. lme-backend catch-up port of the upstream feature wave (the 4 ignored
   goldens) — owner decision pending (Phase-1.5 scale).

## Blockers and open questions for the owner

1. Upstream parity catch-up port (STATE.md history + docs/UPSTREAM_SYNC.md):
   schedule as its own milestone or keep the frozen contract?
2. Tier-2 D-Bus surface (tray, notifications, MPRIS) is built on zbus 5
   blocking API bridged into calloop — validated only against mocks; the
   first real-session run should watch for re-entrancy surprises (D4).
3. Fractional scale (0.9) intentionally deferred — HiDPI users should stay
   on the QML shell until it lands.
