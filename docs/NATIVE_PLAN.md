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

Legend: **done** (implemented + tested as described) · **partial**
(implemented but a named gap remains) · **missing** (not implemented) ·
**external** (a system component the shell calls but does not implement).
"e2e" = full daemon+mock-compositor integration test (crates/lme-shell/tests/golden.rs).
"real-machine" = compiled but not provable in the sandbox — flagged in README.

## Tier 0 — foundation

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 0.1 | Cargo workspace, one target dir, profiles | `/Cargo.toml` | build | done |
| 0.2 | Legacy isolation (qml/, python, demos → legacy/) | git mv | git status | done |
| 0.3 | `lme-shell` daemon + calloop loop | lme-shell/daemon.rs | e2e | done |
| 0.4 | `lme-shell msg` CLI + unix socket IPC | lme-shell/{main,ipc}.rs | unit + e2e | done |
| 0.5 | settings.json load/merge/validate (Phase 0 keys) + locked atomic writes | lme-backend/prefs.rs (existing) | existing goldens | done |
| 0.6 | settings hot-reload via inotify, upstream keys preserved | lme-shell/settings.rs | unit (tmp dir) | done |
| 0.7 | palette.json load + hot-reload | lme-ui/theme.rs + daemon inotify | unit | done |
| 0.8 | Wayland: outputs, layer-shell, seat key/pointer, shm, frame callbacks | lme-ui/wayland | e2e | done |
| 0.9 | Fractional scale v1 + viewporter (buffer scale 1) | — | — | missing (mock lacks the globals; client uses logical-size buffers at scale 1; integer-scale path is what all tests exercise — needs a protocol implementation before real fractional-scale claims) |
| 0.10 | CPU renderer: rounded rects, fills, borders, clip | lme-ui/render | unit + golden | done |
| 0.11 | 9-slice shadow cache (no per-frame blur) | lme-ui/shadow.rs | unit | done (not yet used by panels — shadows land with panel polish) |
| 0.12 | Text: Roboto VF, shaped-line cache, glyph cache, emoji color glyphs | lme-ui/text.rs | unit + golden | done |
| 0.13 | Images (PNG decode, icon blit path) | lme-ui/icons.rs + render | unit | done (PNG path; not yet used by UI surfaces) |
| 0.14 | Icons: hicolor PNG lookup + text-glyph fallback | lme-ui/icons.rs | unit | done (SVG icons not rasterized — D7) |
| 0.15 | Widgets: H/VBox, Label, Icon, Button, Switch, Slider, TextInput, VirtualList, FocusScope | lme-ui/widgets | unit | done (Switch/Slider/VirtualList implemented + unit-tested; panels currently use Label/Button/TextInput/FocusScope) |
| 0.16 | Animation manager (frame-callback driven, bezier curves) | lme-ui/anim.rs | unit + bench | done |
| 0.17 | `RenderBackend` trait + documented GPU stub seam | lme-ui/render | unit | done |
| 0.18 | Mock-compositor golden harness + tolerance diff | lme-shell/tests/golden.rs | e2e ×15 | done (pointer injection + timed topmost captures added in Step 0a; the old capture-commit mode was replaced for the launcher golden — it had captured an empty pre-frame buffer and passed vacuously, D19) |
| 0.19 | Delivery pipeline (scripts/deliver.sh) | scripts | TIER-1 delivery | done |
| 0.20 | Idle budget: ~zero CPU, ≤1 wakeup/s | daemon | bench | done (2 ticks/60 s, 0.15 wakeups/s — see docs/PERF.md) |

