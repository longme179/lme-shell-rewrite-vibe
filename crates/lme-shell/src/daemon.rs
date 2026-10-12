// SPDX-License-Identifier: GPL-3.0-or-later
//! The lme-shell daemon (ADR 0002): one calloop loop; Wayland surfaces,
//! IPC socket, inotify settings watch, clock timer, battery probe and the
//! niri event stream are all event sources on it.

use crate::bar;
use crate::ipc::{self, Command, Reply};
use crate::launcher_data::{self, EmojiIndex};
use crate::niri::NiriService;
use crate::panels::{self, ids, LauncherPanel, OsdPanel, PowerMenuPanel};
use crate::power::{DryRunPower, LogindPower, PowerExecutor};
use crate::settings::SettingsService;
use lme_backend::apps::{desktop_exec, Applications};
use lme_ui::theme::Theme;
use lme_ui::wayland::{AppHandler, KeyboardInteractivity, Layer, OutputRec, UiState, WindowSpec};
use lme_ui::widgets::{KeyInput, UiEvent, Widget, XKB_ESCAPE};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Animator track for a panel reveal animation (window id offset).
pub const REVEAL_TRACK: u64 = 0x900_000;

/// Reveal tween track for a window (panels share this helper).
pub fn reveal_track(window_id: u64) -> u64 {
    REVEAL_TRACK + window_id
}

pub struct Shell {
    pub theme: Theme,
    pub settings: SettingsService,
    pub niri: NiriService,
    pub apps: Vec<Value>,
    pub applications: Applications,
    pub emoji: EmojiIndex,
    /// system probe snapshot ({battery, network, audio, ...}) — fake-injected
    /// in tests via LME_SHELL_FAKE_STATE, live-probed (sysfs, no subprocess)
    /// otherwise.
    pub system_snapshot: serde_json::Map<String, Value>,
    pub media_title: Option<String>,
    pub tray_items: usize,
    /// Tray registry (Tier 1.11) + the open panel window, its menu page
    /// state and the D-Bus manager handle (None without a session bus).
    pub tray: crate::tray::TrayHub,
    pub tray_handle: Option<crate::tray::TrayHandle>,
    pub tray_panel: Option<u64>,
    pub tray_menu_open: Option<String>,
    pub power: Box<dyn PowerExecutor>,
    /// Notification hub (Tier 1.9): live toasts + history + unread.
    pub notifications: crate::notifications::NotificationHub,
    /// lme-notifications toast window (Overlay, top-right).
    pub toasts: Option<u64>,
    /// Sender for D-Bus signal emission (None when the session bus is
    /// unavailable — external notifications disabled, local ones still work).
    pub dbus_cmd: Option<std::sync::mpsc::Sender<crate::notifications::DbusCommand>>,
    /// id counter for local (msg notify) notifications — far from the D-Bus
    /// counter so the ranges never collide.
    pub next_local_id: u32,
    pub bars: HashMap<String, u64>,
    pub launcher: Option<u64>,
    pub power_menu: Option<u64>,
    pub osd: Option<(u64, Instant)>,
    /// Dry-run volume preview shown by the audio-pill wheel (finding e);
    /// re-seeded from the system snapshot when the preview goes stale.
    volume_preview: f32,
    volume_preview_at: Option<Instant>,
    /// Sub-notch workspace wheel remainder (high-res wheels send ±0.5).
    ws_wheel_acc: f32,
    pub dry_run: bool,
    pub started: Instant,
}

impl Shell {
    pub fn new(dry_run: bool) -> Shell {
        let settings = SettingsService::load();
        let palette = settings.palette;
        let theme = Theme::from_prefs(&settings.values, &palette);
        let mut applications = Applications::new(None);
        let apps = applications.scan();
        let emoji_path = repo_asset_path("assets/emoji.json");
        let emoji = EmojiIndex::load(&emoji_path);
        let system_snapshot = probe_system();
        Shell {
            theme,
            settings,
            niri: NiriService::new(),
            apps,
            applications,
            emoji,
            system_snapshot,
            media_title: None,
            tray_items: 0,
            tray: crate::tray::TrayHub::default(),
            tray_handle: None,
            tray_panel: None,
            tray_menu_open: None,
            power: {
                let executor: Box<dyn PowerExecutor> = if dry_run {
                    Box::new(DryRunPower::default())
                } else {
                    Box::new(LogindPower)
                };
                executor
            },
            notifications: crate::notifications::NotificationHub::default(),
            toasts: None,
            dbus_cmd: None,
            next_local_id: 0x4000_0000,
            bars: HashMap::new(),
            launcher: None,
            power_menu: None,
            osd: None,
            volume_preview: 0.0,
            volume_preview_at: None,
            ws_wheel_acc: 0.0,
            dry_run,
            started: Instant::now(),
        }
    }

    /// Rebuild the theme after settings/palette changes and refresh windows.
    pub fn refresh_theme(&mut self) {
        let palette = self.settings.palette;
        self.theme = Theme::from_prefs(&self.settings.values, &palette);
    }

    pub fn rescan_apps(&mut self) {
        self.apps = self.applications.scan();
    }
}

