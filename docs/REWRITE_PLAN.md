# LME Shell native rewrite plan

Phase 0 deliverable. Scope of this document: the whole rewrite; **execution
scope of this branch series is Phase 0 + Phase 1 only** (backend daemon).
Later phases (layer-shell clients, UI toolkit replacement) are listed for
context and require explicit go-ahead before any work starts.

Baseline numbers: `bench/RESULTS.md`. Frozen wire contract:
`docs/REWRITE_INVENTORY.md`. Upstream reference for expectations: Noctalia
v5 reportedly cut per-monitor memory ~6× by moving off Qt Quick to a native
C++/GLES client — most of that gain comes from dropping the QML/Qt runtime,
**not** from the implementation language. Consequences for this project:

- Phase 1 (Rust backend) targets: fewer forks (D-Bus/niri IPC instead of
  CLI subprocesses), lower backend RSS (29.0 + 17.6 MB → target < 15 MB
  combined), no per-event python spawn for clipboard capture, and a
  cleaner substrate for later phases. It will **not** by itself change the
  Quickshell process's memory.
- The big RAM win only arrives in a later phase that replaces the QML UI
  process; the backend work is deliberately sequenced first so that phase
  inherits a stable, tested, native state daemon and no Python dependency.

Principles (unchanged throughout):

1. QML keeps working at every commit; the JSON-lines contract is frozen
   (inventory §7). Deviations get documented + versioned *before* QML changes.
2. Python backend stays the default until parity is proven; the Rust daemon
   is selected with `LME_SHELL_BACKEND=rust` (v1 switch, see M6).
3. `cargo fmt`, `cargo clippy -D warnings`, `cargo test` clean at each step;
   every new crate justified in one line in `lme-backend/README.md`.
4. Shell stays unprivileged; no `~/.config/niri/config.kdl` changes; no
   silent repositories; no borrowed code without a license check
   (project is GPL-3.0-or-later; GPL-compatible deps only).
5. Graphics, backlight, radios, suspend, charging, polkit behavior are
   **needs real-machine validation** items — cloud work covers them with
   stub/golden tests only.

---

## Phase 0 — Baseline (done on `rewrite/phase-0`)

| Item | Status |
|---|---|
| Inventory of every command/state, callers, transport | done → `docs/REWRITE_INVENTORY.md` |
| Backend RSS/startup, subprocess counts, polling cadence | done → `bench/RESULTS.md`, harness in `bench/` |
| Top-5 expected QML costs | done → `bench/RESULTS.md` §QML |
| This plan | this file |

Key baseline facts driving the order below: 10 short-lived subprocesses per
2 s poll cycle (5 forks/s idle); 29.0 MB backend + 17.6 MB clipboard RSS;
a fresh python3 per clipboard event; niri queried by subprocess although
`NIRI_SOCKET` exists; apps re-serialized every 2 s.

---

## Phase 1 — Rust backend daemon (`lme-backend` crate)

One binary, three roles selected by flags (mirroring the Python layout):
default = state daemon on stdio; `--clipboard` = clipboard daemon;
`--capture <kind>` = one-shot clipboard capture (replaces the per-event
python spawn). Same flags otherwise (`--once`, `--demo`, `--settings`,
`--interval`, `--restore-wallpaper`).

Crate policy (each justified in `lme-backend/README.md`): `serde`/`serde_json`
(protocol), `niri-ipc` (typed niri IPC; GPL-3.0-compatible with this project),
`zbus` (system/session D-Bus for BlueZ, power-profiles, later more),
`rustix` (flock/fsync/etc. without libc boilerplate), `ureq` (smallest
blocking HTTPS client; matches the bounded request/response weather design,
no async runtime), `rusqlite` (bundled; same history.sqlite3 schema),
`sha2` (clipboard ids). No tokio, no clap (hand-rolled arg parsing), no
reqwest. `serde_json` with `preserve_order` for settings.json key order.

### M1 — Scaffold + CI hygiene

- `lme-backend/` crate, edition 2021, `cargo fmt`/`clippy -D warnings`/
  `cargo test` wired into `scripts/check.sh` behind a `--rust` flag that
  no-ops with a clear message when cargo is absent.
- Parity harness skeleton: `tests/parity.rs` runs the Python backend and
  the Rust binary in an identical fixture environment (stub `PATH`,
  fixture `HOME`/`XDG_*`, host `/proc`+`/sys` which are deterministic in
  containers) and compares snapshots as JSON trees after normalizing
  volatile fields (`system.cpu`, `system.memory*`, `weather.updatedAt`,
  `system.recording`).
- **Risk:** low. **Effort:** S. **Tests:** harness self-check (Python vs
  Python must be equal after normalization).

