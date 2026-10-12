# STATE.md — native rewrite working state

For a fresh agent: read this top to bottom, then `docs/NATIVE_PLAN.md` (the
coverage table), then `docs/adr/0002-daemon-architecture.md`, then
`docs/DECISIONS.md`. Operative specs: the one-shot native-rewrite mission
(2026-10-11) + docs/MISSION_CONTINUATION.md (2026-10-12) +
docs/MISSION_AMENDMENT_2.md (GitHub backup, 2026-10-12). Earlier Phase 2
documents remain as history.

## Current step

**Session 2026-10-13 (Amendment 5): Tier 0.9 landed + hygiene 1-5 done.**
Read docs/MISSION_AMENDMENT_5.md (debugging cap, freeze rules),
docs/MISSION_AMENDMENT_4.md (work order), docs/RESUME.md (fresh-sandbox
bootstrap), docs/ZBUS_NOTES.md (before any D-Bus work), and
docs/KNOWN_ISSUES.md (live issue list). This session:

1. **Tier 0.9 fractional scale DONE** (D26): the previous session's WIP
   was already correct (mock reads via pread; fresh wl_buffer per frame —
   no live bug in the three mock-side hypotheses). Hardened to the
   Amendment 5 safety design: wp_viewport objects are created ONLY on a
   non-1.0 preferred_scale, so scale 1.0 is byte-identical to TIER-1
   (all 12 other goldens unchanged). e2e fractional_scale_golden at 1.25
   (1600x53 buffer, 1280x42 destination, 9.3 KB golden, VLM-inspected).
   The WIP's LME_E2E_SCALE process-global env var was a REAL race (any
   parallel golden could inherit 1.25 — caught 1-in-4, fixed as an
   explicit harness parameter, not retried).
2. **Hygiene 1 done**: failed-theory code REMOVED (dbus_util.rs deleted:
   no_cache_proxy + drive_executor_forever were D25 ladder steps 3-4,
   "stall identical" — prophylaxis in notifications/tray, no test ever
   needed them). ZBUS_NOTES rules 3-4 marked RETRACTED/hypothesis; the
   D25 "wakeup loss" mechanism marked hypothesis (never reproduced in a
   minimal probe); what is ESTABLISHED is the observable + the
   self-healing fix.
3. **Hygiene 2 done (test-count accounting)**: NO tests were removed —
   static #[test] counts went 141 (1df00cc) -> 143 (0595ea5, +2
   click_ids registry tests). The "137 -> 134" delta was a counting
   artifact (the Amendment-4 session's 134 summed a subset; ignored and
   legacy-reference-skipped tests were counted differently). Current
   exact number: **147 passed + 4 ignored** (cargo test --workspace).
4. **Hygiene 3 done (leaked test processes)**: 82 leaked dbus-daemons
   found and killed — the PrivateBus used `--fork`, so the tracked Child
   was the exited parent and the real bus daemon leaked per test. Now:
   foreground bus, every long-lived e2e child (daemon, mock, bus,
   monitor, fakes) in its OWN process group (process_group(0)) killed
   group-wide + reaped in Drop (kill_group), fakes hold ChildGuards
   (panic-safe), Harness Drop also removes its tmp dir (560 stale dirs
   at audit), and every harness/bus spawn calls
   assert_no_orphaned_test_procs() which fails fast listing orphaned
   (ppid 1) leftovers with kill instructions.
5. **Hygiene 4 done (EMFILE survival)**: inotify_init failure no longer
   kills the daemon — it logs once and degrades to a 5 s
   settings/palette mtime poll (MtimePoll, unit-tested for
   create/modify/delete; insert_mtime_poll repeating timer; PERF.md
   register updated). Selectable explicitly via settings.json
   "hotReload": "poll" (NFS homes). e2e
   settings_poll_hot_reload_survives_without_inotify: forced-poll daemon
   survives, serves IPC, and hot-reloads barFontSize 13 -> 26 through
   the poll (12 s capture golden, VLM-inspected).
