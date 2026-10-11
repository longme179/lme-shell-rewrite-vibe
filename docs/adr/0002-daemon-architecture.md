# ADR 0002 — Daemon architecture: one `lme-shell` process, lazy panel surfaces

- Status: **Accepted** (owner decision 3, one-shot session 2026-10-11)
- Supersedes: ADR 0001's "spawn-on-demand standalone clients" (stack, renderer and
  measurement conclusions of ADR 0001 remain in force).

## Context

ADR 0001 validated the rendering stack with three spike clients and proposed
one process per panel (spawn-on-demand). The owner's native-rewrite mission
instead requires the whole shell to be one native program: bars, notification
daemon, tray, OSD and services in a single long-lived daemon, with panels as
lazily-created layer-shell surfaces that release their buffers when closed.
`lme-shell msg <command>` is a thin CLI over a unix socket; the settings
window runs as a separate process of the same binary.

## Decision

### Process model

```
lme-shell                      (daemon; owns everything below)
├── Wayland connection: per-output bars (Top layer), panel surfaces
│   (Overlay layer, created lazily on toggle, destroyed on close),
│   notification toasts, OSD, tray dropdown
├── Services (in-process, event sources on one calloop loop):
│   niri IPC stream · settings/palette (inotify) · clock (timer) ·
│   notifications (D-Bus) · tray (D-Bus) · MPRIS (D-Bus) · weather · clipboard
└── $XDG_RUNTIME_DIR/lme-shell.sock  (JSON-lines, one object per line;
    `lme-shell msg ...` is a thin client)

lme-shell msg <command>        (short-lived CLI process)
lme-shell settings             (separate window process; Tier 3)
```

### Event loop

One **calloop** poll loop. Event sources: Wayland socket (via
`calloop-wayland-source`), the IPC unix socket listener, inotify watches
(settings.json, palette.json), timers (clock on minute boundaries, service
refresh), and later D-Bus (zbus) and the niri event stream socket. No
per-source threads; subprocess probes stay bounded and async via
`Async::dispatch`-style detached jobs on the same loop (std::process in a
small blocking-pool thread per mission rule "no tokio full").

### Surfaces and memory

- Bars: one layer-shell surface per output, namespace `lme-bar`, created at
  startup, persist.
- Panels: created lazily on first toggle (`launcher`, `power`, `control`,
  `notifications`, `clipboard`, `overview`, `tray`, `wifi`, `settings`,
  `activity`), namespace `lme-<name>`, **destroyed on close** so their shm
  buffers and widget trees return to the allocator. Expected visible lifetime
  is seconds, so allocation at open is acceptable; steady state (bar only)
  must be allocation-free.
- Each surface owns one `SlotPool` buffer sized to its configure; hidden
  implies destroyed (no hidden-but-attached state).
- Rendering: full redraw while visible (ADR 0001), driven exclusively by
  `wl_surface.frame` callbacks during animation; static content draws once
  and requests no frames.

### IPC contract (msg CLI)

One JSON object per line over `$XDG_RUNTIME_DIR/lme-shell.sock`; one JSON
reply line, then close. Commands match the legacy Quickshell IPC surface used
by `config/niri/lme-shell.kdl`:

- `toggle <panel>` — launcher|power|control|notifications|clipboard|overview|
  activity|tray|wifi|settings
- `close` · `refresh` · `status`
- Power/media one-shots reused from the legacy media-keys fragment
  (`volume-up` etc.) land with the modules that need them (Tier 1+).

`config/niri/lme-shell-native.kdl` maps every legacy binding to
`spawn "lme-shell" "msg" ...`. The daemon never edits `config.kdl`.

### Consequences

- ADR 0001's stack (sctk 0.21.1 + tiny-skia + cosmic-text) is retained; the
  spike's surface/buffer code moves into `lme-ui` (see NATIVE_PLAN.md).
- The daemon crash takes down bars and services together — accepted because
  the alternative (multi-process) was measured at 7.8 MB idle per panel and
  the mission asks for a daemon; panics are confined per-event with
  `catch_unwind` at the loop boundary where feasible.
- `lme-backend` stays a workspace crate: its library (prefs, niri, weather,
  clipboard, apps, probes, actions) is linked into the daemon in-process; its
  binary remains the JSON-lines backend for the legacy QML shell
  (`LME_SHELL_BACKEND=rust`).