### M2 — Read-only state: `--once` snapshot (no weather fetch, no actions)

- Port: preferences load+validate, apps scan (desktop-entry localization,
  field codes, Hidden/NoDisplay/OnlyShowIn rules), niri state via the
  existing `niri msg --json` subprocesses (unchanged in this milestone),
  every `system` probe in inventory §2.2 (via the same CLI subprocesses,
  same parsing, same clamp/round semantics), demo snapshot, settings-only
  snapshot, `--once` flag, `--interval` argument validation.
- **Risk:** medium — many small parsers where byte-level quirks matter
  (nmcli escaping, busctl JSON envelope, wpctl/brightnessctl regexes,
  hwmon label rules, `/proc` math, desktop field codes).
- **Effort:** M-L. **Tests:** golden `--once` snapshots vs Python (live,
  settings, demo modes); unit tests per parser with fixtures copied from
  the Python suite's cases; `bench/measure.py --backend rust` must report
  identical per-snapshot subprocess counts.

### M3 — niri IPC over the unix socket

- Replace the two `niri msg --json` polls *and* the persistent
  `event-stream` subprocess with a direct `NIRI_SOCKET` connection using
  `niri-ipc` types: `Workspaces`/`Windows` requests on demand, plus one
  event-stream subscription that marks state dirty (keep the 100 ms
  debounce and the frozen `niri` snapshot shape; keep raw pass-through
  fields the QML reads: `id,idx,name,output,is_focused,is_active,title,
  app_id,workspace_id`).
- Subprocess fallback to `niri msg` when `NIRI_SOCKET` is unset/fails
  (current behavior, keeps the daemon usable from outside the session).
- Net effect at idle: −2 forks/2 s and one fewer persistent child.
- **Risk:** medium — event flood handling, socket reconnect on compositor
  restart, serializing `niri-ipc` types with the exact field names the QML
  expects (verified against `niri msg --json` output captured as goldens).
- **Effort:** M. **Tests:** mock niri IPC server (local `UnixListener`
  speaking the newline-JSON protocol) driving both request and event paths;
  parity of `niri` field against captured `niri msg --json` goldens;
  kill-the-socket reconnect test. **needs real-machine validation** for
  event behavior under the real compositor.

### M4 — settings.json merge with file lock

- Port `Preferences`: defaults, `validate_preference` (all rules incl.
  `barLayout` zone/module checks and `tasks`), stat-stamp reload, `.lock`
  flock, temp-file + fsync + rename atomic write with 0600/0700 modes,
  indent-2 + trailing-newline layout, insertion-ordered keys
  (`serde_json` `preserve_order`).
- **Risk:** medium-low — pure file logic, but cross-lock interop with the
  Python settings app and Python fallback must be flawless (both writers
  live during the transition).
- **Effort:** S-M. **Tests:** golden file bytes for a representative merge;
  concurrent-writer stress test (Rust writer vs Python writer loop, both
  under the same flock, assert no lost updates); every validation rule
  mirrored from the Python unit tests; mode-bit assertions.

### M5 — weather

- Port `WeatherClient`: single background worker thread, forecast/geo/search
  jobs with the exact TTLs (900/3600/120 s), stale/masking rules when the
  location key changes mid-flight, WMO code table, location validation,
  `search` state machine, 1 MiB cap, 7 s timeout, same User-Agent, same
  error strings. HTTP via `ureq` behind a small `Fetch` trait so tests can
  inject canned responses (mirrors the Python `opener` injection).
- **Risk:** medium — subtle state machine (masking, error precedence
  `geo_error if automatic and geo_error else error`).
- **Effort:** M. **Tests:** state-machine unit tests with injected opener
  (ported from the Python suite plus new cases); JSON parity of
  `weather` snapshot field-for-field vs Python under the same injected
  responses; live-network test behind `--ignored` (manual, not CI).

### M6 — clipboard history

- Port `clipboard.py` to `--clipboard` mode: same `history.sqlite3` schema
  and `meta` table (read/write compatible with the Python service — the
  DB is shared state on disk), trim algorithm (pins last, 64 MiB cap,
  limit 20–500), sha256 ids, image sniffing + `images/` dir, preview/search
  fields, `capture.lock` single-owner rule, `--capture` one-shot mode
  (no interpreter startup cost per event), preferences re-read every 1 s,
  0.25 s poll loop, same JSON-lines state and error strings.
