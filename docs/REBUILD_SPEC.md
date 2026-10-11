# REBUILD_SPEC — language-neutral behavior specification

Purpose: describe LME Shell's behavior precisely enough that any component can
be reimplemented in any language, without reading the original QML/Python.

Status legend: **specified** (this doc + golden fixtures are sufficient to
rebuild), **partial** (contract/inputs/outputs known; UI-level spec pending),
**not started** (out of current phase scope; listed for coverage).

Primary sources: `docs/REWRITE_INVENTORY.md` (Phase 0 wire-contract freeze,
extracted at commit `ead4446`), `bench/RESULTS.md`, Phase 1 `lme-backend`
crate + tests. Phase 2 (this branch) fully specifies only the **power menu**
and the **application launcher** (their full sections are written in Steps
2.1/2.2); everything else is filled only as far as the Phase 0 inventory
allows.

## 1. Coverage table (README features)

| README feature | Status | Where specified |
|---|---|---|
| Power menu (lock/suspend/logout/restart/shutdown, confirmations) | partial → full in Step 2.1 | §5 below; action semantics §3.4 |
| Application launcher (keyboard-searchable `.desktop` list) | partial → full in Step 2.2 | §6 below; scan/launch semantics §3.2/§3.4 |
| Modular per-output top bar (15 modules, 3 zones) | not started (Phase 2 out of scope) | data feeds: §3.1 snapshot |
| Overview button / native overview | not started | — |
| System tray dropdown | not started | — |
| Clipboard history (text/image, search, pins, pause) | specified | §4.2; golden: `lme-backend/tests/clipboard_cross.rs` |
| Activity island (MPRIS, focus timer) | not started | — |
| Control center / dashboard | not started | data feeds: §3.1 `system` probes |
| Audio mixer (PipeWire per-device/app) | not started | volume probes/actions: §3.2/§3.4 |
| Weather (auto IP location, forecast) | specified | §4.3; golden: `lme-backend/tests/weather_parity.rs` |
| Lenovo charging controls (85%/100%) | specified | §3.2 `charging`, §3.4 `chargeMode`; golden: charge sysfs tests |
| Polkit authentication popup | not started (explicitly out of Phase 2) | §7 note |
| Wallpaper selection/restoration (awww) | specified | §3.4 `wallpaper`/`--restore-wallpaper` |
| Calendar / todo / focus timer | not started | — |
| MPRIS media controls | not started | — |
| Notifications (native, history, DND) | not started | — |
| Settings application | partial | settings.json schema §4.1 fully specified; UI not started |
| Material tonal surfaces / theming | partial | palette.json §4.4; tokens not yet inventoried |
| State backend (`desktop_backend.py` / `lme-backend`) | specified | §3 complete; golden: `lme-backend/tests/parity.rs` etc. |

## 2. Process topology (current)

```
quickshell -c lme-shell                       (QML UI, 1 process)
├── state backend (python3 desktop_backend.py | lme-backend), JSON lines on stdio
│   └── ~10 short-lived subprocesses per 2 s poll + 1 persistent niri event-stream
├── clipboard service (python3 clipboard.py | lme-backend --clipboard)
├── polkit agent (in-process QML)
└── settings app (separate quickshell instance, backend started with --settings)
```

Phase 2 adds two standalone Wayland layer-shell clients (`lme-powermenu`,
`lme-launcher`) that coexist with the QML shell; QML stays default.

## 3. State backend (fully specified)

### 3.1 Purpose and I/O contract
Long-running process speaking **JSON lines on stdio**: emits one complete
state snapshot per line; consumes one action object per line; every input
line produces exactly one snapshot with an updated `lastResult`.
Snapshot fields: `apps[]`, `niri{}`, `system{}`, `weather{}`, `preferences{}`,
`lastResult{ok,action,message[,pending],sequence}` (sequence monotonic).
Malformed JSON → `lastResult{ok:false,action:"",message:"Invalid JSON input"}`;
line > 64 KiB → "Input exceeds 64 KiB limit".

### 3.2 Inputs / sources (per probe, cached 1.5 s)
cpu (`/proc/stat` delta), memory (`/proc/meminfo`), temperature (hwmon list),
battery (sysfs), network (3 × nmcli), bluetooth (BlueZ GetManagedObjects +
rfkill), audio/mic (wpctl), brightness (brightnessctl), powerProfile (busctl
×2 names, powerprofilesctl fallback), charging (sysfs charge_control_* /
ideapad conservation), recording (`/proc/*/exe` scan), apps (`.desktop` scan,
60 s cadence), niri (2 × `niri msg --json`), weather (§4.3), preferences
(§4.1). Exact field shapes: `docs/REWRITE_INVENTORY.md` §2.1–§2.2.

### 3.3 CLI flags
`--once` (read-only snapshot, no network, exit), `--demo` (fixed sample state,
simulated mutations, never persists), `--settings` (prefs+wallpaper only,
frozen niri/apps), `--restore-wallpaper FILE`, `--interval SECONDS` (min 0.5,
default 2.0), `--clipboard` (clipboard daemon mode), `--capture <kind>`
(one-shot clipboard capture).

