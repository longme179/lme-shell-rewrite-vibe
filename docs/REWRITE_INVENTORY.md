# Rewrite inventory — backend surface and callers

Phase 0 baseline for the native rewrite. This file freezes the *current*
wire contract so each Rust replacement can be parity-tested against it.
Everything below was extracted from `scripts/desktop_backend.py`,
`scripts/weather.py`, `scripts/charge_control.py`, `scripts/clipboard.py`,
`shell.qml`, `settings.qml`, and `qml/services/*.qml` at commit `ead4446`.

Two sibling documents: `docs/REWRITE_PLAN.md` (what migrates, in what order)
and `bench/RESULTS.md` (measured baseline numbers).

## 1. Process topology today

```
quickshell -c lme-shell                       (QML UI, 1 process)
├── python3 -u scripts/desktop_backend.py     (state daemon, JSON lines on stdio)
│   ├── niri msg --json event-stream          (persistent child; only used as
│   │                                          a "something changed" trigger)
│   └── ~10 short-lived subprocesses per 2 s poll cycle (see §4)
├── python3 scripts/clipboard.py              (clipboard daemon, JSON lines on stdio)
│   ├── wl-paste --no-newline --type text  --watch python3 clipboard.py --capture text
│   └── wl-paste --no-newline --type image --watch python3 clipboard.py --capture image
│       (each clipboard event spawns a full python3 process: --capture mode)
├── AuthenticationAgent (in-process QML, polkit D-Bus agent; lazily created)
├── scripts/settings.sh → quickshell settings.qml   (on demand, own backend
│                                                      instance with --settings)
└── awww / awww-daemon (external wallpaper tools)
```

Per-output: the QML `Variants { model: Quickshell.screens }` instantiates one
`PanelWindow` + full `Bar` tree per monitor. Overlay/notification-toast/OSD
windows are single instances anchored to `ShellState.panelScreen`.

## 2. `desktop_backend.py` — stdio JSON-lines protocol

Output: one **complete state snapshot** per line
(`json.dumps(..., ensure_ascii=False, separators=(",", ":"))`, no NaN).
Input: one JSON object per line, `{"action": "...", ...}`; every input line
produces exactly one snapshot emit carrying an updated `lastResult`.

### 2.1 Snapshot shape (live mode)

| Field | Type | Produced by | Notes |
|---|---|---|---|
| `apps` | array | `.desktop` scan (`Applications.scan`) | rescan every 60 s or on `refresh`; entry: `id,name,icon,path,terminal,genericName,comment,keywords[]` (argv hidden) |
| `niri` | object | `niri msg --json workspaces` + `windows` subprocesses | `{available, workspaces[], windows[]}`; workspaces sorted by (output, idx, id); raw niri JSON passed through |
| `system` | object | probes below, cached 1.5 s | see §2.2 |
| `weather` | object | `WeatherClient.snapshot` | see §2.4; network fetch disabled in `--once` and `--settings` |
| `preferences` | object | `settings.json` merge | defaults + validated keys; reloaded on mtime/size/inode change |
| `lastResult` | object | action handler | `{ok, action, message[, pending], sequence}`; `sequence` monotonic |

Settings-app mode (`--settings`): `apps: []`, `system: {}`, `niri` frozen at
startup value, weather allowed, only whitelisted actions accepted.

### 2.2 `system` probe inventory