## Tier 1 — daily-driver shell

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 1.1 | Per-output bar: 3 zones, barLayout, connected pill, margins/height/font-size | lme-shell/bar.rs | golden | done |
| 1.2 | Bar modules (15) | lme-shell/bar.rs, mpris.rs | golden + e2e (real D-Bus) | done — clock (minute timer), workspaces (niri state + event stream), windowTitle, launcher/overview/notifications/settings/clipboard buttons, battery (sysfs), network/audio (fake-state data; live nmcli/wpctl feeds are a backend catch-up), tray (live StatusNotifier items), media (live MPRIS: playing player || first, trackTitle || identity, PropertiesChanged refresh, gone on owner loss), resources (live /proc CPU% + RAM% every 5 s, first sample null until the delta forms) all render with real feeds where the sandbox can prove them; activity island button renders (the island itself is Tier 2.9). Found + fixed D24 (receive_all_signals interface filter hid every PropertiesChanged) |
| 1.3 | Launcher apps mode: desktop scan, fuzzy ranking, favorites | lme-shell/launcher_data.rs | unit + e2e | done (ranking ported 1:1 from LauncherSearch.js; favorites read from settings; favorites-toggle UI not built) |
| 1.4 | Launcher `=` calculator mode | launcher_data.rs | unit | done (local evaluator, D13) |
| 1.5 | Launcher `:` emoji mode | launcher_data.rs + panels.rs | unit | done (copy via wl-copy spawn) |
| 1.6 | Launcher `@` windows mode + auto-append | panels.rs | unit (data path) | done (focusWindow spawn; needs real niri) |
| 1.7 | Power menu: 5 actions, destructive confirmations, Esc close, Overlay+Exclusive, focused output | lme-shell/panels.rs | golden + e2e | done |
| 1.8 | Power actions: logind D-Bus trait + dry-run, swaylock lock, niri quit | lme-shell/power.rs | unit (dry-run) | done — zbus/logind path compiled but only dry-run is provable in the sandbox (real-machine) |
| 1.9 | Notification daemon: toasts, history, DND | lme-shell/notifications.rs | unit + e2e (real D-Bus) | done — org.freedesktop.Notifications server (zbus blocking on its own thread, D4 pattern; body+actions capabilities like legacy), toast surface (360px top-right Overlay column, ≤3 cards), expiry rules (0=never, −1=7s/critical-never, >0=ms), replace-by-id, transient drop, history cap 200, DND (dndUntil setting) + muted apps (notificationMutedApps — both validators added mirroring desktop_backend.py), bell unread badge + markRead-on-open, NotificationClosed/ActionInvoked signals, `msg notify/dnd/notifications`; **notification CENTER panel is Tier 2** (bell answers with a notice + marks read); history persistence (legacy notificationSave) deferred to the Tier-2 backend catch-up — in-memory only (D20) |
| 1.10 | OSD (volume/brightness) + status toasts | lme-shell/panels.rs | e2e | done (msg osd/status; auto-expiry) |
| 1.11 | System tray: StatusNotifier + DBusMenu | lme-shell/tray.rs | unit + e2e (real D-Bus) | done — org.kde.StatusNotifierWatcher host (manager thread, D20 pattern; bus-name + path registrations, Registered/Unregistered signals, per-item New*/PropertiesChanged/LayoutUpdated listeners, NameOwnerChanged owner-loss cleanup; no bus = non-fatal, bar hides the button), SNI snapshots (pixmaps ARGB32→premultiplied with channel clamp, ToolTip, ItemIsMenu, Menu), com.canonical.dbusmenu client (AboutToShow + GetLayout, (ia{sv}av) tree parse with variant unwrap, flat in-panel menu page + back row, Event(clicked)), 268px right-anchored panel (legacy TrayMenu port: hover, primary Activate/onlyMenu, secondary menu, middle SecondaryActivate, wheel Scroll ±120/notch, outside-click dismiss, empty state, auto-close when last item dies), `msg tray` JSON. Deltas vs legacy (D22): menu is an in-panel page (flat, indented submenus) not a floating platform menu; panel anchors at the right edge (no trayAnchorX layout info). Found + fixed D23 (toast click range swallowed the tray id space) and a fetch_menu parse bug (layout member fed to the whole-reply parser — every menu was 'unparsable') |
| 1.12 | niri workspaces + window title + overview action | lme-shell/niri.rs | unit + e2e (fake state) | done (event-stream refresh wired; workspace FOCUS click path returns a Tier-2 notice — moveWindow/focus actions need the live socket, real-machine) |
| 1.13 | Clock ticks on minute boundaries | daemon.rs clock chain | bench | done |
| 1.14 | Pointer interaction pass (Amendment-3 findings a–e) | lme-ui/wayland+widgets, lme-shell/{bar,panels,daemon} | unit + e2e | done — (a) audio/network/battery are hover+click pills (control/wifi notices; network routes to the wifi panel like legacy); (b) launcher hover highlight + click-to-launch + wheel (one row per notch); (c) CenteredPanel outside-click dismiss + surface-coordinate fix (panel-local translation broke every click at the real centered offsets) + non-zero-offset regression tests; (d) value120 notches / touchpad px accumulation (50 px/line) / axis_stop reset — mock sends realistic wheel (120+53px) AND touchpad forms, e2e covers both; (e) NEW behavior documented in README: workspace wheel focus up/down, audio-pill wheel = ±5% dry-run volume OSD. Found + fixed D19 (launcher results were never populated on open/typing) |

