# PERF — measured performance of the native lme-shell

All numbers below are **cloud sandbox measurements** (containerized Linux,
shared vCPU; no GPU). They prove the software architecture's cost structure,
not laptop behavior. Real-machine validation: see README "Validation
checklist". Method: `scripts/bench-native.sh` (repeatable), release build
(`cargo build --release -p lme-shell -p mock-compositor`), the daemon running
against the headless mock compositor at 1280×800 with one output, bar only.

## Idle (bar visible, nothing changing) — release build

| Metric | Result | Target | Note |
|---|---|---|---|
| CPU over 60 s | **0–2 ticks** (HZ=100; latest run 0) | zero CPU ticks | the 30 s battery-sysfs probe and the minute-boundary clock rebuild each cost ≤ 1 tick; between those events the process is fully asleep |
| Wakeups over 60 s | **9** (0.15/s avg, all voluntary) | ≤ 1/s | calloop edge/level sources: wayland fd, ipc listener, inotify, chained one-shot timers |
| Renders in steady state | 0 per second; 1 per rebuild event | — | battery probe every 30 s, clock at minute boundaries (animations disabled in this config) |
| Daemon idle RSS | **11.6–11.9 MB** | ≤ 40 MB (+15/output) | includes FontBook (vendored Roboto + glyph caches), widget trees, shm pool |
| Cold start (client bind → first bar frame) | **9–12 ms** | fast | includes layer-shell round-trips and first render |

Debug-build numbers for comparison: idle 4 ticks/60 s, RSS 19.0 MB,
cold start 47 ms, render p99 72 ms — the per-pixel CPU rasterizer needs the
release profile (as expected).

## Animation (panel reveal, full 1280×800 overlay redraw) — release build

**Step 0b (vsync-grid pacing):** the mock compositor's animate mode now
paces frame callbacks on a vsync grid — every callback is due at the NEXT
grid tick after the commit (the scanout clock, not commit time) — and
counts late (> half-interval) and dropped (commit while a callback was
still unfired) frames. Reveal of the power menu (450 ms eased tween,
frame-callback driven), fps measured inside the reveal window from the
mock's per-callback FRAME log lines:

| Pacing | Frames / window | Delivered fps | Late | Dropped |
|---|---|---|---|---|
| 60 Hz grid | 28 / 450 ms | **60.0** | **0** | **0** |
| 120 Hz grid | 55 / 450 ms | **120.0** | **0** | **0** |
| unthrottled (ceiling) | 117–129 / ~450 ms | ~259–283 | — | — |

Render time during the reveal (LME_SHELL_PERF, ~120 samples):

| Metric | Result |
|---|---|
| Render time median | **2.1–2.2 ms** |
| Render time p95 | **3.1 ms** |
| Render time p99 | **6.6 ms** (first-frames warm-up: buffer alloc + shadow cache) |
| Render time max | 12.8 ms (very first frame) |
| Renders after reveal completed | **0** (verified over a 3 s window) |

Frame budget is 8.3 ms at 120 Hz: every steady render is ≥ 2.5× under
budget, p99 included. The client sustains a full 120 fps reveal with zero
late and zero dropped frames against the paced mock — the earlier
"115+ fps could not be honestly measured" caveat is resolved at the mock
level. What this still does **not** prove: real scanout on actual display
hardware (compositor scheduling, GPU page flips, display refresh). The
mock's grid is a 1 ms-tick software clock; real niri numbers remain a
real-machine checklist item.

## Memory notes

- One bar surface: 1280×42×4 B ≈ 215 KiB per frame buffer (SlotPool reuses
  slots; no growth across rebuilds was observed in the runs).
- Panel surfaces (launcher/power) are full-output overlays (1280×800 ≈
  4 MiB per buffer) and are **destroyed on close**, returning their pools.
- Font glyph caches are bounded (shaped-line cache hard-resets at 512
  entries; SwashCache is shared per process).

## Legacy comparison (same machine, QML shell, from bench/RESULTS.md)

The legacy Quickshell shell idled at ~120–190 MB RSS with ~10 subprocesses
per 2 s poll cycle. The native daemon: 11.6 MB, zero subprocesses at idle
(the battery read is a direct sysfs file read; workspaces/title come from
the niri event stream; network/audio probes are NOT wired in this build —
their modules show placeholders, so the comparison is bar-only vs bar-only).

## Periodic polls register (Amendment 4 lesson c)

Every recurring poll in the daemon, with its period and visibility rule:

| Poll | Period | Runs while | Owner |
|---|---|---|---|
| Clock rebuild | minute boundary | always (bar clock) | calloop timer |
| Battery/sysfs system probe | 30 s | always (bar pill) | calloop timer |
| MPRIS reconcile tick (D25) | 1 s while no player, 5 s while one is live | media module enabled | mpris feed thread |
| Notification expiry timers | per-toast timeout | only while toasts exist | calloop timers |
| Settings/palette mtime poll (Amendment 5, hygiene 4) | 5 s | ONLY when inotify is unavailable (EMFILE) or settings.json `hotReload: "poll"` — the inotify path wakes zero | calloop timer |
| BlueZ adapter probe (Tier 2.3) | with each control-center refresh (5 s while open) | ONLY while the control center is open — one busctl subprocess; zero at idle | system chain |

Idle wakeups with the 1 s empty-tick: ≤ 1.15/s worst case (one ListNames
call per second while waiting for a player) — within the ≤ 1/s class
target, and 0 additional wakeups while any player is live beyond the 5 s
health check. Re-measured at the Tier-2 tier end.
