# Continue: you stopped too early (continuation prompt, 2026-10-12)

Your TIER-1 delivery was good, but the session is NOT complete. Your own report
says done=34, partial=1, missing=17, external=2, and Tier 1 still lacks the
notification daemon, system tray and the MPRIS feed. The mission is unchanged:
a complete native Rust shell, delivered as source + README through the webhook.
Continue autonomously and unattended until Tier 4 is done or about 85% of your
budget is used, then send a FINAL delivery. Do not write "session complete"
before that. You still cannot ask me anything: decide, and log each decision in
docs/DECISIONS.md (mark guesses [ASSUMPTION]).

> Session-setup block intentionally NOT stored here, per the "never write this
> block into any file" rule. Items 1 and 3 are superseded by
> docs/MISSION_AMENDMENT_2.md (GitHub backup + recovery). Item 2 (save this
> file, commit) still applies — done at recovery.

# Recap of binding decisions (details: docs/DECISIONS.md, docs/adr/)

- Rust only. Stack per ADR 0001 (smithay-client-toolkit + tiny-skia + cosmic-text,
  CPU renderer behind a RenderBackend trait, no GPU code you cannot run). One
  daemon `lme-shell`, lazy panel surfaces, `lme-shell msg` CLI, `lme-shell
  settings` as a separate process (ADR 0002).
- Stay compatible with ~/.config/lme-shell/settings.json and palette.json (keep
  unknown and upstream-added keys; locked merge writes). niri bindings live in
  config/niri/lme-shell-native.kdl; never edit the user's niri config.
- legacy/ is a read-only reference and is excluded from the export.
- GPL-3.0-or-later. Noctalia (MIT) is an architecture reference only; log any
  adapted code in THIRD_PARTY.md. No Luau plugins. No native lock screen, PAM or
  password handling: call the external programs the legacy code calls.
- Rules: one poll-based loop; four frame phases (prepare, update, layout, render;
  render never mutates the tree); animation only from frame callbacks using the
  callback timestamp; D-Bus never blocks UI paths and failures are non-fatal;
  idle via ext_idle_notifier_v1; settings and state in separate files.
- Quality bar: zero CPU ticks when idle and at most 1 wakeup/s; idle RSS at most
  40 MB plus 15 MB per extra output; control center at most +25 MB and back to
  baseline on close; shm buffer reuse; crisp fractional scaling; 9-slice shadow
  cache; no gradients; no CPU blur.
- Honesty: never write "works" for something that only ran against the mock
  compositor or unit tests. Write "implemented, needs real-machine validation".
- Tests and demos never call real logind or spawn real apps (trait + mock +
  --dry-run). Never log the weather location.
- Gates at every tier end: fmt, clippy -D warnings, test, e2e goldens.
- Send nothing to Discord except lean checkpoints and the deliveries below.

# Work order (NATIVE_PLAN.md stays the source of truth for status)

Step 0, test gaps from your own report (do first):
 a. Pointer. The mock compositor has no pointer, so every click, hover and
    scroll path is untested. Add pointer enter/leave/motion/button/axis
    injection to the mock (like --key-events). Add e2e tests: bar module click,
    workspace pill click, hover state, scroll on volume and workspaces, launcher
    list click, power menu button click, dismiss on outside click. Fix what breaks.
 b. Pacing. Your log mentions a paced 120 Hz measurement, but your report says
    the mock cannot pace vsync. Reconcile: make the mock deliver frame callbacks
    at the configured refresh from a timer, then report delivered fps, dropped
    frames and p99 render time during a panel reveal. If it truly cannot, say
    exactly why in README and docs/PERF.md.

Step 1, finish Tier 1: notification daemon (org.freedesktop.Notifications via
 zbus: toasts with stable slots, actions, urgency, timeouts, history, Do Not
 Disturb, hover pause), system tray (StatusNotifierWatcher/Item and async cached
 DBusMenu), MPRIS feed, niri workspace/window/overview actions, and every bar
 module still missing. For D-Bus tests, check `which dbus-daemon dbus-run-session`
 and use a private session bus; otherwise use zbus peer-to-peer connections
 (verify in the zbus 5.x docs before relying on it). Deliver TIER-1B.

Step 2, Tier 2, in this order (my judgment of daily value): control center shell
 and Controls tab; audio (PipeWire/WirePlumber; if the libs do not build here,
 use a trait with a wpctl/pactl subprocess fallback and mark it); brightness;
 NetworkManager; BlueZ; power profiles; idle inhibit; Notifications tab;
 clipboard history (same tool as legacy, wl-clipboard, unless a pure-Rust path
 works); calendar, to-do, timer, stopwatch, Agenda tab; weather; activity island;
 night light; capture/screenshots; wallpaper via awww; workspace board; client
 for the existing Lenovo charging helper. Deliver TIER-2.

Step 3, Tier 3: settings window (all legacy pages, same settings.json), idle
 behavior, monitors/displays/keybinds from docs/UPSTREAM_SYNC.md, install script.
 If the settings window is not finished, the README must say the legacy
 Settings app from the upstream repo still edits the same file. Deliver TIER-3.

Step 4, Tier 4: README, coverage table, PERF report, ADRs, final polish, FINAL.
If you cannot finish everything, keep this order. Do not leave half-working
features: finish them, or remove them from the build and mark them missing with
the reason. A missing service (no BlueZ, NetworkManager or PipeWire) must never
panic; degrade gracefully and hide the control.

# Visual quality pass (before TIER-2 and again before FINAL)

- Compare each panel golden with the legacy preview of the same panel in
  docs/previews/ (rendered upstream from the QML). Check palette, radii,
  paddings, type sizes, spacing and glass look. Save side-by-side PNGs (200 KB
  or smaller each) in docs/screens/compare/ and fix clear deviations.
- If you can view images, inspect them yourself. If you cannot, say so in the
  README and use numeric checks (token values, layout metrics); never claim it
  looks good.
- Smoothness: reveals, dismissals and hover transitions use springs or eased
  tweens driven by frame callbacks. Nothing restarts or stutters on resize or
  settings hot reload.

# Delivery

Same pipeline as before (scripts/deliver.sh): gates, export, clean replay,
fail-closed secret scan, zip + README + one summary message. Labels: TIER-1B,
TIER-2, TIER-3, FINAL. Interim checkpoints must be lean; use CKPT_FULL=1 only at
FINAL (the full ckpt-009 after TIER-1 flooded the channel for no benefit).
The FINAL summary includes: coverage counts, commit hash, build command, fps and
RSS numbers labeled "cloud mock", and the three things I must test first on my
laptop. If discord.com becomes unreachable, put the files in the workspace panel
or attach them to your chat reply and state their paths.

# Done

FINAL delivered. Your last message states what was sent, the commit hash and
the top three things to test.