6. **Hygiene 5 done**: scripts/e2e-x10.sh (resumable START END args)
   runs the golden suite N times; run at every tier end; a flake becomes
   a KNOWN_ISSUES entry or a fix (KI-1 records the one ~1/13 flake
   observed mid-session, with ruled-outs and the EMFILE hypothesis).

**Tier 2.1 control center DONE (D27)**: panel + Controls tab + tab grid +
SlidePanelRight + CONTROL_PANEL id range + e2e (open/golden/close) + bar
audio/battery pills open it. Exposed and fixed a LATENT BUG: probe_system
read LME_SHELL_FAKE_STATE_SYSTEM as inline JSON while the harness passes a
path — the fake system state never loaded and goldens had absorbed the
empty state (wheel OSD golden showed 5%, comment said 69%). Now file-read;
bar/osd/control goldens regenerated with real fake data. 152 passed + 4
ignored. KI-3 records the missing Flickable (content overflow on short
outputs).

**Tier 2.2 audio/brightness DONE**: ControlActions executor (wpctl
volume/mute + brightnessctl, same argv as the frozen backend actions,
dry-run logs the intent — the e2e asserts the log line), slider flushes
debounced 100 ms (legacy pendingVolume pattern, schedule_slider_flush),
optimistic snapshot updates keep bar+panel live, mute button wired.
e2e control_volume_slider_flushes_action (click at the VLM/unit-locked
slider coords -> dry-run line + OSD). 24 e2e green, clippy clean.

**Tier 2.3 BlueZ DONE**: probe_bluetooth rides the visibility-gated 5 s
refresh (busctl GetManagedObjects via the frozen backend probe; zero idle
cost — PERF register), TILE_BLUETOOTH toggles adapter power (set_bluetooth
argv, dry-run logged, optimistic flip), examples/fake_bluez on the private
bus doubles as the fake SYSTEM bus (DBUS_SYSTEM_BUS_ADDRESS; busctl honors
it — verified), e2e covers probe + toggle + the service-missing degrade.
Also fixed en route: control-center tabs/tiles now FILL their grid cells
(new lme-ui Fill wrapper — they were label-width before; found because the
e2e tile click missed) and the mock's scripted-event grace exit (3 s after
the LAST event) is documented in the e2e (late no-op motion keeps the run
alive). 156 passed + 4 ignored, 25 e2e green.

**Tier 2.4 (partial, TIER-2A scope) DONE**: wifi tile -> radio toggle
(nmcli read + set, backend wifiToggle shape, dry-run logged, optimistic
connected flip); e2e control_wifi_tile_toggles_radio. The WifiPanel
connection list (nm D-Bus) is the TIER-2 completion item, as planned.
157 passed + 4 ignored, 26 e2e green.

**Next: TIER-2A delivery (deliver.sh TIER-2A: gates + export + replay +
scan + zip + webhook), then Tier 2 completion (2.5 power profiles / idle
inhibit / night light, 2.6 weather, 2.7 agenda, 2.8 clipboard, 2.9
activity island, 2.10 workspace board, WifiPanel).**

Previous session (Amendment 4: robustness + registry + D25):

1. Mission docs saved (AMENDMENT_3 reconstructed, AMENDMENT_4, RESUME.md).
2. deliver.sh: 7 resumable stages + state markers, release-only replay in
   ONE dedicated target dir (deleted right after pass), >=3GB abort,
   --clean. Verified: timeout-kill mid-replay -> re-run resumes -> scanzip
   green. golden.rs spawns bins/examples from the CURRENT profile dir, so
   `cargo test --release --locked` works (137 green in release too).
3. click_ids.rs id registry (Amendment 4 lesson b): named non-overlapping
   ranges + unit tests (no-overlap, constants-in-range); daemon routing
   registry-bounded (power/launcher/bar/tray/toast/workspace).
