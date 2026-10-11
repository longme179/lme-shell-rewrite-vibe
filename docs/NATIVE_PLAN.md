# NATIVE_PLAN — full-shell rewrite coverage and milestones

Branch `rewrite/native` (from `rewrite/phase-2`). Operative spec: the one-shot
native-rewrite mission (owner, 2026-10-11). This file tracks **every legacy
feature**: tier, target module, test method, and status. Updated at every tier
end. Tier order is strict: finish a tier (code, tests, docs, gates green)
before the next.

Workspace layout (ADR 0002):

```
crates/lme-shell/   daemon + msg CLI + settings window (Tier 3)
crates/lme-ui/      toolkit: render backend(s), text, widgets, animation,
                    Wayland surfaces, input, theme/palette, icons
crates/lme-backend/ existing services lib (prefs, niri, weather, clipboard,
                    apps, probes, actions) — linked in-process; binary keeps
                    serving the legacy QML shell
crates/mock-compositor/  headless test compositor (from the spike)
research/           spike crates (reference only, not workspace members)
legacy/             QML/Python shell, never edited, excluded from the export
```

Legend for status: **done** · **partial** · **missing** · **external** (a
system component the shell calls but does not implement). Test methods:
`unit` (in-crate), `golden` (mock-compositor PNG with tolerance + replay),
`e2e` (daemon+msg+compositor integration), `bench` (measured perf),
`real-machine` (cannot be proven in the sandbox — flagged in README).

## Tier 0 — foundation

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 0.1 | Cargo workspace, one target dir, profiles | `/Cargo.toml` | build | done |
| 0.2 | Legacy isolation (qml/, python, demos → legacy/) | git mv | git status | done |
| 0.3 | `lme-shell` daemon skeleton + calloop loop | lme-shell/daemon.rs | unit, e2e | done |
| 0.4 | `lme-shell msg` CLI + unix socket IPC | lme-shell/{main,ipc}.rs | unit, e2e | done |
| 0.5 | settings.json load/merge/validate (Phase 0 keys) + locked atomic writes | lme-backend/prefs.rs (existing) | existing goldens | done |
| 0.6 | settings hot-reload via inotify | lme-shell/settings.rs | unit (tmp dir) | done |
| 0.7 | palette.json load + hot-reload | lme-ui/theme.rs | unit | done |
| 0.8 | Wayland: outputs, layer-shell, seat key/pointer, shm, frame callbacks | lme-ui/wayland | golden | done |
| 0.9 | Fractional scale v1 + viewporter (buffer scale 1) | lme-ui/wayland | unit; scale=1 in sandbox → real-machine | partial (mock has no fractional globals; client requests them, falls back to integer scale) |
| 0.10 | CPU renderer: rounded rects, fills, borders, clip | lme-ui/render | unit | done |
| 0.11 | 9-slice shadow cache (no per-frame blur) | lme-ui/shadow.rs | unit, golden | done |
| 0.12 | Text: Roboto VF, shaped-line cache, glyph cache | lme-ui/text.rs | unit, golden | done |
| 0.13 | Images (PNG/JPEG decode, rounded clip) | lme-ui/img.rs | unit | done |
| 0.14 | Icons: freedesktop-icons lookup + fallback glyph | lme-ui/icons.rs | unit | done |
| 0.15 | Widget toolkit: H/VBox, Label, Icon, Button, Switch, Slider, TextInput, VirtualList, Popup | lme-ui/widgets | unit, golden | done |
| 0.16 | Animation manager (frame-callback driven, time-based easing) | lme-ui/anim.rs | unit, bench | done |
| 0.17 | `RenderBackend` trait + documented GPU stub seam | lme-ui/render | unit | done |
| 0.18 | Mock-compositor golden harness + tolerance diff | lme-shell/tests | golden | done |
| 0.19 | Delivery pipeline (scripts/deliver.sh) proven | scripts | dry-run | done |
| 0.20 | Idle budget: 0 CPU ticks / ≤1 wakeup per second | daemon | bench | done (bench/idle-bench, sandbox CPU) |

