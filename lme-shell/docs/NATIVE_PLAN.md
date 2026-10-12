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
| 0.18 | Mock-compositor golden harness + tolerance diff | lme-shell/tests/golden.rs | e2e ×7 | done |
| 0.19 | Delivery pipeline (scripts/deliver.sh) | scripts | TIER-1 delivery | done |
| 0.20 | Idle budget: ~zero CPU, ≤1 wakeup/s | daemon | bench | done (2 ticks/60 s, 0.15 wakeups/s — see docs/PERF.md) |

## Tier 1 — daily-driver shell

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 1.1 | Per-output bar: 3 zones, barLayout, connected pill, margins/height/font-size | lme-shell/bar.rs | golden | done |
| 1.2 | Bar modules (15) | lme-shell/bar.rs | golden | partial — clock (minute timer), workspaces (niri state + event stream), windowTitle, launcher/overview/notifications/settings/clipboard buttons, battery (sysfs 30 s), network/audio (fake-state data; no live probes), tray (hidden, no items) are rendered; **media/resources/activity have no live feed (MPRIS/probes not wired) and stay hidden** |
| 1.3 | Launcher apps mode: desktop scan, fuzzy ranking, favorites | lme-shell/launcher_data.rs | unit + e2e | done (ranking ported 1:1 from LauncherSearch.js; favorites read from settings; favorites-toggle UI not built) |
| 1.4 | Launcher `=` calculator mode | launcher_data.rs | unit | done (local evaluator, D13) |
| 1.5 | Launcher `:` emoji mode | launcher_data.rs + panels.rs | unit | done (copy via wl-copy spawn) |
| 1.6 | Launcher `@` windows mode + auto-append | panels.rs | unit (data path) | done (focusWindow spawn; needs real niri) |
| 1.7 | Power menu: 5 actions, destructive confirmations, Esc close, Overlay+Exclusive, focused output | lme-shell/panels.rs | golden + e2e | done |
| 1.8 | Power actions: logind D-Bus trait + dry-run, swaylock lock, niri quit | lme-shell/power.rs | unit (dry-run) | done — zbus/logind path compiled but only dry-run is provable in the sandbox (real-machine) |
| 1.9 | Notification daemon: toasts, history, DND | — | — | missing (NotificationHub state struct exists; no org.freedesktop.Notifications server, no toast surface fed by external apps — Tier 2) |
| 1.10 | OSD (volume/brightness) + status toasts | lme-shell/panels.rs | e2e | done (msg osd/status; auto-expiry) |
| 1.11 | System tray: StatusNotifier + DBusMenu | — | — | missing (bar tray module renders only with items>0; no watcher implemented) |
| 1.12 | niri workspaces + window title + overview action | lme-shell/niri.rs | unit + e2e (fake state) | done (event-stream refresh wired; workspace FOCUS click path returns a Tier-2 notice — moveWindow/focus actions need the live socket, real-machine) |
| 1.13 | Clock ticks on minute boundaries | daemon.rs clock chain | bench | done |

## Tier 2 — control center and services

| # | Feature | Target module | Test | Status |
|---|---|---|---|---|
| 2.1 | Control center tabs | — | — | missing |
| 2.2 | Audio/brightness actions | — | — | missing (probes exist in lme-backend; no UI) |
| 2.3 | Bluetooth (BlueZ) | — | — | missing (external tools for pairing) |
| 2.4 | Wi-Fi (NetworkManager) | — | — | missing (external: nm-connection-editor) |
| 2.5 | Power profiles / idle inhibit / night light | — | — | missing |
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
| 3.1 | Settings window (legacy pages) | — | — | missing (stub prints a notice) |
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
- Tier 1: 1.1–1.8, 1.10, 1.12, 1.13 done; 1.2 partial; 1.9, 1.11 missing.
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