## Tier 2 — control center and services

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 2.1 | Control center tabs | control_center.rs | unit + e2e golden + VLM | done (Controls tab; others placeholder'd; overflow KI-3) |
| 2.2 | Audio/brightness actions | daemon ControlActions | e2e control_volume_slider_flushes_action | done (volume/mute/brightness via wpctl/brightnessctl, 100 ms debounce, optimistic snapshot) |
| 2.3 | Bluetooth (BlueZ) | daemon probe_bluetooth + ControlActions | e2e control_bluetooth_tile_toggles (fake org.bluez + service-missing) | done (state tile + power toggle; pairing UI external) |
| 2.4 | Wi-Fi (NetworkManager) | ControlActions::wifi_toggle | e2e control_wifi_tile_toggles_radio | partial (radio toggle done; WifiPanel connection list = TIER-2 completion) |
| 2.5 | Power profiles / idle inhibit / night light | ControlActions + held children | e2e control_stay_awake_night_light_and_power_profile | done (profile row + stay-awake inhibitor + night-light child; swayidle manager itself = idle-management row) |
| 2.6 | Weather UI | — | — | missing (backend client done in Phase 1) |
| 2.7 | Calendar / to-do / timer | — | — | missing |
| 2.8 | Clipboard history UI | — | — | missing (store done in Phase 1) |
| 2.9 | Activity island (MPRIS, timer, recorder) | — | — | missing |
| 2.10 | Workspace board | — | — | missing |
| 2.11 | Wallpaper via awww | lme-backend actions | existing | external (awww binary) |
| 2.12 | Lenovo charging helper | lme-backend/charging.rs | existing goldens | done (backend; UI missing) |

## Tier 3 — completeness

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 3.1 | Settings window (legacy pages) | — | — | missing (stub prints a notice; must mirror upstream f4ddf64: openPage IPC reports success only when the window is actually mapped, and callers restart a dead instance) |
| 3.2 | Idle behavior (ext_idle_notifier_v1) | — | — | missing |
| 3.3 | Monitors / displays / keybinds (UPSTREAM_SYNC) | — | — | missing |
| 3.4 | Install script (Fedora 44) | — | — | missing |
| 3.5 | Polkit agent | legacy external | — | external (mission rule 7) |

## Tier 4 — hardening

| # | Feature | Target | Status |
|---|---|---|---|
| 4.1 | README (honest coverage, install, perf, checklist) | README.md | done |
| 4.2 | Performance report | docs/PERF.md | done (sandbox-labeled) |
| 4.3 | Coverage table final pass | this file | done |
| 4.4 | ADRs 0001/0002 current | docs/adr/ | done |

## Session summary (this delivery)

- Tier 0 complete (0.9 fractional-scale excepted, documented above).
- Tier 1: **complete** (1.1–1.14); Tier 0.9 fractional-scale still open.
- Tiers 2–3: only backend-frozen parts done (2.11 external, 2.12 backend);
  the rest missing. Tier 4 done for what exists.
- Gates at every commit: `cargo fmt`, `cargo clippy -D warnings`,
  `cargo test --workspace` — 93 passed, 0 failed, 4 ignored (documented
  upstream-drift goldens in lme-backend; see docs/UPSTREAM_SYNC.md).
- Verification evidence: docs/screens/ (bar, power menu, launcher from the
  golden harness), docs/PERF.md (idle/anim numbers above).

## Known gaps carried by the frozen-contract backend

`lme-backend` implements the Phase 0 frozen contract (see
docs/UPSTREAM_SYNC.md): the upstream feature wave (wifi details, night light,
capture/OCR, monitors, keybinds, displays, idle, notification history,
feature-job `pending` semantics) is **not** ported; the 4 drifted golden
parity tests are `#[ignore]`d with that reason. The native shell's own
services consume the frozen surface. Catch-up port = future milestone;
noted in README limitations.