| Key | Source | Mechanism | Cadence |
|---|---|---|---|
| `cpu` | `/proc/stat` first line | two-sample delta, `round(...,1)`, null on first sample | each probe window |
| `memoryPercent/UsedGiB/TotalGiB` | `/proc/meminfo` | MemTotal−MemAvailable, GiB rounded 2 | each probe window |
| `tempC` | `/sys/class/hwmon/hwmon*/temp*_input` (k10temp/zenpower/coretemp, Tctl/Tdie/Package) then `/sys/class/thermal/thermal_zone*` (x86_pkg_temp/cpu-thermal/acpitz) | read, /1000, clamp −20..150 | each probe window |
| `battery` | `/sys/class/power_supply/*/type == Battery` | mean capacity, any status==Charging | each probe window |
| `network` | `nmcli radio wifi`; `nmcli -t -f TYPE,STATE,CONNECTION device status`; `nmcli -t -f ACTIVE,SSID,SIGNAL device wifi list --rescan no` | 3 subprocesses; custom `:`-escape splitter; top 12 networks sorted (connected first, −strength) | each probe window |
| `bluetooth` | `busctl --system --json=short call org.bluez / ... GetManagedObjects` + `/sys/class/rfkill/rfkill*` | adapters (`/org/bluez/hciN` regex), preferred adapter from prefs, paired/connected devices under adapter path; `bluetoothctl show` fallback | each probe window |
| `audio` / `microphone` | `wpctl get-volume @DEFAULT_AUDIO_SINK@` / `@DEFAULT_AUDIO_SOURCE@` | regex `Volume: ([0-9.]+)`, `[MUTED]`, ×100 rounded | each probe window |
| `brightness` | `brightnessctl --class=backlight -m` | regex `,(\d+)%,` | each probe window |
| `powerProfile` | `busctl --system get-property .../ActiveProfile` (two service names) → `powerprofilesctl get` fallback | provider cached after first success | each probe window |
| `charging` | `charge_control.py` sysfs discovery: `charge_control_end_threshold` → `charge_types` (Standard/Long Life) → ideapad `VPC2004:*/conservation_mode` | `{available,kind,mode,limitPercent,supports85,writable,reason,canSet,busy}`; `canSet` also requires pkexec+trusted helper (`/usr/local/libexec/lme-shell-charge`, root-owned 0755 path check) | each probe window |
| `recording` | `/proc/*/exe` scan for wf-recorder / gpu-screen-recorder owned by uid, non-zombie | `{active,name,elapsed}` max elapsed | each probe window |

System cache invalidation: any action in
`{refresh, volume, mute, micVolume, micMute, brightness, wifiToggle,
bluetoothToggle, bluetoothDevice, powerProfile, chargeMode}` clears the cache.

### 2.3 Actions (input lines)

| Action | Payload | Implementation | QML callers |
|---|---|---|---|
| `refresh` | — | app rescan + state refresh; `--settings` mode: prefs reload only | DeviceControls “Refresh”, IpcHandler `refresh()` |
| `launch` | `{id}` | desktop-entry argv; `Terminal=true` → configured terminal `-e`; else `gio launch <path>` (spawn argv fallback) | Launcher |
| `setting` | `{key, value}` | `Preferences.set` → validate + flock + atomic write (see §2.5) | all settings pages, ControlDashboard tiles, ActivityCard, DeviceControls |
| `wallpaper` | `{path}` | validate image ext; `awww query` (start `awww-daemon`, poll ≤4 s) then `awww img <path> [--transition-type fade\|none]`; saves preference | WallpaperPage (drag & drop) |
| `workspace` | `{id}` | resolve against cached niri workspaces (refresh once if stale); `niri msg action focus-monitor <output>` + `focus-workspace <idx>` | BarModule workspaces, Overview |
| `focusWindow` | `{id}` | `niri msg action focus-window --id <id>` | Overview, TasksWidget |
| `moveWindow` | `{id, workspaceId}` | validate both against fresh `niri_state()`, then **NIRI_SOCKET** unix socket `{"Action":{"MoveWindowToWorkspace":{window_id, reference:{Id}, focus:false}}}`, reply must be `Ok` | Overview board drag/drop |
| `nativeOverview` | — | `niri msg action toggle-overview` | ActivityButton, BarModule overview |
| `volume` / `micVolume` | `{value}` 0–100 | `wpctl set-volume --limit 1.0 @DEFAULT_AUDIO_[SINK\|SOURCE]@ n%` | ControlDashboard sliders (debounced 100 ms) |
| `mute` / `micMute` | — | `wpctl set-mute ... toggle` | ControlDashboard, media keys via niri kdl |
| `brightness` | `{value}` 1–100 | `brightnessctl --class=backlight set n%` | ControlDashboard slider |
| `wifiToggle` | — | read `nmcli radio wifi`, invert | ControlDashboard, DeviceControls |
| `bluetoothToggle` | — | BlueZ `Powered` property via busctl (fallback `bluetoothctl power`); soft-block recovery via `rfkill unblock`; 3×120 ms confirmation poll; hard-block → error string | ControlDashboard, DeviceControls |
| `bluetoothDevice` | `{path, connected}` | path regex `/org/bluez/hciN/dev_XX_..`; `busctl --system call org.bluez <path> org.bluez.Device1 Connect\|Disconnect` | DeviceControls |
| `powerProfile` | `{value}` | set-property on cached provider (busctl, 30 s timeout) or `powerprofilesctl set` | ControlDashboard dropdown |
| `chargeMode` | `{value: 85\|100}` | background thread: sysfs write if writable, else `pkexec --disable-internal-agent /usr/bin/python3 -I /usr/local/libexec/lme-shell-charge <n>`; verify kernel echo; result arrives on a later snapshot as `lastResult{pending→done}` | ChargingWidget |
| `weatherSearch` | `{query}` | background geocoding (`geocoding-api.open-meteo.com`, 6 results) | WeatherPage search field |
| `weatherDetect` | — | background `ipwho.is` lookup | WeatherPage |
| `weatherRefresh` | — | force forecast (+geo if automatic) | WeatherWidget refresh |
| `weatherSelect` | `{location}` | validate location, set prefs `weatherLocation`, `weatherAutoLocation=false` | WeatherPage results |
| `lock` | — | spawn `swaylock --color 22212c` | PowerMenu, ControlDashboard |
| `screenshot` / `screenshotArea` | — | sleep 0.2 s; `niri msg action screenshot-screen\|screenshot` | ControlDashboard |
| `logout` | — | `niri msg action quit --skip-confirmation` | PowerMenu |
| `suspend` / `reboot` / `poweroff` | — | `systemctl suspend\|reboot\|poweroff` | PowerMenu |
| `openSettings` | `{target: network\|bluetooth\|audio}` | spawn `nm-connection-editor` / `blueman-manager` / `pavucontrol` | DeviceControls, ControlDashboard |

