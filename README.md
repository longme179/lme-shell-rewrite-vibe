# lme-shell — native Wayland shell for Fedora 44 + niri 25.11+

The native Rust rewrite of [LME Shell](https://github.com/longme179/lme-fedora-niri-qs)
(QML/Quickshell UI + Python backend, GPL-3.0-or-later). One daemon owns the
bars, panels, OSD and services; a CPU renderer (no GPU, no Qt) draws
everything behind a `RenderBackend` seam. The legacy shell stays untouched in
[`legacy/`](legacy/) as the executable specification and runtime fallback.

**Status: Tier 0 complete, Tier 1 mostly complete (this is an interim
delivery, not a finished shell).** Built and verified headless against a mock
Wayland compositor in a cloud sandbox — everything marked *needs real-machine
validation* below was compiled and unit/integration-tested, but never ran
against real niri or real hardware. Do not replace your working QML shell
with this yet.

## What works (and how it was tested)

| Feature | Status | Tested by | Real-machine needed |
|---|---|---|---|
| Daemon + `lme-shell msg` IPC (toggle/close/refresh/status/osd/notify/dnd/notifications/tray/power) | done | e2e vs mock compositor + unit | no |
| Per-output bar: 3 zones from `barLayout`, connected pill, margins/height/font size | done | golden PNG (docs/screens/bar.png) | no |
| Bar modules: clock (minute-boundary), workspaces, window title, launcher/overview/notifications/settings/clipboard buttons, battery, network, audio, tray (live StatusNotifier items), media (live MPRIS players), resources (live CPU%/RAM% every 5 s) | done (data via sysfs, /proc, niri IPC, session-bus MPRIS; network/audio show real values only when the backend feeds them) | golden + e2e | niri stream, nmcli/wpctl feeds, real players |
| Pointer input: hover states, click routing, wheel semantics (value120 notches + touchpad pixel accumulation + axis_stop), outside-click panel dismiss, launcher hover/click-to-launch/wheel | done | unit + e2e via mock pointer injection | yes (real seat/touchpad feel) |
| Bar modules: media, resources, activity island | missing (no MPRIS/probe feed wired) | — | — |
| Launcher: apps mode with fuzzy ranking (exact port of `LauncherSearch.js`), favorites | done | unit + e2e (typed keys + Enter launch, pointer hover/click/wheel, Esc) | no |
| Launcher `=` calculator, `:` emoji (assets/emoji.json, copy on Enter), `@` windows | done | unit (calculator/emoji/mode-detect) | focus-window spawn needs niri |
| Power menu: Lock/Suspend/Log out/Restart/Shut down, confirmations for destructive actions, Esc closes, Overlay+Exclusive on the focused output | done | golden PNG + e2e key injection | yes (visual) |
| Power actions: logind D-Bus (Suspend/Reboot/PowerOff), `swaylock --color 22212c`, `niri msg action quit --skip-confirmation` | done, dry-run tested | unit (dry-run executor) | **yes — logind/swaylock/niri calls** |
| OSD popups + status toasts (auto-expiry) | done | e2e | no |
| settings.json + palette.json hot reload (inotify), upstream-added keys preserved on writes | done | unit (tmp dirs) | no |
| Idle cost: 0–2 CPU ticks / 60 s, 0.15 wakeups/s, 11.9 MB RSS, 9–12 ms cold start | done | bench (docs/PERF.md) | re-measure on laptop |
| Panel reveal animation: frame-callback driven, render p95 3.1 ms | done | bench (mock vsync-grid: 120.0 fps reveal, 0 late, 0 dropped) | fps at real scanout |
| Notification daemon: org.freedesktop.Notifications server, toast popups (top-right, ≤3), expiry rules, replace-by-id, DND + muted apps, bell unread badge, history (in-memory) | done (server + toasts e2e'd against a real private D-Bus) | unit + e2e (busctl client, dbus-monitor signal check) | yes (real session bus / real apps) |
| System tray: org.kde.StatusNotifierWatcher host, live item tracking (pixmaps, tooltips, status), com.canonical.dbusmenu menus (in-panel page), 268px right-anchored panel with hover/click/middle/wheel, `msg tray` | done (watcher + panel e2e'd against a real private D-Bus with a real SNI client, examples/fake_sni) | unit + e2e | yes (real session bus / real tray apps) |
| Notification CENTER panel, control center, clipboard UI, settings window, Wi-Fi/Bluetooth panels, night light, weather UI, activity island | **missing (Tier 2/3)** | — | — |
| Backend services (niri IPC, prefs with cross-writer locking, weather, clipboard store, app scan, charging) | done (frozen Phase 0 contract) | 50 golden parity tests vs the Python backend | no |

Full line-by-line status: [`docs/NATIVE_PLAN.md`](docs/NATIVE_PLAN.md).

### New behavior (no legacy-QML equivalent — deliberate additions)

These are additions, not ports; each is documented here per the rewrite
mission's honesty rule:

- **Wheel over the workspaces row** focuses the previous/next workspace
  (one notch = one workspace, high-resolution half-notches accumulate).
- **Wheel over the audio pill** previews volume ±5% per notch. Until the
  Tier-2 audio service exists this is **dry-run**: it only shows the
  "Volume (dry-run)" OSD and never touches the sink.
- **Click on the audio / battery pills** answers with a control-center
  notice and **click on the network pill** with a Wi-Fi panel notice —
  the real panels are Tier 2.
- **Click on the dimmed backdrop** dismisses the launcher / power menu
  (legacy closed panels only via Esc or their close buttons).

## Architecture

- [`docs/adr/0001-native-ui-stack.md`](docs/adr/0001-native-ui-stack.md) —
  rendering stack: smithay-client-toolkit 0.21 + tiny-skia + cosmic-text,
  full redraw while visible (validated by the Phase 2 spike).
- [`docs/adr/0002-daemon-architecture.md`](docs/adr/0002-daemon-architecture.md) —
  one daemon, lazy panel surfaces, one calloop loop; `lme-shell msg` over a
  unix socket; settings as a separate process (Tier 3, stubbed).
- `crates/lme-ui` — the toolkit: display-list `DrawCmd`s → `RenderBackend`
  (CPU implementation validated; a GPU backend is a documented seam,
  deliberately not written — untested GPU code is a liability).
- `crates/lme-backend` — the Phase 1 services library (linked in-process);
  its binary still serves the legacy QML shell via `LME_SHELL_BACKEND=rust`.

## Build (Fedora 44)

```bash
# dnf packages (same as the legacy shell, minus quickshell):
sudo dnf install gcc rustc cargo wayland-devel libxkbcommon-devel \
                 google-roboto-fonts swaylock niri
# (the shell also calls: niri msg, gio, wl-copy, wl-clipboard, awww — see below)

git clone https://github.com/longme179/lme-fedora-niri-qs.git
cd lme-fedora-niri-qs
cargo build --release          # one workspace, one target dir
# binary: target/release/lme-shell
```

Optional at runtime: `swaylock` (Lock), `wl-copy` (launcher copy actions),
`awww`/`awww-daemon` (wallpaper, Tier 2), `nm-connection-editor`,
`blueman-manager`, `pavucontrol` (external tools the full shell opens).
The shell never needs root; power actions go through logind.

## Run

```bash
# inside a niri session:
lme-shell daemon &             # bars appear on every output
lme-shell msg status           # daemon status JSON
lme-shell msg toggle launcher  # open/close the launcher
lme-shell msg toggle power     # power menu
lme-shell msg close            # close open panels
lme-shell msg osd Volume 40    # OSD popup
lme-shell msg tray             # registered tray items (JSON)
lme-shell msg media            # MPRIS players + the bar pill title (JSON)
lme-shell msg power suspend    # logind suspend (add --dry-run to the daemon for tests)
```

Merge [`config/niri/lme-shell-native.kdl`](config/niri/lme-shell-native.kdl)
into your niri config for the legacy key bindings (Mod+Space launcher,
Mod+Shift+E power, …). The shell never edits `~/.config/niri/config.kdl`.

Configuration files are unchanged from the legacy shell:
`~/.config/lme-shell/settings.json` (hot-reloaded; keys upstream added after
the Phase 0 freeze are preserved on writes) and
`~/.config/lme-shell/palette.json` (12 Dracula-style color roles).

## Measured performance (cloud sandbox — not laptop numbers)

See [`docs/PERF.md`](docs/PERF.md) for method and full tables. Release build,
1280×800, one output, bar only: **0–2 CPU ticks over 60 s** (the 30 s battery
sysfs read and the minute clock), **0.15 wakeups/s**, **11.9 MB RSS**,
**9–12 ms** bind→first-frame. Against the mock's vsync-grid pacing the
panel reveal delivers **60.0 fps at a 60 Hz grid and 120.0 fps at a 120 Hz
grid with zero late and zero dropped frames** (render median 2.1 ms, p99
6.6 ms; 8.3 ms budget at 120 Hz). The legacy QML shell idled at 120–190 MB
with ~10 subprocesses per 2 s poll.

## Real-machine validation checklist (do these first)

1. Bars on all outputs at login; exclusive zone correct with niri's own bar
   disabled; pointer cursor visible over bar and panels (cursor image is
   hand-drawn — no cursor-theme dependency).
2. `lme-shell msg toggle power` → menu opens on the **focused** output,
   confirmations appear, and **suspend/reboot/poweroff actually work through
   logind**; Lock spawns swaylock; Log out runs `niri msg action quit`.
3. Launcher: typing filters apps (fuzzy, favorites), `=2*21` shows 42,
   `:rainbow` finds 🌀, `@` lists open windows, Enter launches/copies/focuses.
4. Keyboard feel: Esc closes panels; arrow keys move launcher selection and
   power-menu focus; typing reaches the launcher instantly.
5. Idle: `grep -E 'utime|stime' /proc/$(pidof lme-shell)/stat` stays flat
   over a minute; `vmstat 1` shows no wakeups attributable to the shell.
6. Panel reveal smoothness at your display's refresh (frame-callback driven;
   the sandbox could only prove render times, not vsync pacing).
7. Pointer input: clicks on bar buttons, workspace pills, power-menu buttons
   (the sandbox compositor has no pointer — **this path is entirely
   untested**).

## Known limitations (honest list)

- Tier 2/3 features are missing: control center, clipboard UI, settings
  window, Wi-Fi/Bluetooth/night-light/weather UIs, activity island,
  workspace board, idle management. Toggling them via `msg` or the bar
  shows an OSD notice instead of pretending. (Notification daemon, tray
  and MPRIS media landed in Tier 1.)
- Fractional scaling (Tier 0.9) is implemented and golden-validated at
  1.25 against the mock compositor, with the scale-1.0 path kept
  byte-identical to the pre-fractional build. It has NOT been validated on
  a real compositor at a fractional scale yet (docs/KNOWN_ISSUES.md KI-2)
  — the first real-session HiDPI run should compare against the integer
  scale look before trusting it.
- The activity bar module has no data feed yet, so it stays hidden even
  when enabled in settings.
- If inotify is unavailable (EMFILE / NFS), the daemon survives with hot
  reload degraded to a 5 s mtime poll (or force it with settings.json
  `"hotReload": "poll"`).
- The launcher's `calculate` mode evaluates locally (no backend round-trip) —
  a deliberate deviation, logged in docs/DECISIONS.md D13.
- `lme-backend` implements the Phase 0 frozen Python contract; the upstream
  feature wave (wifi/night-light/capture/monitors/keybinds/displays/idle/
  notification history) is not ported (4 parity tests `#[ignore]`d, see
  docs/UPSTREAM_SYNC.md).
- No GPU rendering path (by decision: it could not be tested here). The CPU
  renderer's numbers above are the honest ceiling for this build.

## Licensing and attribution

GPL-3.0-or-later (see [LICENSE](LICENSE), [THIRD_PARTY.md](THIRD_PARTY.md)).
The engineering reference is [Noctalia](https://github.com/noctalia-dev/noctalia)
(MIT) — no code or assets copied. Motion curves derive from
DankMaterialShell (MIT, notice in `third_party/`). Vendored Roboto variable
font is OFL. The legacy QML shell remains in `legacy/` untouched.