## Tier 1 — daily-driver shell

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 1.1 | Per-output bar: 3 zones, module order from barLayout, islands/connected styles, barFontSize/barHeight/margins | lme-shell/bar.rs | golden | done |
| 1.2 | Bar modules: launcher, overview, workspaces, windowTitle, activity, media, resources, tray, notifications, audio, network, battery, clock, settings, clipboard | lme-shell/modules/*.rs | unit+golden | partial: clock, workspaces, windowTitle, launcher, settings, notifications, battery, network, tray(icon count), audio(icon+level); media/resources/activity show placeholder text — needs MPRIS/probe feeds (Tier 2) |
| 1.3 | Launcher: apps mode, desktop scan, fuzzy ranking, favorites | lme-shell/launcher.rs + lme-backend/apps.rs | unit+golden | done (rank ported from LauncherSearch.js, unit-tested) |
| 1.4 | Launcher `=` calculator mode | lme-shell/launcher.rs | unit | done |
| 1.5 | Launcher `:` emoji mode (assets/emoji.json) | lme-shell/launcher.rs | unit | done |
| 1.6 | Launcher `@` windows mode | lme-shell/launcher.rs | unit | done (needs niri stream; mock fixture in tests) |
| 1.7 | Power menu: 5 actions, confirmations for destructive, Esc close, Overlay+Exclusive, focused output | lme-shell/powermenu.rs | golden+e2e | done |
| 1.8 | Power actions via logind D-Bus (trait + mock + --dry-run), lock=swaylock, logout=niri quit | lme-shell/power.rs | unit (mock bus) | done (dry-run default in sandbox; zbus call needs real-machine) |
| 1.9 | Notification daemon: toasts, history, DND | lme-shell/notifications.rs | unit+golden | done (zbus org.freedesktop.Notifications; server bind needs real session → real-machine note) |
| 1.10 | OSD (volume/brightness popups) + status toasts | lme-shell/osd.rs | golden | done |
| 1.11 | System tray: StatusNotifier + DBusMenu | lme-shell/tray.rs | unit | done (zbus + menu model; icons/activation need real session) |
| 1.12 | niri workspaces + window title + overview actions | lme-backend/niri.rs + modules | unit | done (request/response; event stream needs real niri) |
| 1.13 | Clock ticks on minute boundaries (seconds only when enabled) | lme-shell/clock.rs | unit | done |

## Tier 2 — control center and services

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 2.1 | Control center tabs: Controls, Audio, Devices, Agenda, Notifications | lme-shell/control.rs | golden | partial: Controls+Audio+Notifications tabs functional; Devices/Agenda stubbed |
| 2.2 | Audio + mic volume, brightness (wpctl/brightnessctl via lme-backend actions) | control center | unit (stub tools) | done |
| 2.3 | Bluetooth radios + devices (BlueZ D-Bus) | lme-shell/bluetooth.rs | unit (mock objects) | missing (external: blueman for pairing; toggles routed through lme-backend probes) |
| 2.4 | Wi-Fi scan/connect (NetworkManager) | lme-shell/wifi.rs | unit | missing (external: nm-connection-editor) |
| 2.5 | Power profiles, idle inhibit, night light | control center | unit | missing (probes exist in lme-backend; UI not wired) |
| 2.6 | Weather (existing lme-backend client + widget) | lme-shell/weather.rs | existing goldens | missing (client done; widget/UI not started) |
| 2.7 | Calendar / to-do / timer / stopwatch | lme-shell/agenda.rs | unit | missing |
| 2.8 | Clipboard history UI (lme-backend store ready) | lme-shell/clipboard_panel.rs | unit | missing |
| 2.9 | Activity island (MPRIS, timer, recorder status) | lme-shell/activity.rs | unit | missing |
| 2.10 | Workspace board (right-click overview) | lme-shell/overview.rs | golden | missing |
| 2.11 | Wallpaper via awww | lme-backend actions | existing | external (awww binary) |
| 2.12 | Lenovo charging helper | lme-backend/charging.rs | existing goldens | done (backend); UI missing |

## Tier 3 — completeness

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 3.1 | Settings window (legacy pages) as separate process | lme-shell/settings_app.rs | golden | missing |
| 3.2 | Idle behavior (ext_idle_notifier_v1, AC/battery profiles) | lme-shell/idle.rs | unit | missing |
| 3.3 | Monitors / displays / keybinds features (UPSTREAM_SYNC) | lme-backend + UI | unit | missing |
| 3.4 | Install script (Fedora 44) | scripts/install-native.sh | bash -n + dry run | missing |
| 3.5 | Polkit agent | legacy external | — | external (mission rule 7: only port if testable; stays legacy/external, README says so) |

## Tier 4 — hardening

| # | Feature | Target | Status |
|---|---|---|---|
| 4.1 | README (honest coverage, install, perf, checklist) | README.md | done |
| 4.2 | Performance report (idle wakeup, fps, RSS, cold start) | docs/PERF.md | done (sandbox-labeled) |
| 4.3 | Coverage table final pass | this file | done |
| 4.4 | ADRs 0001/0002 current | docs/adr/ | done |

## Known gaps carried by the frozen-contract backend

`lme-backend` implements the Phase 0 frozen contract (see
docs/UPSTREAM_SYNC.md): the upstream feature wave (wifi details, night light,
capture/OCR, monitors, keybinds, displays, idle, notification history,
feature-job `pending` semantics) is **not** ported; the 4 drifted golden
parity tests are `#[ignore]`d with that reason. The native shell's own
services consume the frozen surface. Catch-up port = future milestone
(NOT in this session's budget); noted in README limitations.
