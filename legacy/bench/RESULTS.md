# Phase 0 measured baseline

All numbers produced by `bench/measure.py` (stub-tool harness, see
`bench/stubs/`) on the development container: Debian 13, Python 3.12.15,
kernel container. The **structure** of these numbers (which subprocesses run,
how often, which fields dominate the snapshot) transfers to the target
laptop; absolute RSS/wall numbers also depend on the real machine's
`.desktop` population and toolchain — re-run `bench/measure.py` there for
gold numbers. QML-side costs are analysis only: Quickshell/Qt cannot run in
this container (see "needs real-machine validation").

## Python backend (`scripts/desktop_backend.py`)

| Measurement | Value |
|---|---|
| `--once` wall time, no desktop tools on PATH | 0.100–0.102 s |
| `--once` wall time, all stub tools (max churn) | 0.133–0.137 s |
| Steady-state RSS (interval 2.0, 11 s sample) | **29.0 MB** (jumps from 5.5 MB after first snapshot) |
| Python 3.12 + used stdlib imports baseline | 13.7 MB |
| Snapshot JSON line size (8 sandbox apps) | 4.9 KB total, 2.9 KB of it `apps` |
| Short-lived subprocesses per snapshot | **12** (first cycle; 10/cycle after power-profile provider caches) |
| Idle fork rate (interval 2.0) | **5 forks/s** (10 per cycle) |
| Persistent children | 1 (`niri msg --json event-stream`) |
| Per-cycle subprocess list | `niri msg --json` ×2, `nmcli` ×3, `wpctl` ×2, `busctl` BlueZ ×1, `brightnessctl` ×1, `powerprofilesctl get` ×1 (provider probe ×2 only on first cycle) |
| App rescan | every 60 s: `rglob('*.desktop')` + `configparser` parse of each file |

Snapshot line size scales with installed applications: ~365 B/app in this
fixture ⇒ a full Fedora install with ~300 entries emits a ~100–110 KB line
every 2 s, which the QML side then `JSON.parse`s and re-`stringify`s three
times for identity-stability comparisons (see §QML).

## Clipboard service (`scripts/clipboard.py`)

| Measurement | Value |
|---|---|
| Steady-state RSS | **17.6 MB** (mostly the sqlite3 module + interpreter) |
| Per clipboard event | one fresh `python3 clipboard.py --capture <kind>` process (≈13.7 MB transient RSS each, plus fork/exec of wl-paste) |
| Idle poll loop | 0.25 s selector timeout; serializes only on revision/pause/config changes |

## Combined backend-side baseline

- 2 long-lived Python processes: ~46.6 MB RSS.
- 5 short-lived forks/s at idle + 1 persistent niri child.
- Snapshot re-serialization every 2 s (~100 KB on a full Fedora install).

## QML-side expected costs (analysis; needs real-machine validation)

Ranked expected contributors to RAM/CPU and idle wakeups. These cannot be
measured in the container (no compositor); verify with `qs -c lme-shell`
under the real session plus `heaptrack`/`perf` when convenient.

1. **Qt Quick runtime itself** — one QML engine, scene graph thread, glyph
   atlas, Quickshell modules, and per-window render targets dominate RSS
   (public Quickshell-based shells commonly sit at 150–300 MB). This is the
   ~6× Noctalia-vs-Quickshell factor and is *not* addressed by Phase 1.
2. **Per-output duplication** — `Variants { model: Quickshell.screens }`
   builds a full `Bar` (3 `BarGroup`s, up to 15 `BarModule` `Loader`s, a
   1 Hz clock `Timer`, workspace `Repeater`, `ActivityButton`) per monitor.
   Single-panel laptop = 1×, but every module re-evaluates bindings against
   the same 2 Hz snapshot.
3. **Snapshot churn** — every 2 s: `JSON.parse` of the full line, then three
   `JSON.stringify` equality passes (`niri`, `apps`, `preferences`) for
   delegate-identity stability. On ~300 apps that is ~400 KB of
   parse+stringify work every 2 s → GC pressure and CPU spikes on the UI
   thread; also re-fires any bindings that touch those objects.
4. **Always-on timers/animations** — clock `Timer` 1 s per output;
   `CalendarWidget` 60 s `running: true` once instantiated;
   `FocusTimer` ticker 250 ms while active; per-notification expiry timers;
   `Behavior on expansion` 360 ms morph repaints; `SlideLoader` slide
   animations per open. Each timer wake can drag the scene graph out of idle.
5. **Image caches** — Launcher `Image` tiles for every `.desktop` icon,
   MPRIS artwork in ActivityCard, clipboard image previews (file URIs into
   `history.sqlite3` images dir), wallpaper previews in Settings. Qt's
   pixmap cache is unbounded by default.

## Known cloud-validation limits

Graphics rendering, backlight, radios (Wi-Fi/Bluetooth), suspend, charging
thresholds, and polkit prompts cannot be exercised in this container. Those
code paths are covered by golden-JSON parity tests with stub tools and fake
sysfs trees, and every one of them is marked
**needs real-machine validation** in the plan.

---

# Phase 1 results — Rust backend vs Python backend (same container, same day)

Produced by `bench/measure.py --backend {python,rust}` (release build,
`opt-level=z` + LTO). The Rust runs below use the **subprocess parity mode**
(no `NIRI_SOCKET`), i.e. the worst case; inside a real niri session the socket
path additionally removes the two `niri msg --json` state polls per cycle and
the persistent event-stream child.

| Measurement | Python | Rust | Delta |
|---|---|---|---|
| `--once` wall, no desktop tools on PATH | 0.100–0.103 s | 0.018 s | **5.5× faster** |
| `--once` wall, all stub tools (max churn) | 0.128–0.129 s | 0.065 s | **2× faster** |
| Snapshot line, no-tools / stub-tools fixtures | 4597 / 5181 B | 4596 / 5180 B | same shape |
| Short-lived subprocesses per snapshot | 12 | 12 | identical list (parity) |
| State-daemon steady-state RSS (11 s) | 29.5 MB | **6.0 MB** | −79% |
| Clipboard-daemon steady-state RSS | 17.5 MB | **4.1 MB** | −77% |
| Combined backend RSS | 47.0 MB | **10.1 MB** | −78% (target < 15 MB ✓) |
| Per clipboard event | fresh python3 (~13.7 MB transient) | `lme-backend --capture` one-shot | no interpreter startup |

Parity evidence for these numbers: 58 tests green (34 unit + 24 integration,
including golden `--once` snapshots in three modes, a 22-action replay with
identical `lastResult`/subprocess sequences/socket bytes, cross-writer
settings.json stress, two-way clipboard DB handoff, weather state-machine
script, charge pending→completed with a fake sysfs) — `scripts/check.sh --rust`.

Idle fork rate is unchanged in parity mode (5 forks/s) by design: M3's niri
socket path removes niri polls only when `NIRI_SOCKET` is present, and the
nmcli/wpctl/busctl probes intentionally stay subprocesses for exact behavioral
parity until the measured Phase 2 D-Bus migration. **needs real-machine
validation**: RSS/forks under a real compositor session with `NIRI_SOCKET`
set, wl-paste watchers, and one full `LME_SHELL_BACKEND=rust` session.