Error contract: any failure → `lastResult = {ok:false, action, message:
str(exc)[:400], sequence}`; the QML shows it as a status toast. Malformed
JSON input → `lastResult{ok:false, action:"", message:"Invalid JSON input"}`;
input line >64 KiB → “Input exceeds 64 KiB limit”.

### 2.4 `weather` snapshot shape

```
{temperature, feelsLike, humidity, wind, condition, icon, daily[3]{date,min,max,rainChance},
 unit:"°C"|"°F", windUnit:"km/h"|"mph", updatedAt,
 available, configured, automatic, location{name,country,latitude,longitude},
 loading, stale, error, search{query,loading,results[],error}}
```
TTLs: forecast 900 s (120 s on error), geo-IP 3600 s (120 s on error);
single background worker thread; providers: `api.open-meteo.com` (CC BY 4.0
attribution shown in UI), `ipwho.is`; 1 MiB response cap, 7 s timeout,
User-Agent `LME-Shell/1.0 (desktop weather)`; WMO weather_code → label+icon
table (28 codes); location validation rounds lat/lon to 6 decimals.

### 2.5 `preferences` file contract

Path: `$XDG_CONFIG_HOME/lme-shell/settings.json` (dir mode 0700, file 0600).
Write path: `settings.json.lock` flock (exclusive) → force reload → merge →
temp file `.settings-*.json` (0600, fsync) → `os.replace` → stat stamp
`(mtime_ns, size, inode)`. JSON layout: `indent=2` + trailing newline, key
order = defaults order then new keys (insertion-ordered dict).

Keys and validation (invalid saved keys are ignored, not fatal):
`language:"en"`, `animations:bool`, `opacity:0.60–1`, `accent:#rrggbb`,
`wallpaper:""|absolute existing file`, `terminal:[A-Za-z0-9_.+-]{1,80}`,
`bluetoothAdapter:""|/org/bluez/hci[0-9]+`, `tasks[]{id,text≤240,done}≤100`,
`weatherAutoLocation:bool`, `weatherLocation{}`, `weatherUnit:c|f`,
`activityEnabled:bool`, `clipboardEnabled:bool`, `clipboardImages:bool`,
`clipboardLimit:20–500`, `barStyle:islands|connected`, `barFontSize:12–16`,
`barLayout{left,center,right}` unique module ids from the 15 known modules.

### 2.6 CLI flags

`--once` (read-only snapshot, no weather fetch, exit), `--demo` (fixed sample
state, all mutations simulated, never persists), `--settings` (prefs+wallpaper
only), `--restore-wallpaper FILE` (apply with awww, no save, then exit),
`--interval SECONDS` (min 0.5, default 2.0).

### 2.7 Environment

