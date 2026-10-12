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
| CPU over 60 s | **2 ticks** (HZ=100 → 0.02 s) | zero CPU ticks | the 2 ticks are the 30 s battery-sysfs probe and the minute-boundary clock rebuild (one bar re-render each); between those events the process is fully asleep |
| Wakeups over 60 s | **9** (0.15/s avg, all voluntary) | ≤ 1/s | calloop edge/level sources: wayland fd, ipc listener, inotify, chained one-shot timers |
| Renders in steady state | 0 per second; 1 per rebuild event | — | battery probe every 30 s, clock at minute boundaries (animations disabled in this config) |
| Daemon idle RSS | **11.6 MB** | ≤ 40 MB (+15/output) | includes FontBook (vendored Roboto + glyph caches), widget trees, shm pool |
| Cold start (client bind → first bar frame) | **10 ms** | fast | includes layer-shell round-trips and first render |

Debug-build numbers for comparison: idle 4 ticks/60 s, RSS 19.0 MB,
cold start 47 ms, render p99 72 ms — the per-pixel CPU rasterizer needs the
release profile (as expected).

## Animation (panel reveal, full 1280×800 overlay redraw) — release build

Reveal of the power menu (460 ms eased tween, frame-callback driven, mock
compositor pacing at `--fps 120`):

| Metric | Result |
|---|---|
| Frames rendered during reveal | 38 |
| Render time median | **2.2 ms** |
| Render time p95 | **2.5 ms** |
| Render time p99 | **3.6 ms** |
| Render time max | 9.3 ms (first frames, cache warm-up) |
| Renders after reveal completed | **0** (verified over a 3 s window) |

Frame budget at 120 Hz is 8.3 ms; every steady render is ≥ 2× under budget,
so the client is never the limiter. The observed 38 frames / 460 ms ≈ 83 fps
is the mock compositor's callback pacing, not client cost — **the 115+ fps
target could not be honestly measured against this mock and needs real
niri hardware** (mission rule: never claim untested performance).

Unthrottled (mock fires callbacks immediately) the same reveal renders at
~220 frames/s, i.e. the render loop ceiling on this vCPU is far above 120 Hz.

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