4. **D25** (the session's big fight, full ladder in docs/DECISIONS.md):
   the MPRIS e2e flaked 40-60% — zbus 5.19 wakeup loss in the full-daemon
   context. mpris.rs rewritten single-threaded async (one block_on, custom
   Feeds stream) + self-healing reconcile tick (1s empty / 5s live) +
   3s per-call timeouts; dbus_util helpers (no_cache_proxy,
   drive_executor_forever) applied to notifications/tray; debug knobs
   (LME_SHELL_NO_NOTIF/TRAY, LME_MPRIS_DEBUG, examples/mpris_probe.rs).
   12/12 standalone e2e + 134 full suite green, fmt+clippy clean.
   Sandbox lesson: ~180 zombie test processes exhausted the 128-instance
   inotify budget -> daemon EMFILE deaths (kill stale test procs when
   e2e's fail fast with "inotify: Too many open files").

**Next: Tier 0.9 fractional-scale (time-boxed; see AMENDMENT_4 plan: bind
wp_fractional_scale_manager_v1 + wp_viewporter via wayland-protocols
staging, mock --scale flag, goldens 1.0/1.25 <=200KB), then Tier 2 in
order (control center shell + Controls tab first; TIER-2A delivery after
control center + audio + brightness + network + Bluetooth). Idle polls
register lives in docs/PERF.md; re-measure idle at every tier end.**

## Done (this session, branch rewrite/native)

**Session 2026-10-13 additions (tray):**

7. **Tier 1.11 system tray DONE** (commits 76c5f37, ebc3d4e):
   StatusNotifierWatcher host + SNI tracking + DBusMenu client + the
   268px right-anchored panel (port of TrayMenu.qml; D22 deltas:
   in-panel menu page, right-edge anchor). `msg tray` JSON. Unit tests
   (9 in tray.rs incl. the widget pointer pass) + e2e
   `tray_sni_lifecycle` on a private session bus with a REAL SNI client
   (`examples/fake_sni`, zbus interface server): register → bar button →
   panel → row Activate → right-click AboutToShow/GetLayout → menu page
   → entry Event → wheel Scroll → golden panel → kill → NameOwnerChanged
   removal + auto-close. e2e suite 18 → 19.
8. **Bugs found by the tray e2e and fixed:**
   - D23: handle_toast_click's open-ended `id >= 0x7000…` range swallowed
     the whole tray id space (every tray click died as a phantom
     notification dismiss) — bounded to [0x7000, 0x7100).
   - fetch_menu fed the (ia{sv}av) layout member to the whole-reply
     parser (fields.len() != 2) — every real menu came back
     "unparsable"; parse_menu_node is the correct entry point.
   - zbus-5 API drift in the WIP (Value::Variant → Value::Value, header
     arg by value, Message::header().member(), emit_signal body by ref,
     Structure::from variant-wraps Value fields — test trees must use
     StructureBuilder::append_field for the bare wire form).
   - a{sv} values arrive variant-wrapped: dict_map unwraps (else every
     dbusmenu prop parsed as empty).
   - pixmap decode: clamp channels to alpha instead of dropping icons
     with invalid non-premultiplied data.

**Session 2026-10-13 additions (media + resources → Tier 1 complete):**

9. **Tier 1.2 remainder DONE** (commit 8359776): mpris.rs (ListNames scan
   + NameOwnerChanged, per-player snapshots, PropertiesChanged refresh,
   gone-on-owner-loss; D20 thread pattern; non-fatal without a bus) →
   bar media pill (playing || first, trackTitle || identity) + `msg
   media`; system chain now merges /proc CPU%+RAM% (SystemProbes deltas)
   with the sysfs battery into the 5 s chained one-shot. e2e
   `mpris_media_module` with a REAL player (examples/fake_mpris, the
   legacy mpris_fixture.py port): track → golden bar pill → busctl Next →
   PropertiesChanged refresh → kill → removal. Suite 19 → 20 e2e.
10. **D24 found + fixed** (commit 8359776): zbus receive_all_signals
    filters on the proxy's interface — PropertiesChanged never reached
    the MPRIS listener (dead refresh path) nor the tray item listener
    (latent: only New* worked). Both now subscribe on a Properties proxy
    at the same dest/path. Caught by the busctl-driven Next call.
11. **Tier 1 is COMPLETE** (1.1–1.14). Remaining pre-Tier-2: 0.9
    fractional-scale.

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
`cargo test --workspace`: **152 passed, 0 failed, 4 ignored** (lme-backend
goldens ignored with upstream-drift reasons — docs/UPSTREAM_SYNC.md).
Tier 0.9 + hygiene gate: e2e x10 green (scripts/e2e-x10.sh).

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
**TIER-1-COMPLETE delivered 2026-10-12 (commit 1df00cc):** zip (1.0 MB,
1 part) + README + coverage summary sent via webhook after full replay
verification (release + debug + 137 tests green in the export tree,
secret scan clean). Completes Tier 1 (1.1–1.14): adds tray (1.11),
MPRIS media + resources feeds (1.2) over the TIER-1B set, with the D23
toast-id-range / fetch_menu parse / D24 receive_all_signals fixes the
new e2e suites exposed. Sandbox note: the deliver.sh single invocation
exceeds one 10-min command budget when the replay target is cold-ish —
completed manually mirroring the script steps exactly (gates cached,
export recreated, replay resumed, scan, zip, send); replay-target disk
accounting lesson recorded (release dir removable after its phase).


- Label TIER-1, commit 6169ddc, 2026-10-11. Zip 1 part (well under the 7 MB
  split threshold). NATIVE_PLAN counts at delivery: done=34 partial=1
  missing=17 external=2.
- Label **TIER-1B, commit ae0d04d, 2026-10-12.** Zip 1 part + README +
  summary + Amendment-2 addendum (backup repo URL + branch + hash) sent.
  Coverage at delivery: done=36 partial=1 missing=16 external=2. New since
  TIER-1: Step 0a pointer pass + D19 launcher fix, Step 0b vsync perf
  (60/120 fps reveal, 0 late/dropped), Tier 1.9 notification daemon, e2e
  suite 7 → 18 (incl. real private-bus D-Bus tests). Deliberate deviation
  from the continuation-prompt order (1.11 tray + 1.2 MPRIS were listed
  before TIER-1B): session budget favored locking in the verified feature
  set; tray + MPRIS are the next session's first items (recorded in
  DECISIONS D21). Replay: export build+test clean via the chunked-prewarm
  replay dir (LME_REPLAY_TARGET, sandbox lesson).
- Export excludes legacy/, docs/previews/, assets/wallpapers/ (EXPORT_NOTES
  inside the zip says so); docs/screens/ kept (3 PNGs).
- Backend parity tests that need the legacy reference tree skip cleanly in
  the export (require_legacy_reference!) — they run fully in the repo.

## Next actions (for the next session, in priority order)

1. Tier 1.2 remainder: MPRIS (media module), probes for resources module.
2. Tier 0.9: fractional-scale-v1 + viewporter (extend the mock with the
   globals to test headlessly).
3. Tier 2 in order: control center, audio/brightness actions (lme-backend
   probes exist), clipboard UI, weather UI, agenda, activity island.
4. Notification history persistence + notification center panel (Tier 2,
   needs the backend notificationSave catch-up — D20).
5. lme-backend catch-up port of the upstream feature wave (the 4 ignored
   goldens) — owner decision pending (Phase-1.5 scale).
6. TIER-2 delivery checkpoint when the control center lands (per the
   tier cadence: zip + README + summary via webhook).

## Blockers and open questions for the owner

1. Upstream parity catch-up port (STATE.md history + docs/UPSTREAM_SYNC.md):
   schedule as its own milestone or keep the frozen contract?
2. Tier-2 D-Bus surface (tray, notifications, MPRIS) is built on zbus 5
   blocking API bridged into calloop — validated only against mocks; the
   first real-session run should watch for re-entrancy surprises (D4).
3. Fractional scale (0.9) intentionally deferred — HiDPI users should stay
   on the QML shell until it lands.