### 3.4 Actions (27, exact payloads and implementations)
Frozen in `docs/REWRITE_INVENTORY.md` §2.3 with QML callers. The power-menu
and launcher relevant subset:

| Action | Implementation today (Python) | Callers |
|---|---|---|
| `lock` | spawn `swaylock --color 22212c` | PowerMenu, ControlDashboard |
| `logout` | `niri msg action quit --skip-confirmation` | PowerMenu |
| `suspend` | `systemctl suspend` | PowerMenu |
| `reboot` | `systemctl reboot` | PowerMenu |
| `poweroff` | `systemctl poweroff` | PowerMenu |
| `launch` `{id}` | desktop-entry argv; `Terminal=true` → configured terminal `-e`; else `gio launch <path>` (argv spawn fallback) | Launcher |

Phase 2 deviation (approved in the Phase 2 kickoff): native power-menu
executes power actions via **systemd-logind on the system D-Bus** instead of
`systemctl` CLI; lock/logout semantics unchanged (swaylock spawn / niri quit).
Dry-run mode is mandatory for tests.

### 3.5 Error/edge contract
Any failure → `lastResult{ok:false, message:str(exc)[:400], sequence}` shown
as a status toast. Subprocess env: `LC_ALL=C`. `settings.json` cross-writer
safe via `settings.json.lock` flock + atomic replace (temp 0600 + fsync).

### 3.6 Golden fixtures
`lme-backend/tests/` (parity.rs, actions_parity.rs, niri_socket.rs,
prefs_cross.rs, weather_parity.rs, clipboard_cross.rs, loopd.rs) with stub
tools and fake sysfs trees; the Python suite in `scripts/` remains the
behavioral reference.

## 4. Files and schemas

### 4.1 `~/.config/lme-shell/settings.json` (read/write, atomic)
Dir 0700, file 0600, `indent=2` + trailing newline, key order = defaults
order then new keys. Keys with validation (invalid ignored, not fatal):
`language:"en"`, `animations:bool`, `opacity:0.60–1`, `accent:#rrggbb`,
`wallpaper:""|absolute existing`, `terminal:[A-Za-z0-9_.+-]{1,80}`,
`bluetoothAdapter:""|/org/bluez/hci[0-9]+`, `tasks[]{id,text≤240,done}≤100`,
`weatherAutoLocation:bool`, `weatherLocation{}`, `weatherUnit:c|f`,
`activityEnabled:bool`, `clipboardEnabled:bool`, `clipboardImages:bool`,
`clipboardLimit:20–500`, `barStyle:islands|connected`, `barFontSize:12–16`,
`barLayout{left,center,right}` unique ids of the 15 known modules.

### 4.2 `~/.local/state/lme-shell/clipboard/history.sqlite3`
Tables `entries(id PK sha256, kind, mime, data BLOB, preview, search, suffix,
created, pinned)`, `meta(key,value)`; images as `images/<id>.<ext>`; trim to
limit/64 MiB keeping pins; single-owner `capture.lock` flock.

### 4.3 Weather providers
`api.open-meteo.com` (forecast), `geocoding-api.open-meteo.com` (search, 6
results), `ipwho.is` (geo-IP). TTLs 900/3600 s (120 s on error), 1 MiB
response cap, 7 s timeout, User-Agent `LME-Shell/1.0 (desktop weather)`,
WMO code → label/icon table (28 codes).

### 4.4 `~/.config/lme-shell/palette.json` (read, watch)
Color token overrides for the Material/tonal theme; exact token list to be
inventoried in Step 2.1 when the power menu look is specified.

## 5. Power menu (full spec lands in Step 2.1)

Known today: actions above (§3.4) with **confirmation required for
destructive actions** (restart, shutdown; per README "Lock, suspend, logout,
restart, and shutdown actions with confirmation for destructive steps");
Esc closes; shown on the focused output only; look follows settings.json
(`opacity`, `accent`) + palette.json. QML reference: `qml/panels/PowerMenu.qml`.
Behavioral spec (labels, shortcuts, focus, layer/anchor) is written into
this section in Step 2.1, after which `lme-powermenu` implements it.

## 6. Application launcher (full spec lands in Step 2.2)

Known today: XDG data-dirs `.desktop` scan; entry fields
`id,name,icon,path,terminal,genericName,comment,keywords[]`; localized names
(`LC_*` resolution); `NoDisplay`/`Hidden`/`OnlyShowIn` filtering; launch
semantics §3.4 (no shell, Terminal handling, app must survive the client);
searchable + keyboard navigation; rescan cadence 60 s. No extra features
(frecency/plugins/calculator — ideas list in docs/, not implemented).
QML reference: `qml/panels/Launcher.qml`. Full behavioral spec (ranking,
key handling, icons, Actions) written in Step 2.2.

## 7. Explicitly out of Phase 2 scope
Polkit popup (security review required), lock screen/ext-session-lock, bar,
control center, notifications, tray, clipboard UI, settings app, and any
deletion/edit of the QML panels (they remain the default fallback).