- `wl-paste --watch` watchers now spawn the Rust binary's `--capture` mode.
- **Risk:** medium — DB compatibility both directions, blob handling.
- **Effort:** M. **Tests:** operation parity vs Python against a
  throwaway DB (same ops sequence ⇒ same rows, same JSON states);
  Python-service-then-Rust-service and Rust-then-Python DB handoff tests;
  trim/pin limit tests; oversized-input tests. **needs real-machine
  validation** for wl-paste watcher behavior under a real Wayland session.

### M7 — remaining actions + event loop + switch

- Port `perform()` handlers not covered above: volume/mute/mic, brightness,
  wifi toggle, bluetooth power (busctl set-property → bluetoothctl fallback
  → rfkill recovery → 3×120 ms confirm), bluetoothDevice connect/disconnect,
  powerProfile set (provider cache), lock/screenshot/logout/suspend/reboot/
  poweroff, wallpaper (awww daemon startup handshake), `launch` (terminal
  choice, `gio launch` fallback), `openSettings` spawns, `chargeMode`
  background job with the pkexec trusted-helper path check and
  kernel-confirmation semantics, demo-mode equivalents.
- Port the main loop: stdin JSON lines (64 KiB cap, ack-before-background-
  result ordering), 1.5 s system-cache window, 60 s app rescan, niri event
  debounce, `--restore-wallpaper`, restart-safe shutdown (SIGTERM/SIGINT,
  child termination).
- D-Bus migration note: keep CLI subprocess calls for BlueZ/power-profiles
  in M7 *only if* needed to ship; prefer `zbus` blocking connections with
  cached proxies so idle cost is zero forks (this is where most of the
  5 forks/s goes: nmcli×3, busctl×1, wpctl×2 are replaceable by zbus/MPRIS-
  style APIs; nmcli wifi list stays a subprocess — NetworkManager's D-Bus
  wifi scan API wakes the radio — but can move to a slower cadence).
- **Switch (flag):** `LME_SHELL_BACKEND=rust` env read in `ShellState.qml`
  and `ClipboardState.qml` chooses the Rust binary; default stays `python`.
  `scripts/build-backend.sh` builds `--release` into `lme-backend/target/`;
  `scripts/check.sh --rust` runs the full gate; README documents the flag.
- **Risk:** high — largest surface, ordering guarantees, polkit interplay.
- **Effort:** L. **Tests:** action parity suite driving both backends with
  the stub tools and comparing `lastResult` + stub-call sequences exactly;
  charge-mode async ordering test (pending → completed across snapshots);
  64 KiB/malformed-input tests; SIGTERM cleanup test. **needs real-machine
  validation:** charging writes, polkit prompt, swaylock, screenshots,
  suspend, backlight, radio toggles, awww interplay.

### Phase 1 exit criteria

- `cargo fmt --check` + `clippy -D warnings` + `cargo test` green; existing
  Python suite still green; parity suite green in all three modes.
- `bench/measure.py --backend rust` vs `--backend python`: identical
  snapshot shapes; RSS and fork-rate deltas recorded in `bench/RESULTS.md`.
- One session on the real laptop with `LME_SHELL_BACKEND=rust` documented
  (or explicitly listed as pending if hardware access is unavailable).
- Python fallback retained and default until that session is clean.

---

## Phase 2 — Quickshell-side reductions (context only, not started)

Ordered candidates after Phase 1, each independent of the others:

| Item | Idea | Risk |
|---|---|---|
| Snapshot delta protocol v2 | backend emits only changed sections or bumps a per-section revision; QML drops the 3× stringify comparisons | medium (contract change, versioned) |
| Icon/image cache bounds | explicit `cacheLimit`/`sourceSize` for Launcher tiles and MPRIS artwork | low |
| Timer consolidation | single 1 Hz driver for clock/calendar; stop CalendarWidget timer when panel closed | low |
| Per-output sharing | share one parsed snapshot across outputs (already singleton `ShellState`; verify no per-output copies of parsed data) | low |
| MPRIS/artwork lazy loading | load artwork only when Activity island expanded | low |

**needs real-machine validation** for all of the above (requires a running
Quickshell session + profiling).

## Phase 3 — Layer-shell clients / UI toolkit replacement (context only)

A native layer-shell bar (smithay-client-toolkit, one process per output or
one process with per-output surfaces) is the phase that reproduces the
Noctalia-class memory win by dropping Qt. It inherits: the M2–M7 daemon,
the frozen JSON protocol (or its v2), and the parity harness. Big open
questions deferred until then: text/font stack (cosmic-text), animation
approach, notification server, polkit agent, tray implementation, gesture
support. Explicit go-ahead required before any of this starts.

---

## Effort scale

S ≤ a focused day; M = a few days with tests; L = a week+ including parity
suites. All cloud-executable; real-machine items tracked in a checklist at
the end of each milestone section.
