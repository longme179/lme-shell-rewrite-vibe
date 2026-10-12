// SPDX-License-Identifier: GPL-3.0-or-later
//! The lme-shell daemon (ADR 0002): one calloop loop; Wayland surfaces,
//! IPC socket, inotify settings watch, clock timer, battery probe and the
//! niri event stream are all event sources on it.

use std::path::PathBuf;

use crate::bar;
use crate::click_ids;
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
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant, SystemTime};

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
    /// Live CPU/memory sampler (keeps the last /proc/stat sample for the
    /// delta math; first read reports cpu: null).
    pub system_probes: lme_backend::probes::SystemProbes,
    pub media_title: Option<String>,
    /// MPRIS players (org.mpris.MediaPlayer2.*) — bar media module feed.
    pub mpris: Vec<crate::mpris::MprisPlayer>,
    pub mpris_handle: Option<crate::mpris::MprisHandle>,
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
    /// Control center window id (Tier 2.1).
    pub control: Option<u64>,
    /// Selected control center tab (legacy ShellState.controlTab).
    pub control_tab: usize,
    /// Hardware actions executor (Tier 2.2; dry-run under --dry-run).
    pub actions: ControlActions,
    /// Debounced slider values (legacy pendingVolume/pendingBrightness,
    /// flushed 100 ms after the last move).
    pub pending_volume: Option<i64>,
    pub pending_brightness: Option<i64>,
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
        let mut system_probes = lme_backend::probes::SystemProbes::new(
            lme_backend::runner::CommandRunner::new(),
            PathBuf::from("/proc"),
            PathBuf::from("/sys"),
        );
        let system_snapshot = probe_system(&mut system_probes);
        Shell {
            theme,
            settings,
            niri: NiriService::new(),
            apps,
            applications,
            emoji,
            system_snapshot,
            system_probes,
            media_title: None,
            mpris: Vec::new(),
            mpris_handle: None,
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
            control: None,
            control_tab: 0,
            actions: ControlActions { dry_run },
            pending_volume: None,
            pending_brightness: None,
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
fn probe_system(probes: &mut lme_backend::probes::SystemProbes) -> serde_json::Map<String, Value> {
    let mut snapshot = serde_json::Map::new();
    if std::env::var_os("LME_SHELL_FAKE_STATE").is_some() {
        // The harness passes a PATH to a json file (it used to be passed as
        // inline json — the from_str(env) silently failed forever and the
        // goldens absorbed the empty state; found via the Tier 2.1 control
        // center showing "Unavailable" despite fake data).
        if let Some(path) = std::env::var_os("LME_SHELL_FAKE_STATE_SYSTEM") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(Value::Object(system)) = serde_json::from_str::<Value>(&text) {
                    return system;
                }
            }
        }
        return snapshot;
    }
    // CPU/memory deltas (needs two samples: the first is cpu: null)
    for (k, v) in probes.cpu_memory() {
        snapshot.insert(k, v);
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
/// "Day, Mon d" for the control center clock row (Controls tab header).
pub fn clock_date_label() -> String {
    let now = chrono_like_now();
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {} {}",
        DAYS[now.0 as usize],
        now.2,
        MONTHS[(now.1 - 1) as usize],
    )
}

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
        if self.control == Some(window_id) {
            self.control = None;
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
            UiEvent::Slide(crate::control_center::ids::VOL_SLIDER, value) => {
                // Tier 2.2: debounced flush (legacy pendingVolume + 100 ms)
                self.volume_preview = value.clamp(0.0, 100.0);
                self.volume_preview_at = Some(Instant::now());
                self.pending_volume = Some(value.round() as i64);
                self.show_osd(state, format!("Volume {}%", value.round() as i64), value);
                schedule_slider_flush(state, "volume");
            }
            UiEvent::Slide(crate::control_center::ids::BRIGHT_SLIDER, value) => {
                self.pending_brightness = Some((value.round() as i64).max(1));
                self.show_osd(
                    state,
                    format!("Brightness {}%", value.round() as i64),
                    value,
                );
                schedule_slider_flush(state, "brightness");
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
                || self.control == Some(window_id)
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
        // power menu buttons — bounded to the registered power range so a
        // foreign id can never route into these arms (D23 protection)
        if self.power_menu == Some(window_id) {
            if !click_ids::POWER_PANEL.contains(id) {
                return;
            }
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
        // launcher activation (Enter) — bounded to the launcher range
        // (D23 protection); only the list widget activates
        if self.launcher == Some(window_id) && click_ids::LAUNCHER_PANEL.contains(id) {
            if id == ids::LAUNCHER_LIST {
                self.activate_launcher_selection(state);
            }
            return;
        }
        // control center widgets — bounded to the registered range (D23)
        if self.control == Some(window_id) {
            if !click_ids::CONTROL_PANEL.contains(id) {
                return;
            }
            self.handle_control_click(state, id);
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
            // tray panel widget ids (Tier 1.11) — registry-bounded ranges
            // (click_ids; D23: open-ended checks are forbidden)
            id if click_ids::TRAY_ROWS.contains(id) || click_ids::TRAY_MENU.contains(id) => {
                self.handle_tray_click(state, id);
            }
            id if click_ids::WORKSPACE_BUTTONS.contains(id) => {
                let ws_id = (id - bar::ids::WORKSPACE_BASE) as i64;
                match self.niri.focus_workspace(ws_id) {
                    Ok(m) => self.show_osd(state, m, -1.0),
                    Err(e) => self.show_osd(state, e, -1.0),
                }
            }
            _ => {
                // Registry diagnostic (D23 guard): an id that belongs to a
                // registered range but reaches this arm is a routing bug —
                // surface it so e2e logs catch the class D23 came from.
                if let Some(r) = click_ids::find(id) {
                    eprintln!(
                        "lme-shell: click id {id:#x} in registered range {:?} but unhandled",
                        r.name
                    );
                }
            }
        }
    }

    /// Wheel-over-bar-module behaviors (Amendment 3 finding e — NEW
    /// behavior, no legacy equivalent):
    /// - workspaces row: focus the previous/next workspace per notch
    /// - audio pill: preview volume ±5% per notch, dry-run (no audio service
    ///   yet — the OSD is the only effect; README documents this)
    fn handle_wheel(&mut self, state: &mut UiState, window_id: u64, id: u64, dy: f32) {
        // tray rows: forward the notch to the item's Scroll handler (wheel
        // events always carry the TRAY_ROW_WHEEL sub-block id; the sub-base
        // ladder inside the registered TRAY_ROWS range is asserted by the
        // click_ids unit test)
        if click_ids::TRAY_ROWS.contains(id) && id >= crate::tray::ids::TRAY_ROW_WHEEL {
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
            // other bar pills have no wheel behavior — swallow (registry-
            // bounded, so future bar modules can't fall into foreign arms)
            id if click_ids::BAR_BUTTONS.contains(id) => {}
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

    /// Tier 2.1 control center clicks (ids pre-validated against the
    /// CONTROL_PANEL range by handle_click).
    fn handle_control_click(&mut self, state: &mut UiState, id: u64) {
        use crate::control_center::ids as cc;
        match id {
            cc::CLOSE => self.close_panels(state),
            cc::LOCK => {
                let _ = self.power.lock();
                self.close_panels(state);
            }
            cc::TILE_DND => {
                // legacy: dnd tile toggles forever (-1) / off (0)
                let next = if crate::notifications::dnd_active(&self.settings.values) {
                    0
                } else {
                    -1
                };
                if let Err(e) = self.settings.set("dndUntil", Value::from(next)) {
                    eprintln!("lme-shell: control: dnd: {e}");
                }
                self.refresh_control_center(state);
            }
            cc::TILE_MOTION => {
                let next = !self
                    .settings
                    .values
                    .get("animations")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if let Err(e) = self.settings.set("animations", Value::from(next)) {
                    eprintln!("lme-shell: control: animations: {e}");
                }
                self.refresh_theme();
                self.rebuild_bars(state);
                self.refresh_control_center(state);
            }
            cc::TILE_WIFI => self.show_osd(state, "Wi-Fi settings land with Tier 2.4", -1.0),
            cc::TILE_BLUETOOTH => {
                // Tier 2.3: probe-driven state; toggle runs the action
                let bt = self
                    .system_snapshot
                    .get("bluetooth")
                    .cloned()
                    .unwrap_or(Value::Null);
                let available = bt
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !available {
                    self.show_osd(state, "No Bluetooth adapter is available", -1.0);
                } else {
                    let enabled = bt.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                    let adapter = bt
                        .get("adapter")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.actions.bluetooth_power(!enabled, &adapter);
                    // optimistic flip (legacy perform_demo semantics)
                    if let Some(b) = self.system_snapshot.get_mut("bluetooth") {
                        b["enabled"] = Value::from(!enabled);
                    }
                    self.refresh_control_center(state);
                }
            }
            cc::VOL_MUTE => {
                // Tier 2.2: wpctl mute toggle + optimistic snapshot flip
                self.actions.mute();
                if let Some(audio) = self.system_snapshot.get_mut("audio") {
                    let muted = audio.get("muted").and_then(Value::as_bool).unwrap_or(false);
                    audio["muted"] = Value::from(!muted);
                }
                self.rebuild_bars(state);
                self.refresh_control_center(state);
            }
            id if (cc::PROFILE_BASE..cc::PROFILE_BASE + 3).contains(&id) => {
                self.show_osd(state, "Power-profile switching lands with Tier 2.5", -1.0)
            }
            cc::MEDIA_PREV | cc::MEDIA_PLAY | cc::MEDIA_NEXT => self.show_osd(
                state,
                "Media transport controls land with the activity island (Tier 2.9)",
                -1.0,
            ),
            id if (cc::TAB_BASE..cc::TAB_BASE + 6).contains(&id) => {
                self.control_tab = (id - cc::TAB_BASE) as usize;
                self.refresh_control_center(state);
            }
            _ => {}
        }
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
            "control" => {
                // Tier 2.1: the control center panel (bar audio/battery
                // pills route here too).
                if self.control.is_some() {
                    self.close_panels(state);
                } else {
                    self.close_panels(state);
                    self.open_control_center(state);
                }
            }
            "notifications" | "clipboard" | "overview" | "activity" | "wifi" | "settings" => {
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

    /// Tier 2.3: BlueZ adapter state for the control-center tile. Probed
    /// ONLY while the panel is open (rides the visibility-gated 5 s
    /// refresh; one busctl subprocess — PERF register). Without org.bluez
    /// on the bus this fails gracefully to "No adapter" (the e2e
    /// service-missing case).
    fn probe_bluetooth(&mut self) {
        let preferred = self
            .settings
            .values
            .get("bluetoothAdapter")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let state = self.system_probes.bluetooth(&preferred);
        self.system_snapshot.insert("bluetooth".to_string(), state);
    }

    fn open_control_center(&mut self, state: &mut UiState) {
        let output = state.focused_output();
        let (out_w, out_h) = output
            .as_ref()
            .map(|o| (o.logical.0, o.logical.1))
            .unwrap_or((1920, 1080));
        // legacy loader: width min(600, out-32), height min(900, out-42)
        let w = crate::control_center::PANEL_W
            .min(out_w as f32 - 32.0)
            .max(320.0);
        let h = 900.0_f32.min(out_h as f32 - 42.0).max(360.0);
        let mut spec = WindowSpec::full_overlay("lme-control", KeyboardInteractivity::Exclusive);
        spec.output = output.as_ref().map(|o| o.output.clone());
        // legacy scrim for side panels (0.13, vs 0.40 for centered ones)
        spec.background = lme_ui::Color::rgba(0.0, 0.0, 0.0, 0.13);
        let id = state.create_window(spec);
        self.probe_bluetooth();
        let data = crate::control_center::control_data(self);
        let panel =
            crate::control_center::ControlCenterPanel::new(&self.theme, &data, self.control_tab);
        let track = REVEAL_TRACK + id;
        state.set_root(
            id,
            Box::new(panels::SlidePanelRight::new(Box::new(panel), w, h, track)),
        );
        self.control = Some(id);
        self.start_reveal(state, id);
    }

    /// Rebuild the open control center from a fresh snapshot (called from
    /// the 5 s system chain and after settings changes — only while open,
    /// so no idle cost).
    pub fn refresh_control_center(&mut self, state: &mut UiState) {
        let Some(id) = self.control else { return };
        self.probe_bluetooth();
        let data = crate::control_center::control_data(self);
        let tab = self.control_tab;
        let panel = crate::control_center::ControlCenterPanel::new(&self.theme, &data, tab);
        let w = crate::control_center::PANEL_W;
        if let Some(win) = state.window(id) {
            let h = win.size.1 as f32 - 10.0;
            let track = REVEAL_TRACK + id;
            state.set_root(
                id,
                Box::new(panels::SlidePanelRight::new(Box::new(panel), w, h, track)),
            );
            state.request_render(id);
        }
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
        if let Some(id) = self.control.take() {
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
                    ("control", self.control.is_some()),
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
                self.system_snapshot = probe_system(&mut self.system_probes);
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
            Command::Media => Reply {
                ok: true,
                message: format!(
                    "{} player{}",
                    self.mpris.len(),
                    if self.mpris.len() == 1 { "" } else { "s" }
                ),
                detail: Some(serde_json::json!({
                    "count": self.mpris.len(),
                    "title": self.media_title,
                    "players": self.mpris.iter().map(|p| serde_json::json!({
                        "bus": p.bus,
                        "identity": p.identity,
                        "title": p.track_title,
                        "artist": p.artist_line(),
                        "status": p.status,
                        "playing": p.is_playing(),
                    })).collect::<Vec<_>>(),
                })),
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
    // with local notifications only (msg notify). LME_SHELL_NO_NOTIF=1
    // skips the module (D25 stall bisect knob).
    if std::env::var_os("LME_SHELL_NO_NOTIF").is_none() {
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
    } // LME_SHELL_NO_NOTIF gate

    // ---- tray StatusNotifierWatcher (Tier 1.11) ----
    // Non-fatal without a session bus (D4): the bar hides the tray button.
    // LME_SHELL_NO_TRAY=1 skips the module (D25 stall bisect knob).
    if std::env::var_os("LME_SHELL_NO_TRAY").is_none() {
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
    } // LME_SHELL_NO_TRAY gate

    // ---- MPRIS media feed (Tier 1.2) ----
    // Non-fatal without a session bus (D4): the media module stays hidden.
    {
        let (mtx, mchannel) = calloop::channel::channel::<crate::mpris::MprisEvent>();
        match crate::mpris::spawn(mtx) {
            Ok(mhandle) => {
                crate::daemon::with_shell(&mut state, |shell, _| {
                    shell.mpris_handle = Some(mhandle);
                });
                if let Err(e) = handle.insert_source(mchannel, |event, _, state| {
                    if let calloop::channel::Event::Msg(ev) = event {
                        crate::daemon::with_shell(state, |shell, state| {
                            shell.mpris_event(state, ev);
                        });
                    }
                }) {
                    eprintln!("lme-shell: mpris channel: {e:?}");
                }
            }
            Err(e) => {
                eprintln!("lme-shell: mpris: session bus unavailable ({e})");
            }
        }
    }

    // ---- settings/palette hot reload (inotify; 5 s mtime poll fallback) ----
    // Amendment 5 (hygiene 4): inotify_init can fail (EMFILE — observed
    // under heavy e2e parallelism) and previously KILLED the daemon at
    // startup. It must survive: log once, skip the watcher, and poll
    // settings/palette mtimes every 5 s instead (docs/PERF.md). The poll
    // path is also selectable explicitly via settings.json
    // "hotReload": "poll" (NFS homes, exhausted instance budget).
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
        let config_dir = settings_dir();
        let _ = std::fs::create_dir_all(&config_dir);
        let forced_poll = with_shell(&mut state, |shell, _| {
            shell
                .settings
                .values
                .get("hotReload")
                .and_then(Value::as_str)
                == Some("poll")
        });
        let inotify = if forced_poll {
            None
        } else {
            match inotify::Inotify::init() {
                Ok(i) => Some(i),
                Err(e) => {
                    eprintln!("lme-shell: inotify: {e}; hot reload degraded to a 5 s mtime poll");
                    None
                }
            }
        };
        match inotify {
            Some(inotify) => {
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
                        apply_config_reload(state);
                    }
                    Ok(calloop::PostAction::Continue)
                }) {
                    eprintln!(
                        "lme-shell: inotify source: {e:?}; hot reload degraded to a 5 s mtime poll"
                    );
                    insert_mtime_poll(&handle, &config_dir);
                }
            }
            None => insert_mtime_poll(&handle, &config_dir),
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

    // ---- system probe chain: battery (sysfs) + CPU/RAM (/proc), 5 s ----
    // (legacy backend sampled on demand with a 1.5 s TTL; a 5 s chain is
    // fresh enough for the resources pill and costs 0.2 wakeups/s idle,
    // well inside the ≤1/s budget. CPU% needs two samples — the first
    // tick reports null and the pill shows 0% until the second.)
    {
        let handle_system = handle.clone();
        let _ = handle_system;
        let timer = calloop::timer::Timer::from_duration(Duration::from_secs(5));
        let _ = handle.insert_source(timer, |_, _, state| {
            crate::daemon::with_shell(state, |shell, state| {
                shell.system_snapshot = probe_system(&mut shell.system_probes);
                shell.rebuild_bars(state);
                // Tier 2.1: refresh the control center while it is OPEN
                // (no new poll — rides the existing chain; PERF register).
                if shell.control.is_some() {
                    shell.refresh_control_center(state);
                }
            });
            // re-arm: chained one-shot, so an idle shell wakes once per 5 s
            let timer = calloop::timer::Timer::from_duration(Duration::from_secs(5));
            if let Some(handle) = &state.handle {
                let _ = handle.insert_source(timer, |_, _, state| {
                    crate::daemon::with_shell(state, |shell, state| {
                        shell.system_snapshot = probe_system(&mut shell.system_probes);
                        shell.rebuild_bars(state);
                        if shell.control.is_some() {
                            shell.refresh_control_center(state);
                        }
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

/// Control-center hardware actions (Tier 2.2): volume/mute via wpctl,
/// brightness via brightnessctl — same commands as lme-backend actions
/// (frozen contract), executed directly by the daemon (the backend
/// `perform` path would pull the whole DesktopBackend). Dry-run logs the
/// intent instead of spawning (the e2e asserts the log line).
pub struct ControlActions {
    pub dry_run: bool,
}

impl ControlActions {
    fn run(&self, label: &str, argv: &[&str]) {
        if self.dry_run {
            eprintln!("lme-shell: dry-run: {label} [{}]", argv.join(" "));
            return;
        }
        if let Err(e) = crate::power::spawn_detached(argv) {
            eprintln!("lme-shell: control: {label}: {e}");
        }
    }

    pub fn volume(&self, value: i64) {
        self.run(
            &format!("volume {value}%"),
            &[
                "wpctl",
                "set-volume",
                "--limit",
                "1.0",
                "@DEFAULT_AUDIO_SINK@",
                &format!("{value}%"),
            ],
        );
    }

    pub fn mute(&self) {
        self.run(
            "mute toggle",
            &["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"],
        );
    }

    pub fn brightness(&self, value: i64) {
        self.run(
            &format!("brightness {value}%"),
            &[
                "brightnessctl",
                "--class=backlight",
                "set",
                &format!("{value}%"),
            ],
        );
    }

    /// Tier 2.3: adapter power via busctl (set_bluetooth argv; falls back
    /// to bluetoothctl without an adapter path).
    pub fn bluetooth_power(&self, enabled: bool, adapter: &str) {
        let state = if enabled { "on" } else { "off" };
        if adapter.is_empty() {
            self.run(
                &format!("bluetooth power {state}"),
                &["bluetoothctl", "--timeout", "10", "power", state],
            );
        } else {
            self.run(
                &format!("bluetooth power {state} ({adapter})"),
                &[
                    "busctl",
                    "--system",
                    "set-property",
                    "org.bluez",
                    adapter,
                    "org.bluez.Adapter1",
                    "Powered",
                    "b",
                    if enabled { "true" } else { "false" },
                ],
            );
        }
    }
}

/// Apply a settings/palette reload (shared by the inotify watcher and the
/// 5 s mtime poll fallback — Amendment 5, hygiene 4).
fn apply_config_reload(state: &mut UiState) {
    with_shell(state, |shell, state| {
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

/// Mtime snapshot of (settings.json, palette.json) in a config dir.
fn config_mtimes(dir: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
    let mtime = |file: &str| {
        std::fs::metadata(dir.join(file))
            .and_then(|meta| meta.modified())
            .ok()
    };
    (mtime("settings.json"), mtime("palette.json"))
}

/// 5 s mtime poll fallback for settings/palette hot reload when inotify is
/// unavailable (EMFILE / exhausted instances — Amendment 5, hygiene 4: the
/// daemon must survive and keep hot reload working, listed in docs/PERF.md).
struct MtimePoll {
    last: (Option<SystemTime>, Option<SystemTime>),
}

impl MtimePoll {
    fn new(dir: &Path) -> Self {
        Self {
            last: config_mtimes(dir),
        }
    }

    /// True when settings.json or palette.json changed (mtime create,
    /// modify OR delete) since the previous call.
    fn changed(&mut self, dir: &Path) -> bool {
        let now = config_mtimes(dir);
        let changed = now != self.last;
        self.last = now;
        changed
    }
}

/// Install the repeating 5 s mtime-poll timer for config hot reload.
fn insert_mtime_poll(handle: &calloop::LoopHandle<'static, UiState>, config_dir: &Path) {
    let dir = config_dir.to_path_buf();
    let mut poll = MtimePoll::new(&dir);
    let timer = calloop::timer::Timer::from_duration(Duration::from_secs(5));
    let _ = handle.insert_source(timer, move |_, _, state| {
        if poll.changed(&dir) {
            apply_config_reload(state);
        }
        calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(5))
    });
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

/// Flush a debounced slider value 100 ms after the last move (legacy
/// volumeDelay/brightnessDelay timers; Tier 2.2).
pub fn schedule_slider_flush(state: &mut UiState, which: &'static str) {
    let Some(handle) = state.handle.clone() else {
        return;
    };
    let timer = calloop::timer::Timer::from_duration(Duration::from_millis(100));
    let _ = handle.insert_source(timer, move |_, _, state| {
        crate::daemon::with_shell(state, |shell, state| match which {
            "volume" => {
                if let Some(v) = shell.pending_volume.take() {
                    shell.actions.volume(v);
                    // optimistic snapshot update (the real probe is a
                    // deferred backend catch-up; keeps bar+panel live)
                    if let Some(audio) = shell
                        .system_snapshot
                        .entry("audio".to_string())
                        .or_insert_with(|| serde_json::json!({"available": true}))
                        .as_object_mut()
                    {
                        audio.insert("volume".into(), Value::from(v));
                        audio.insert("muted".into(), Value::from(false));
                    }
                    shell.rebuild_bars(state);
                    if shell.control.is_some() {
                        shell.refresh_control_center(state);
                    }
                }
            }
            _ => {
                if let Some(v) = shell.pending_brightness.take() {
                    shell.actions.brightness(v);
                    if let Some(bright) = shell
                        .system_snapshot
                        .entry("brightness".to_string())
                        .or_insert_with(|| serde_json::json!({}))
                        .as_object_mut()
                    {
                        bright.insert("available".into(), Value::from(true));
                        bright.insert("percent".into(), Value::from(v));
                    }
                    shell.rebuild_bars(state);
                    if shell.control.is_some() {
                        shell.refresh_control_center(state);
                    }
                }
            }
        });
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

    #[test]
    fn mtime_poll_detects_create_modify_delete() {
        // Amendment 5 (hygiene 4): the inotify-unavailable fallback must
        // see settings.json / palette.json create, modify AND delete.
        let dir = std::env::temp_dir().join(format!("lme-mtimepoll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");

        let mut poll = MtimePoll::new(&dir);
        assert!(!poll.changed(&dir), "no files yet: nothing changed");

        // create
        std::fs::write(&settings, "{}").unwrap();
        assert!(poll.changed(&dir), "create detected");
        assert!(!poll.changed(&dir), "stable: no change");

        // modify (mtime must advance; sleep past any fs timestamp grace)
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&settings, r#"{"barFontSize": 26}"#).unwrap();
        assert!(poll.changed(&dir), "modify detected");

        // second file (palette.json) counts too
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("palette.json"), "{}").unwrap();
        assert!(poll.changed(&dir), "palette create detected");

        // delete
        std::thread::sleep(Duration::from_millis(20));
        std::fs::remove_file(&settings).unwrap();
        assert!(poll.changed(&dir), "delete detected");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mtime_poll_ignores_other_files() {
        let dir = std::env::temp_dir().join(format!("lme-mtimepoll-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut poll = MtimePoll::new(&dir);
        std::fs::write(dir.join("unrelated.json"), "{}").unwrap();
        assert!(
            !poll.changed(&dir),
            "only settings.json / palette.json are watched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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