/// The emoji index ships with the repository; resolve relative to the
/// manifest so it also works from the release export.
fn repo_asset_path(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Battery from sysfs only (no subprocess): /sys/class/power_supply/*/type ==
/// Battery → mean capacity + any charging. Everything else stays unavailable
/// until its Tier 2 service lands (honest placeholders).
fn probe_system() -> serde_json::Map<String, Value> {
    let mut snapshot = serde_json::Map::new();
    if std::env::var_os("LME_SHELL_FAKE_STATE").is_some() {
        if let Ok(text) = std::env::var("LME_SHELL_FAKE_STATE_SYSTEM") {
            if let Ok(Value::Object(system)) = serde_json::from_str::<Value>(&text) {
                return system;
            }
        }
        return snapshot;
    }
    let mut percents: Vec<f64> = Vec::new();
    let mut charging = false;
    if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
        let mut supplies: Vec<_> = entries.flatten().collect();
        supplies.sort_by_key(|e| e.file_name());
        for supply in supplies {
            let base = supply.path();
            let is_battery = std::fs::read_to_string(base.join("type"))
                .map(|t| t.trim() == "Battery")
                .unwrap_or(false);
            if !is_battery {
                continue;
            }
            if let Ok(cap) = std::fs::read_to_string(base.join("capacity")) {
                if let Ok(p) = cap.trim().parse::<f64>() {
                    percents.push(p);
                }
            }
            if let Ok(status) = std::fs::read_to_string(base.join("status")) {
                if status.trim() == "Charging" {
                    charging = true;
                }
            }
        }
    }
    if !percents.is_empty() {
        let mean = percents.iter().sum::<f64>() / percents.len() as f64;
        snapshot.insert(
            "battery".into(),
            serde_json::json!({"available": true, "percent": mean, "charging": charging}),
        );
    }
    snapshot
}

/// Clock label: "ddd, MMM d  ·  HH:mm" (compact "HH:mm") — BarModule.qml.
pub fn clock_label(compact: bool) -> String {
    let now = chrono_like_now();
    if compact {
        format!("{:02}:{:02}", now.3, now.4)
    } else {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{}, {} {}  ·  {:02}:{:02}",
            DAYS[now.0 as usize],
            now.2,
            MONTHS[(now.1 - 1) as usize],
            now.3,
            now.4
        )
    }
}

/// (weekday0-6, month1-12, day, hour, minute) from the system clock
/// (UTC-independent civil time via libc localtime; no chrono dependency).
fn chrono_like_now() -> (i32, i32, i32, i32, i32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = localtime_offset_secs(secs);
    let adjusted = secs + local;
    let days = adjusted.div_euclid(86400);
    let tod = adjusted.rem_euclid(86400);
    let (hour, minute) = (tod / 3600, (tod % 3600) / 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
    let _ = year;
    (
        weekday as i32,
        m as i32,
        d as i32,
        hour as i32,
        minute as i32,
    )
}

/// Read the local UTC offset for a timestamp from /etc/localtime semantics.
/// The sandbox has no tz database, so fall back to the TZ env or UTC.
fn localtime_offset_secs(_unix: i64) -> i64 {
    if let Ok(tz) = std::env::var("TZ") {
        // support the fixed forms used in tests: "UTC", "UTC-7", "<+07>-7"
        if let Some(rest) = tz.strip_prefix("UTC") {
            if let Ok(hours) = rest.parse::<i64>() {
                return -hours * 3600;
            }
        }
    }
    0
}

impl AppHandler for Shell {
    fn output_added(&mut self, state: &mut UiState, output: &OutputRec) {
        let id = bar::create_bar(state, self, output);
        self.bars.insert(output.name.clone(), id);
    }

    fn output_removed(&mut self, state: &mut UiState, name: &str) {
        if let Some(id) = self.bars.remove(name) {
            state.close_window(id);
        }
    }

    fn window_closed(&mut self, _state: &mut UiState, window_id: u64) {
        if self.launcher == Some(window_id) {
            self.launcher = None;
        }
        if self.power_menu == Some(window_id) {
            self.power_menu = None;
        }
        if let Some((osd_id, _)) = self.osd {
            if osd_id == window_id {
                self.osd = None;
            }
        }
    }

    fn ui_event(&mut self, state: &mut UiState, window_id: u64, ev: UiEvent) {
        match ev {
            UiEvent::Click(id) => self.handle_click(state, window_id, id),
            UiEvent::Wheel(id, dy) => self.handle_wheel(state, window_id, id, dy),
            UiEvent::Dismiss => {
                // outside-click on a panel's backdrop (CenteredPanel/TrayPopup)
                if self.launcher == Some(window_id)
                    || self.power_menu == Some(window_id)
                    || self.tray_panel == Some(window_id)
                {
                    self.close_panels(state);
                }
            }
            // launcher search field changed: recompute the result list
            // (the query-change signal the panel emits on every keystroke)
            UiEvent::Slide(panels::ids::LAUNCHER_INPUT, _) => {
                if self.launcher == Some(window_id) {
                    self.refresh_launcher(state);
                }
            }
            UiEvent::Submit(_, _) | UiEvent::Slide(_, _) | UiEvent::Toggle(_, _) => {}
            UiEvent::Focus(_) => {}
        }
    }

    fn key(&mut self, state: &mut UiState, window_id: u64, key: &KeyInput) {
        if key.sym == XKB_ESCAPE {
            // Esc closes any open panel (QML closePanels).
            if self.launcher == Some(window_id)
                || self.power_menu == Some(window_id)
                || self.tray_panel == Some(window_id)
            {
                self.close_panels(state);
            }
        }
    }

    fn tick(&mut self, state: &mut UiState) {
        // Stop frame callbacks once a window's reveal tween has completed.
        let ids: Vec<u64> = state.window_ids();
        for id in ids {
            let animating = state.window(id).map(|w| w.animating).unwrap_or(false);
            if animating && state.anims.finished(REVEAL_TRACK + id) {
                if let Some(w) = state.window(id) {
                    w.animating = false;
                }
            }
        }
    }

    fn theme(&self) -> &Theme {
        &self.theme
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Recover the concrete Shell from the handler box during re-entrant calls.
pub fn with_shell<R>(state: &mut UiState, f: impl FnOnce(&mut Shell, &mut UiState) -> R) -> R {
    state.with_shell(|handler, state| {
        let shell = handler
            .as_any_mut()
            .downcast_mut::<Shell>()
            .expect("daemon handler is Shell");
        f(shell, state)
    })
}

impl Shell {
    fn handle_click(&mut self, state: &mut UiState, window_id: u64, id: u64) {
        // toast interactions (dismiss ×, action pills)
        if self.handle_toast_click(state, id) {
            return;
        }
        // power menu buttons
        if self.power_menu == Some(window_id) {
            match id {
                ids::POWER_LOCK => {
                    let _ = self.power.lock();
                    self.close_panels(state);
                }
                ids::POWER_CLOSE | ids::POWER_BACK => {
                    self.close_panels(state);
                }
                ids::POWER_SUSPEND => self.confirm_power(state, "suspend", "Suspend"),
                ids::POWER_LOGOUT => self.confirm_power(state, "logout", "Log out"),
                ids::POWER_REBOOT => self.confirm_power(state, "reboot", "Restart"),
                ids::POWER_OFF => self.confirm_power(state, "poweroff", "Shut down"),
                ids::POWER_CONFIRM => {
                    let pending = self.pending_power(state);
                    if let Some((action, _)) = pending {
                        let result = match action.as_str() {
                            "suspend" => self.power.suspend(),
                            "logout" => self.power.logout(),
                            "reboot" => self.power.reboot(),
                            "poweroff" => self.power.poweroff(),
                            _ => Ok(String::new()),
                        };
                        if let Err(e) = result {
                            self.show_osd(state, e, -1.0);
                        }
                    }
                    self.close_panels(state);
                }
                _ => {}
            }
            return;
        }
        // launcher activation (Enter)
        if self.launcher == Some(window_id) && id == ids::LAUNCHER_LIST {
            self.activate_launcher_selection(state);
            return;
        }
        // bar modules
        match id {
            bar::ids::LAUNCHER => self.toggle_panel(state, "launcher"),
            bar::ids::OVERVIEW => {
                let _ = crate::spawn_detached(&["niri", "msg", "action", "toggle-overview"]);
            }
            bar::ids::NOTIFICATIONS => {
                // opening the (Tier-2) center also marks them read (legacy
                // markRead-on-open); for now the center is a notice
                self.toggle_panel(state, "notifications");
                self.mark_notifications_read(state);
            }
            bar::ids::SETTINGS => {
                let _ = crate::spawn_detached(&["lme-shell", "settings"]);
            }
            bar::ids::CLIPBOARD => {
                self.show_osd(
                    state,
                    "Clipboard panel is not implemented in this build",
                    -1.0,
                );
            }
            bar::ids::AUDIO | bar::ids::BATTERY => {
                self.toggle_panel(state, "control");
            }
            bar::ids::NETWORK => {
                // legacy opens the Wi-Fi panel from the network module
                self.toggle_panel(state, "wifi");
            }
            bar::ids::TRAY => {
                self.open_tray_panel(state);
            }
            // tray panel widget ids (Tier 1.11) — the id space
            // [0x7100_0000_0000, 0x7400_0000_0000) is reserved for the tray
            // (toast ids live below 0x7100, toast action ids at 0x7400+)
            id if (crate::tray::ids::TRAY_ROW_ACT..0x7400_0000_0000).contains(&id) => {
                self.handle_tray_click(state, id);
            }
            id if id >= bar::ids::WORKSPACE_BASE => {
                let ws_id = (id - bar::ids::WORKSPACE_BASE) as i64;
                match self.niri.focus_workspace(ws_id) {
                    Ok(m) => self.show_osd(state, m, -1.0),
                    Err(e) => self.show_osd(state, e, -1.0),
                }
            }
            _ => {}
        }
    }

    /// Wheel-over-bar-module behaviors (Amendment 3 finding e — NEW
    /// behavior, no legacy equivalent):
    /// - workspaces row: focus the previous/next workspace per notch
    /// - audio pill: preview volume ±5% per notch, dry-run (no audio service
    ///   yet — the OSD is the only effect; README documents this)
    fn handle_wheel(&mut self, state: &mut UiState, window_id: u64, id: u64, dy: f32) {
        // tray rows: forward the notch to the item's Scroll handler
        if (crate::tray::ids::TRAY_ROW_WHEEL..0x7200_0000_0000).contains(&id) {
            self.handle_tray_wheel(id, dy);
            return;
        }
        match id {
            bar::ids::WORKSPACES => {
                self.ws_wheel_acc += dy;
                let steps = self.ws_wheel_acc.trunc();
                self.ws_wheel_acc -= steps;
                let steps = steps as i32;
                if steps == 0 {
                    return;
                }
                // which bar (per-output workspace row) was wheeled?
                let output = self
                    .bars
                    .iter()
                    .find(|(_, w)| **w == window_id)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();
                let workspaces = self.niri.workspaces_for(&output);
                let Some(target) = workspace_focus_target(&workspaces, steps) else {
                    return;
                };
                match self.niri.focus_workspace(target) {
                    Ok(_) => {} // silent on success (no OSD spam while scrolling)
                    Err(e) => eprintln!("lme-shell: workspace wheel: {e}"),
                }
            }
            bar::ids::AUDIO => {
                // re-seed the preview when the last wheel is stale
                let fresh = self
                    .volume_preview_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(2500));
                if !fresh {
                    self.volume_preview = self
                        .system_snapshot
                        .get("audio")
                        .and_then(|v| v.get("volume"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0) as f32;
                }
                self.volume_preview = (self.volume_preview + 5.0 * dy).clamp(0.0, 100.0);
                self.volume_preview_at = Some(Instant::now());
                self.show_osd(state, "Volume (dry-run)", self.volume_preview);
            }
            _ => {}
        }
    }

    fn pending_power(&mut self, state: &mut UiState) -> Option<(String, String)> {
        let window_id = self.power_menu?;
        state
            .with_window(window_id, |window, _ctx| {
                window
                    .root()
                    .as_any()
                    .downcast_ref::<PowerMenuPanel>()
                    .and_then(|p| p.pending.map(|(a, l)| (a.to_string(), l.to_string())))
            })
            .flatten()
    }

    fn confirm_power(&mut self, state: &mut UiState, action: &'static str, label: &'static str) {
        if let Some(window_id) = self.power_menu {
            let theme =
                lme_ui::theme::Theme::from_prefs(&self.settings.values, &self.settings.palette);
            state.with_window(window_id, |window, _ctx| {
                if let Some(panel) = window.root().as_any().downcast_mut::<PowerMenuPanel>() {
                    panel.request_confirmation(&theme, action, label);
                }
            });
            state.request_render(window_id);
        }
    }

    fn activate_launcher_selection(&mut self, state: &mut UiState) {
        let Some(window_id) = self.launcher else {
            return;
        };
        let selection = state
            .with_window(window_id, |window, _ctx| {
                window
                    .root()
                    .as_any()
                    .downcast_mut::<panels::CenteredPanel>()
                    .and_then(|c| c.panel_mut().as_any().downcast_mut::<LauncherPanel>())
                    .and_then(|panel| panel.selected_result().cloned())
            })
            .flatten();
        let Some(result) = selection else { return };
        match result.kind {
            launcher_data::ResultKind::App { id } => {
                let outcome = self.launch_app(&id);
                match outcome {
                    Ok(m) => {
                        if !m.is_empty() {
                            self.show_osd(state, m, -1.0);
                        }
                    }
                    Err(e) => self.show_osd(state, e, -1.0),
                }
            }
            launcher_data::ResultKind::Copy { value } => {
                let _ = crate::spawn_detached(&["wl-copy", "--type", "text/plain", &value]);
                self.close_panels(state);
            }
            launcher_data::ResultKind::Window { window_id: wid } => {
                let _ = crate::spawn_detached(&[
                    "niri",
                    "msg",
                    "action",
                    "focus-window",
                    "--id",
                    &wid.to_string(),
                ]);
                self.close_panels(state);
            }
        }
        if self.launcher.is_some() {
            self.close_panels(state);
        }
    }

    /// Launch a scanned app by id (legacy `launch` semantics: argv spawn,
    /// Terminal=true → configured terminal -e, gio fallback).
    pub fn launch_app(&mut self, id: &str) -> Result<String, String> {
        let Some(entry) = self.applications.entries.get(id) else {
            return Err(format!("Unknown application: {id}"));
        };
        let get = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let terminal = get("Terminal").to_lowercase() == "true";
        let argv = desktop_exec(&get("Exec"), &get("Name"), &get("Icon"), &get("Path"))?;
        let terminal_pref = self
            .settings
            .values
            .get("terminal")
            .and_then(Value::as_str)
            .unwrap_or("kitty")
            .to_string();
        if terminal {
            let mut full = vec![terminal_pref, "-e".to_string()];
            full.extend(argv);
            crate::spawn_detached(&full.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
        } else {
            if crate::spawn_detached(&argv.iter().map(|s| s.as_str()).collect::<Vec<_>>()).is_err()
            {
                crate::spawn_detached(&["gio", "launch", &get("Path")])?;
            }
        }
        Ok(String::new())
    }

    // ---- panel management (legacy ShellState.togglePanel) ----

    pub fn toggle_panel(&mut self, state: &mut UiState, panel: &str) {
        match panel {
            "launcher" => {
                if self.launcher.is_some() {
                    self.close_panels(state);
                } else {
                    self.close_panels(state);
                    self.open_launcher(state);
                }
            }
            "power" => {
                if self.power_menu.is_some() {
                    self.close_panels(state);
                } else {
                    self.close_panels(state);
                    self.open_power_menu(state);
                }
            }
            "notifications" | "control" | "clipboard" | "overview" | "activity" | "wifi"
            | "settings" => {
                self.show_osd(
                    state,
                    format!("The {panel} panel is not implemented in this build"),
                    -1.0,
                );
            }
            "tray" => {
                // Tier 1.11 tray panel (empty → still opens with the
                // "No tray applications" row, like legacy)
                self.open_tray_panel(state);
            }
            _ => {}
        }
    }

    fn open_power_menu(&mut self, state: &mut UiState) {
        let output = state.focused_output();
        let (w, h) = PowerMenuPanel::size();
        let mut spec = WindowSpec::full_overlay("lme-power", KeyboardInteractivity::Exclusive);
        spec.output = output.map(|o| o.output);
        spec.background = lme_ui::Color::rgba(0.0, 0.0, 0.0, 0.40);
        let id = state.create_window(spec);
        let panel = PowerMenuPanel::new(&self.theme);
        let track = REVEAL_TRACK + id;
        state.set_root(
            id,
            Box::new(panels::CenteredPanel::new(
                Box::new(panel),
                w as f32,
                h as f32,
                track,
            )),
        );
        self.start_reveal(state, id);
        self.power_menu = Some(id);
    }

    fn open_launcher(&mut self, state: &mut UiState) {
        let output = state.focused_output();
        let (w, h) = LauncherPanel::size();
        let mut spec = WindowSpec::full_overlay("lme-launcher", KeyboardInteractivity::Exclusive);
        spec.output = output.map(|o| o.output);
        spec.background = lme_ui::Color::rgba(0.0, 0.0, 0.0, 0.40);
        let id = state.create_window(spec);
        let panel = LauncherPanel::new(&self.theme);
        let track = REVEAL_TRACK + id;
        state.set_root(
            id,
            Box::new(panels::CenteredPanel::new(
                Box::new(panel),
                w as f32,
                h as f32,
                track,
            )),
        );
        // populate the initial result list (empty query = ranked app list);
        // found by the Step-0a pointer tests — before this the launcher
        // opened with zero results until an unrelated niri/settings event
        // happened to fire refresh_launcher
        self.launcher = Some(id);
        self.refresh_launcher(state);
        state.set_keyboard_focus(id, Some(ids::LAUNCHER_INPUT));
        self.start_reveal(state, id);
    }

    pub(crate) fn start_reveal(&mut self, state: &mut UiState, window_id: u64) {
        let track = REVEAL_TRACK + window_id;
        let dur = self.theme.slide_duration_ms;
        let c = self.theme.spatial_curve;
        let curve = lme_ui::anim::Curve {
            x1: c[0],
            y1: c[1],
            x2: c[2],
            y2: c[3],
        };
        state.anims.tween(track, 1.0, dur, curve, Instant::now());
        if let Some(w) = state.window(window_id) {
            w.animating = true;
        }
        state.request_frame(window_id);
    }

    pub fn close_panels(&mut self, state: &mut UiState) {
        if let Some(id) = self.launcher.take() {
            state.close_window(id);
        }
        if let Some(id) = self.power_menu.take() {
            state.close_window(id);
        }
        if self.tray_panel.is_some() {
            self.close_tray_panel(state);
        }
    }

    pub fn show_osd(&mut self, state: &mut UiState, message: impl Into<String>, value: f32) {
        let message = message.into();
        if let Some((id, _)) = self.osd {
            state.close_window(id);
            self.osd = None;
        }
        let panel = OsdPanel::new(message, value);
        let (w, h) = panel.size();
        let spec = WindowSpec {
            namespace: "lme-osd".into(),
            layer: Layer::Overlay,
            anchor: (false, false, true, false),
            exclusive_zone: -1,
            size: Some((w, h)),
            keyboard: KeyboardInteractivity::None,
            margins: (0, 0, 24, 0),
            output: state.focused_output().map(|o| o.output),
            background: lme_ui::Color::rgba(0.0, 0.0, 0.0, 0.0),
        };
        let id = state.create_window(spec);
        state.set_root(id, Box::new(panel));
        state.request_render(id);
        self.osd = Some((id, Instant::now() + Duration::from_millis(1800)));
        schedule_osd_expiry(state);
    }

    /// Rebuild every bar's widget tree (clock tick, state change, settings).
    pub fn rebuild_bars(&mut self, state: &mut UiState) {
        let names: Vec<(String, u64)> = self
            .bars
            .iter()
            .map(|(name, id)| (name.clone(), *id))
            .collect();
        for (name, id) in names {
            let width = state.window(id).map(|w| w.size.0 as f32).unwrap_or(1920.0);
            let root = bar::build_bar_root(self, &name, width);
            state.set_root(id, root);
        }
    }
}

impl Shell {
    /// Launcher query changed: recompute results against current shell data.
    pub fn refresh_launcher(&mut self, state: &mut UiState) {
        let Some(id) = self.launcher else { return };
        let favorites: Vec<String> = self
            .settings
            .values
            .get("favoriteApps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let apps = self.apps.clone();
        let windows = self.niri.state().get("windows").cloned();
        let emoji = self.emoji.clone(); // Arc-backed: cheap
        state.with_window(id, |window, _ctx| {
            let Some(centered) = window
                .root()
                .as_any()
                .downcast_mut::<panels::CenteredPanel>()
            else {
                return;
            };
            let Some(panel) = centered
                .panel_mut()
                .as_any()
                .downcast_mut::<LauncherPanel>()
            else {
                return;
            };
            panel.update_results(&panels::LauncherData {
                apps: &apps,
                favorites: &favorites,
                windows: windows.as_ref(),
                emoji: &emoji,
            });
        });
        state.request_render(id);
    }
}

impl Shell {
    pub fn handle_ipc(&mut self, state: &mut UiState, cmd: Command) -> Reply {
        match cmd {
            Command::Ping => Reply::ok("pong"),
            Command::Status => {
                let panels_open: Vec<&str> = [
                    ("launcher", self.launcher.is_some()),
                    ("power", self.power_menu.is_some()),
                ]
                .into_iter()
                .filter(|(_, open)| *open)
                .map(|(name, _)| name)
                .collect();
                let battery_available = self
                    .system_snapshot
                    .get("battery")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let detail = serde_json::json!({
                    "version": VERSION,
                    "uptimeSec": self.started.elapsed().as_secs(),
                    "outputs": self.bars.len(),
                    "panelsOpen": panels_open,
                    "dryRun": self.dry_run,
                    "apps": self.apps.len(),
                    "niriAvailable": self.niri.available(),
                    "batteryAvailable": battery_available,
                });
                Reply {
                    ok: true,
                    message: "running".into(),
                    detail: Some(detail),
                }
            }
            Command::Toggle { panel } => {
                self.toggle_panel(state, &panel);
                Reply::ok(format!("toggled {panel}"))
            }
            Command::Close => {
                self.close_panels(state);
                Reply::ok("closed")
            }
            Command::Refresh => {
                self.rescan_apps();
                self.niri.refresh();
                self.system_snapshot = probe_system();
                self.rebuild_bars(state);
                Reply::ok(format!("refreshed ({} apps)", self.apps.len()))
            }
            Command::Osd { label, value } => {
                self.show_osd(state, label, value);
                Reply::ok("osd shown")
            }
            Command::Notify { summary, body } => {
                self.local_notification(state, summary, body);
                Reply::ok("notified")
            }
            Command::Dnd { minutes } => {
                let until: i64 = if minutes < 0 {
                    -1
                } else if minutes == 0 {
                    0
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64 + minutes * 60_000)
                        .unwrap_or(minutes)
                };
                match self.settings.set("dndUntil", Value::from(until)) {
                    Ok(()) => Reply::ok(format!("dnd until {until}")),
                    Err(e) => Reply {
                        ok: false,
                        message: e,
                        detail: None,
                    },
                }
            }
            Command::Notifications => Reply {
                ok: true,
                message: format!(
                    "{} live, {} history, {} unread",
                    self.notifications.live.len(),
                    self.notifications.history.len(),
                    self.notifications.unread
                ),
                detail: Some(crate::notifications::history_json(&self.notifications)),
            },
            Command::Tray => Reply {
                ok: true,
                message: format!("{} tray items", self.tray.items.len()),
                detail: Some(self.tray.json()),
            },
            Command::StatusMessage { message } => {
                self.show_osd(state, message, -1.0);
                Reply::ok("status shown")
            }
            Command::Power { action } => match action.as_str() {
                "lock" => reply_result(self.power.lock()),
                "logout" => reply_result(self.power.logout()),
                "suspend" => reply_result(self.power.suspend()),
                "reboot" => reply_result(self.power.reboot()),
                "poweroff" => reply_result(self.power.poweroff()),
                _ => Reply::err(format!("Unknown power action: {action}")),
            },
        }
    }
}

fn reply_result(result: Result<String, String>) -> Reply {
    match result {
        Ok(m) => Reply::ok(m),
        Err(e) => Reply::err(e),
    }
}

/// Next minute boundary from now.
pub fn next_minute() -> Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() + local_offset_now_secs();
    let into_minute = secs % 60;
    let wait = 60 - into_minute + 1;
    Duration::from_secs(wait)
}

fn local_offset_now_secs() -> u64 {
    0
}

// ---------------------------------------------------------------------------
// Event loop assembly: every service is a calloop event source (ADR 0002).
// ---------------------------------------------------------------------------

pub fn run(dry_run: bool) -> std::process::ExitCode {
    let shell = Shell::new(dry_run);
    let mut state = match UiState::connect(Box::new(shell)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lme-shell: Wayland connection failed: {e}");
            eprintln!("  (start inside a niri session, or set WAYLAND_DISPLAY)");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut event_loop = match calloop::EventLoop::<UiState>::try_new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("lme-shell: event loop: {e:?}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let handle = event_loop.handle();
    if let Err(e) = state.insert_sources(&handle) {
        eprintln!("lme-shell: wayland sources: {e}");
        return std::process::ExitCode::FAILURE;
    }
    // The loop handle is stored for shell-side one-shot timers (OSD expiry).
    state.handle = Some(handle.clone());

    // ---- IPC socket ----
    use std::os::unix::net::UnixListener;
    let socket = ipc::socket_path();
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("lme-shell: cannot bind {}: {e}", socket.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let _ = listener.set_nonblocking(true);
    let (ipc_tx, ipc_channel) = calloop::channel::channel::<(Command, std_mpsc::Sender<Reply>)>();
    let ipc_tx_accept = ipc_tx.clone();
    {
        use calloop::generic::Generic;
        use calloop::{Interest, Mode};
        let source = Generic::new(listener, Interest::READ, Mode::Level);
        let insert = handle.insert_source(source, move |_readiness, listener, _state| {
            while let Ok((stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                ipc::serve_connection(stream, ipc_tx_accept.clone());
            }
            Ok(calloop::PostAction::Continue)
        });
        if let Err(e) = insert {
            eprintln!("lme-shell: ipc source: {e:?}");
            return std::process::ExitCode::FAILURE;
        }
    }
    if let Err(e) = handle.insert_source(ipc_channel, |event, _, state| {
        if let calloop::channel::Event::Msg((cmd, reply_tx)) = event {
            crate::daemon::with_shell(state, |shell, state| {
                let reply = shell.handle_ipc(state, cmd);
                let _ = reply_tx.send(reply);
            });
        }
    }) {
        eprintln!("lme-shell: ipc channel: {e:?}");
        return std::process::ExitCode::FAILURE;
    }

    // ---- notification D-Bus server (Tier 1.9) ----
    // Non-fatal when the session bus is absent (D4): the shell then runs
    // with local notifications only (msg notify).
    {
        let (ntx, nchannel) = calloop::channel::channel::<crate::notifications::DbusEvent>();
        match crate::notifications::spawn(ntx) {
            Ok(_dbus) => {
                crate::daemon::with_shell(&mut state, |shell, _| {
                    shell.dbus_cmd = Some(_dbus.cmd_tx);
                });
                if let Err(e) = handle.insert_source(nchannel, |event, _, state| {
                    if let calloop::channel::Event::Msg(ev) = event {
                        crate::daemon::with_shell(state, |shell, state| {
                            shell.notification_event(state, ev);
                        });
                    }
                }) {
                    eprintln!("lme-shell: notification channel: {e:?}");
                }
            }
            Err(e) => {
                eprintln!("lme-shell: notifications: session bus unavailable ({e})");
            }
        }
    }

    // ---- tray StatusNotifierWatcher (Tier 1.11) ----
    // Non-fatal without a session bus (D4): the bar hides the tray button.
    {
        let (ttx, tchannel) = calloop::channel::channel::<crate::tray::TrayEvent>();
        match crate::tray::spawn(ttx) {
            Ok(thandle) => {
                crate::daemon::with_shell(&mut state, |shell, _| {
                    shell.tray_handle = Some(thandle);
                });
                if let Err(e) = handle.insert_source(tchannel, |event, _, state| {
                    if let calloop::channel::Event::Msg(ev) = event {
                        crate::daemon::with_shell(state, |shell, state| {
                            shell.tray_event(state, ev);
                        });
                    }
                }) {
                    eprintln!("lme-shell: tray channel: {e:?}");
                }
            }
            Err(e) => {
                eprintln!("lme-shell: tray: session bus unavailable ({e})");
            }
        }
    }

    // ---- settings/palette hot reload (inotify) ----
    {
        use calloop::generic::Generic;
        use calloop::{Interest, Mode};
        struct InotifyCell {
            fd: std::os::fd::RawFd,
            inner: std::cell::RefCell<inotify::Inotify>,
        }
        impl InotifyCell {
            fn new(inotify: inotify::Inotify) -> Self {
                use std::os::fd::AsRawFd;
                let fd = inotify.as_raw_fd();
                Self {
                    fd,
                    inner: std::cell::RefCell::new(inotify),
                }
            }
            fn read<'a>(&self, buffer: &'a mut [u8]) -> std::io::Result<inotify::Events<'a>> {
                self.inner.borrow_mut().read_events(buffer)
            }
        }
        // SAFETY: the fd belongs to the Inotify stored in `inner`, which
        // lives exactly as long as `self`; calloop only polls it.
        impl std::os::fd::AsFd for InotifyCell {
            fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
                unsafe { std::os::fd::BorrowedFd::borrow_raw(self.fd) }
            }
        }
        let inotify = match inotify::Inotify::init() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("lme-shell: inotify: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };
        let config_dir = settings_dir();
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = inotify.watches().add(
            &config_dir,
            inotify::WatchMask::CLOSE_WRITE
                | inotify::WatchMask::MOVED_TO
                | inotify::WatchMask::CREATE,
        );
        let source = Generic::new(InotifyCell::new(inotify), Interest::READ, Mode::Level);
        if let Err(e) = handle.insert_source(source, |_r, inotify, state| {
            let mut buffer = [0u8; 4096];
            let mut settings_changed = false;
            let mut palette_changed = false;
            if let Ok(events) = inotify.read(&mut buffer) {
                for event in events {
                    let name = event.name.unwrap_or_default().to_string_lossy().to_string();
                    if name == "settings.json" {
                        settings_changed = true;
                    } else if name == "palette.json" {
                        palette_changed = true;
                    }
                }
            }
            if settings_changed || palette_changed {
                crate::daemon::with_shell(state, |shell, state| {
                    let s_changed = shell.settings.reload_settings();
                    let p_changed = shell.settings.reload_palette();
                    if s_changed || p_changed {
                        shell.refresh_theme();
                        shell.rebuild_bars(state);
                        if shell.launcher.is_some() {
                            shell.refresh_launcher(state);
                        }
                    }
                });
            }
            Ok(calloop::PostAction::Continue)
        }) {
            eprintln!("lme-shell: inotify source: {e:?}");
            return std::process::ExitCode::FAILURE;
        }
    }

    // ---- clock: minute-boundary one-shot chain ----
    {
        let handle_clock = handle.clone();
        let arm = |handle: &calloop::LoopHandle<'static, UiState>| {
            let timer = calloop::timer::Timer::from_duration(next_minute());
            let _ = handle.insert_source(timer, |_, _, state| {
                crate::daemon::with_shell(state, |shell, state| {
                    shell.rebuild_bars(state);
                });
                calloop::timer::TimeoutAction::Drop
            });
        };
        arm(&handle_clock);
    }

    // ---- battery probe: 30 s chain (sysfs only, no subprocess) ----
    {
        let handle_battery = handle.clone();
        let _ = handle_battery;
        let timer = calloop::timer::Timer::from_duration(Duration::from_secs(30));
        let _ = handle.insert_source(timer, |_, _, state| {
            crate::daemon::with_shell(state, |shell, state| {
                shell.system_snapshot = probe_system();
                shell.rebuild_bars(state);
            });
            // re-arm: chained one-shot, so an idle shell without a battery
            // still only wakes once per 30 s
            let timer = calloop::timer::Timer::from_duration(Duration::from_secs(30));
            if let Some(handle) = &state.handle {
                let _ = handle.insert_source(timer, |_, _, state| {
                    crate::daemon::with_shell(state, |shell, state| {
                        shell.system_snapshot = probe_system();
                        shell.rebuild_bars(state);
                    });
                    calloop::timer::TimeoutAction::Drop
                });
            }
            calloop::timer::TimeoutAction::Drop
        });
    }

    // ---- niri event stream (change signal → state refresh) ----
    {
        let (niri_tx, niri_channel) = calloop::channel::channel::<()>();
        {
            // Bridge the niri notify thread (std mpsc) into the calloop loop.
            let (std_tx, std_rx) = std_mpsc::channel::<()>();
            std::thread::spawn(move || {
                while let Ok(()) = std_rx.recv() {
                    if niri_tx.send(()).is_err() {
                        break;
                    }
                }
            });
            crate::daemon::with_shell(&mut state, |shell, _state| {
                shell.niri.start_stream(std_tx);
            });
        }
        if let Err(e) = handle.insert_source(niri_channel, |event, _, state| {
            if matches!(event, calloop::channel::Event::Msg(())) {
                crate::daemon::with_shell(state, |shell, state| {
                    if shell.niri.refresh() {
                        shell.rebuild_bars(state);
                        if shell.launcher.is_some() {
                            shell.refresh_launcher(state);
                        }
                    }
                });
            }
        }) {
            eprintln!("lme-shell: niri channel: {e:?}");
            return std::process::ExitCode::FAILURE;
        }
    }

    eprintln!(
        "lme-shell {} daemon ready (socket {}, dry-run={dry_run})",
        VERSION,
        socket.display()
    );

    // ---- run ----
    loop {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_secs(3600)), &mut state) {
            // A dispatch error usually means the compositor is gone; exit
            // cleanly so the session manager can restart us.
            eprintln!("lme-shell: loop error: {e:?}");
            break;
        }
        if state.exit {
            break;
        }
    }
    let _ = std::fs::remove_file(&socket);
    std::process::ExitCode::SUCCESS
}

fn settings_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from(".config"))
        .join("lme-shell")
}

/// Called by show_osd via the stored loop handle: schedule expiry.
pub fn schedule_osd_expiry(state: &mut UiState) {
    let Some(handle) = state.handle.clone() else {
        return;
    };
    let timer = calloop::timer::Timer::from_duration(Duration::from_millis(1800));
    let _ = handle.insert_source(timer, |_, _, state| {
        let osd_id =
            crate::daemon::with_shell(state, |shell, _state| shell.osd.take().map(|(id, _)| id));
        if let Some(id) = osd_id {
            state.close_window(id);
        }
        calloop::timer::TimeoutAction::Drop
    });
}

/// Workspace to focus after `steps` wheel notches over the workspaces row
/// (positive = down = next workspace). Pure function so the target math is
/// unit-testable without a Wayland connection. `workspaces` is the same
/// per-output, sort-key-ordered list the bar renders.
pub fn workspace_focus_target(workspaces: &[Value], steps: i32) -> Option<i64> {
    if workspaces.len() < 2 || steps == 0 {
        return None;
    }
    let focused = workspaces
        .iter()
        .position(|ws| {
            ws.get("is_focused")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .unwrap_or(0);
    let target = (focused as i32 + steps).clamp(0, workspaces.len() as i32 - 1) as usize;
    workspaces
        .get(target)
        .and_then(|ws| ws.get("id"))
        .and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspaces() -> Vec<Value> {
        serde_json::json!([
            {"id": 1, "idx": 1, "is_focused": true, "output": "mock-1"},
            {"id": 2, "idx": 2, "is_focused": false, "output": "mock-1"},
            {"id": 3, "idx": 3, "is_focused": false, "output": "mock-1"}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    #[test]
    fn wheel_down_moves_to_next_workspace() {
        assert_eq!(workspace_focus_target(&workspaces(), 1), Some(2));
    }

    #[test]
    fn wheel_up_clamps_at_first() {
        assert_eq!(workspace_focus_target(&workspaces(), -1), Some(1));
    }

    #[test]
    fn wheel_many_clamps_at_last() {
        assert_eq!(workspace_focus_target(&workspaces(), 9), Some(3));
    }

    #[test]
    fn wheel_zero_or_single_workspace_is_none() {
        assert_eq!(workspace_focus_target(&workspaces(), 0), None);
        assert_eq!(workspace_focus_target(&workspaces()[..1], 5), None);
    }
}
