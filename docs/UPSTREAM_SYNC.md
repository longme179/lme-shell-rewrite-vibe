# UPSTREAM_SYNC — origin/main moved after the Phase 0 baseline

Done as PHASE2_AMENDMENT_1 Step U, 2026-10-11. Base used by all Phase 0–2.0
work: `ead4446` ("Add clipboard history and expanding Activity island").
New upstream head: `ef5281c` ("Restore Settings and add element themes with
liquid feedback").

## New upstream commits (all 2026-10-11, oldest first)

| Commit | Subject |
|---|---|
| `6765d19` | Add appearance profiles, safe displays and integrated desktop controls |
| `4ee056f` | Add detailed monitoring, laptop idle profiles and niri shortcuts |
| `a99a027` | Refresh keybindings when the selected config path changes |
| `414167f` | Add Night Light, screenshot studio and motion presets |
| `ef5281c` | Restore Settings and add element themes with liquid feedback |

Total: `git diff --stat ead4446..ef5281c` → **107 files changed, 5609
insertions(+), 705 deletions(-)**.

## Classification (per Step U item 1)

### Backend commands (`scripts/`, 12 files, +1423/−54)
- `desktop_backend.py`: +472/−54 — imports a new `desktop_features` module;
  adds feature-job infrastructure (background threads + `pending` results),
  wifi scanning (`nmcli` fields incl. `networks[]` with `security/secured/
  enterprise/bssid`, `saved[]`, `device`), `system.battery.acOnline`,
  `platform{}`, `idle`, `nightLight`, `capture` (screenshot+OCR), `monitor`,
  `keybinds`, `displays`, `wallpaperLibrary`, `notificationHistory`.
- **New scripts**: `desktop_features.py` (profiles/motion/theme defaults),
  `display_control.py`, `idle_control.py`, `night_light.py`,
  `niri_keybinds.py`, `screen_capture.py`, `system_monitor.py`.
- `doctor.sh`, `settings.sh`, `start.sh`, `install-fedora.sh`: minor updates.
- **Phase 1 impact**: the Rust `lme-backend` now covers a strict subset of
  the Python feature surface → 4 golden parity tests fail (details in
  STATE.md "Upstream parity blocker"); classified a **blocker for the
  owner**, not fixed (the port is Phase-1.5-sized, outside "small fix").

### Settings schema (`settings.json`)
~28 new preference keys: `appearanceProfiles`, `barEdge`, `barEdgeMargin`,
`barHeight`, `barSideMargin`, `controlLayout`, `controlWidth`, `density`,
`displayLayout`, `dndUntil`, `favoriteApps`, `fontOverrides`, `fontSize`,
`idleEnabled`, `idleLockBeforeSleep`, `idleProfiles{ac,battery}`,
`motionBounce`, `motionPreset`, `motionSpeed`, `nightLightEnabled`,
`nightLightEnd`, `nightLightMode`, `nightLightStart`,
`nightLightTemperature`, `niriConfigPath`, `notificationMutedApps`,
`popupBlur`, `themeOverrides`, `wallpaperFavorites`, `wallpaperFolder`.
The QML settings app (`qml/settings/`, 14 files) was reworked to match.

### Launcher (Step 2.2 target) — CHANGED, spec must follow new behavior
- `qml/panels/Launcher.qml` reworked (118 lines now, was ~250): multi-mode
  query — apps (default), `=`/bare-expression **calculator** (backend
  `calculate` action, 140 ms debounce), `:` **emoji** (assets/emoji.json,
  copy-on-Enter), `@` **windows** (niri window focus, auto-appended to app
  results when text matches), **favorites** (`preferences.favoriteApps` +
  Favorites toggle), list-based results (was icon tiles), `maximumLength:
  160`, placeholder "Apps · = calculate · : emoji · @ windows".
- **New `qml/services/LauncherSearch.js`**: fuzzy ranking — exact 100 /
  prefix 85 / substring 70 / genericName+comment+keywords 55 / subsequence
  40−len-gap / per-word Levenshtein (≤ min(2, len/3)) 20; up to 8 space
  separated terms with AND semantics (every term must match ≥0); sort:
  score desc → favorites first → `name.localeCompare`.
- Launch semantics for apps unchanged (`launch {id}`); new actions used:
  `calculate`, `focusWindow`, `setting{favoriteApps}`.

### Power menu (Step 2.1 target) — UNCHANGED
`qml/panels/PowerMenu.qml` (133 lines) is byte-identical to the Phase 0
baseline. The 2.1 spec (REBUILD_SPEC §5) remains valid.

### Other QML panels / shell
- `ControlDashboard.qml` major rework (488 lines changed); new panels:
  `ScreenshotEditor.qml`, `SystemMonitor.qml`, `WifiPanel.qml`,
  `WifiWidget.qml`.
- `Theme.qml` motion system (+113: DankMaterialShell-derived curves),
  `qml/components/` (13 files incl. new LiquidFeedback ripple),
  `shell.qml` (+46), bar modules, `Notifications.qml` (history/DND),
  `DeviceControls.qml`, `DesktopPreview.qml`, `WeatherWidget.qml`,
  `ChargingWidget.qml`, `ActivityIsland.qml`, auth window.
- `ShellState.qml`: new state (popupBlur, panelKind, wifiOpen, controlTab,
  wifiResult, idle inhibit) — **merged cleanly with our Phase 1
  `LME_SHELL_BACKEND` switch** (both sides preserved).

### Docs, tests, assets, third-party
- `docs/previews/` +17 PNGs; `docs/` +9 (INIR_PORT.md and others);
  new `third_party/DankMaterialShell-LICENSE`; `THIRD_PARTY.md` reworked
  upstream (end-4/iNiR/DankMaterialShell/Caelestia/Open-Meteo attributions)
  — merged with our Phase 2 sections (both preserved).
- `tests/`: +5 files (`test_capture_night.py`, `test_laptop_features.py`,
  `test_personalization.py`, `test_upgrade_ui.py`, `test_upgrades.py`);
  Python suite now 147 tests, all pass in container (18 skip).
- `assets/` (emoji.json), `AGENTS.md`, `.agents/skills/ponytail`,
  `requirements-fedora.txt` (`tesseract` etc.), `config/niri/` (+3).

## Rebase of `rewrite/phase-2` onto `origin/main` (Step U item 2)

One conflict only (`THIRD_PARTY.md`, both sides had rewritten the file) —
resolved by keeping the upstream attribution document and appending our
Phase 2 sections. `README.md` and `ShellState.qml` auto-merged with both
sides' changes intact (verified by grep). Old → new commit hashes (linear
history preserved, no merges; `rewrite/phase-0` and `rewrite/phase-1`
repointed to their rebased equivalents so every kept branch sits on one
consistent history):