`NIRI_SOCKET` (niri IPC unix socket; only used by `moveWindow` today),
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_DATA_DIRS`, `XDG_CURRENT_DESKTOP`
(defaults `niri`), `LC_ALL/LC_MESSAGES/LANG` (desktop-entry localization,
`LC_ALL=C` forced for subprocesses).

## 3. `clipboard.py` — stdio JSON-lines protocol

Output state (on change only; 0.25 s poll): `{entries[], paused, capturing,
available, total, revision, error, lastResult}`. Entry: `{id(sha256), kind,
mime, preview, created, pinned, bytes, image(file URI or "")}`.

Actions: `search{query,kind}`, `copy{id}` (wl-copy `--type mime`, 4 s),
`pin{id}` (max 20 pins), `delete{id}`, `clear{includePinned}`, `pause{value}`.

Storage: `$XDG_STATE_HOME/lme-shell/clipboard/history.sqlite3` (sqlite3,
mode 0600) — tables `entries(id PK, kind, mime, data BLOB, preview, search,
suffix, created, pinned)`, `meta(key,value)` with `revision`, `paused`;
trim to limit/64 MiB total keeping pins; images as `images/<id>.<ext>`.
Capture: single-owner `capture.lock` flock; `wl-paste --watch` re-executes
`clipboard.py --capture <kind>` **per clipboard event** (a fresh python3
process each time); `MAX_TEXT` 1 MiB, `MAX_IMAGE` 8 MiB; image sniffing
PNG/JPEG/GIF/BMP/WEBP; text preview = first 240 chars, search = casefolded.
Preferences (`clipboardEnabled/Images/Limit`) re-read from settings.json
every 1 s; umask 077.

## 4. Measured subprocess budget (stub-tool harness, see `bench/`)

Per 2 s poll cycle at idle (all tools present, niri available): **10
short-lived subprocesses + 1 persistent event-stream child**:

```
niri msg --json workspaces          niri msg --json windows
nmcli radio wifi                    nmcli -t -f TYPE,STATE,CONNECTION device status
nmcli -t -f ACTIVE,SSID,SIGNAL device wifi list --rescan no
busctl --system --json=short call org.bluez / … GetManagedObjects
wpctl get-volume @DEFAULT_AUDIO_SINK@     wpctl get-volume @DEFAULT_AUDIO_SOURCE@
brightnessctl --class=backlight -m         powerprofilesctl get   (after provider probe)
```
(first cycle additionally probes both power-profile D-Bus names once).
Clipboard event adds one `python3 clipboard.py --capture <kind>` process
(~13.7 MB transient RSS each). App rescan every 60 s re-parses every
`.desktop` file (~300 on a full Fedora install).

## 5. Quickshell-internal surfaces (stay in QML through Phase 1)

- `IpcHandler target "shell"`: `toggle(panel)`, `close()`, `refresh()`,
  `authenticationStatus()` — used by `config/niri/lme-shell.kdl` bindings.
- `Quickshell.Services.Notifications` server (history, DND, actions, expiry).
- `Quickshell.Services.Mpris` (players, metadata, artwork).
- `Quickshell.Services.SystemTray` (items + native menus).
- Polkit agent (`qml/auth/`): `org.freedesktop.PolicyKit1.AuthenticationAgent`,
  register at session bus, password dialog, cancellation.
- `Quickshell.Wayland` `IdleInhibitor` (Stay awake), layer-shell windows,
  `WlrLayershell` namespaces: `lme-bar`, `lme-overlay`, `lme-notifications`,
  `lme-osd`, auth window.
- Palette file watch: `$XDG_CONFIG_HOME/lme-shell/palette.json`.

## 6. Files and D-Bus names the shell owns or touches

| Path / name | Access |
|---|---|
| `~/.config/lme-shell/settings.json` (+`.lock`) | read/write, atomic |
| `~/.config/lme-shell/palette.json` | read, watch |
| `~/.local/state/lme-shell/clipboard/history.sqlite3` (+`images/`, `capture.lock`) | read/write |
| `/usr/local/libexec/lme-shell-charge` | root-owned helper, executed via pkexec (trusted path check: all parents root, no group/world write) |
| `org.bluez` (system bus) | GetManagedObjects, Adapter1.Powered, Device1.Connect/Disconnect |
| `org.freedesktop.UPower.PowerProfiles` / `net.hadess.PowerProfiles` | ActiveProfile get/set |
| `org.freedesktop.Notifications` (session) | via Quickshell |
| `org.freedesktop.PolicyKit1` (session) | auth agent |
| MPRIS `org.mpris.MediaPlayer2.*` (session) | via Quickshell |
| `NIRI_SOCKET` | Action requests (only MoveWindowToWorkspace today) |

## 7. Contract freeze rules for the rewrite

1. JSON-lines on stdio, one snapshot per line, same field names and shapes.
2. `lastResult.sequence` monotonic; ack for every input line before any
   background result replaces it (charge mode ordering guarantee).
3. Same error message strings (QML surfaces them verbatim; tests assert some).
4. `settings.json` and `history.sqlite3` stay read-compatible in both
   directions (Python fallback must keep working against Rust-written state).
5. Any intentional deviation gets a version note in `docs/REWRITE_PLAN.md`
   **before** the QML changes.