| Old | New | Commit |
|---|---|---|
| `cf5c92e` | `37c2256` | Phase 0 baseline |
| `4a86c55` | `6306804` | M1+M2 |
| `5d3ada4` | `9a9b318` | M3 |
| `0d38b3c` | `250f8cc` | M4 |
| `1e6cbd5` | `abfd158` | M5 |
| `f2e0753` | `02b1835` | M6 |
| `d1848df` | `ac294bc` | M7 |
| `a9fe198` | `b8e438e` | Step 2.0 spike |
| `d326d1e` | `d310ed2` | PHASE2_AMENDMENT_1 doc |
| `c85755b` | `169409c` | Step C scripts |
| `63da180` | `9fabeea` | Step C wip.patch fix |

Verification that nothing was lost: `git diff <old-tip> <new-tip>` touches
exactly the 107 files upstream changed — the delta between the old and new
branch tips is purely the upstream incorporation.

## Gate re-run after rebase (Step U item 3)

- `lme-backend`: fmt ✓, clippy `-D warnings` ✓, tests **50/54** — 4 golden
  parity failures, all caused by the upstream feature wave above (exact
  field diffs in STATE.md): snapshot parity ×3 (`--once` live/demo/settings)
  and `action_sequence_parity` action 15 `lock` (Python now returns
  `{"ok":true,"message":"Working…","pending":true}` — async feature job —
  while lme-backend still acks synchronously "Lock requested").
- Spike crates: `mock-compositor` fmt/clippy/3 tests ✓; `spike-cpu`
  fmt/clippy/2 tests ✓; `spike-gpu` fmt/clippy/build ✓ (no run: no EGL in
  cloud, unchanged); `spike-slint` fmt/clippy ✓, 0 unit tests by design.
- Python suite: 147 tests OK, 18 skipped (container skips).
- `bash -n` all shell scripts ✓ (via check.sh run in the replay).

Decision per amendment ("fix Phase 1 only when small"): the parity gap =
9 new top-level snapshot fields + ~28 preferences keys + nested
`battery.acOnline` / `network.{device,networks[],saved}` growth + the
feature-job (`pending`) action semantics. That is a Phase-1.5-scale port,
not a small fix inside crates we own → **listed as a blocker for the
owner**; the Rust backend remains a faithful implementation of the Phase 0
frozen contract (which `LME_SHELL_BACKEND=rust` still serves correctly for
the QML shell's core surface).

## Sync 2026-10-12 (post-recovery check, Amendment 2)

Upstream moved ef5281c → 56fc7a4 (2 commits). `git diff --stat
ef5281c..origin/main -- qml scripts docs README.md` = 5 files:
README.md (+517/−204 rewrite), docs/TROUBLESHOOTING.md, docs/VALIDATION.md,
qml/settings/SettingsApplication.qml, scripts/settings.sh.

- `f4ddf64` Require a visible Settings window before reusing IPC instance:
  `openPage` IPC now returns `bool` (`window.visible && !window.minimized`);
  settings.sh only reuses the running instance when the reply is `true`,
  otherwise kills it and starts a fresh one (Quickshell 0.3.2 can exit 0 on an
  IPC error). **Impact on the native rewrite:** Tier 3.1 (`lme-shell settings`
  separate process) must mirror this contract — its `openPage` IPC must only
  report success when a window is actually mapped, and the launcher fallback
  must restart a dead/unresponsive instance. Recorded as a Tier 3.1
  requirement in NATIVE_PLAN.md. No rebase performed (per Amendment 2);
  behavior read via `git show origin/main:<path>`.
- `56fc7a4` README rewrite (Fedora step-by-step): documentation only; informs
  the Tier 4 README tone/structure (step-by-step install + command
  explanations). No code impact.

No other upstream changes since the Step U sync. The 4 ignored lme-backend
goldens (upstream feature wave) are unchanged — owner decision still pending.
