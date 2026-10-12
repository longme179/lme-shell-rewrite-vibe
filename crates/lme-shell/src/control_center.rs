// SPDX-License-Identifier: GPL-3.0-or-later
//! Control center panel (Tier 2.1 — ControlCenter.qml + ControlDashboard.qml
//! port). One panel window, six tabs (Controls is fully rendered in this
//! tier; the others land with their service modules — see NATIVE_PLAN 2.x).
//! Rebuilt from a `ControlData` snapshot; the daemon refreshes it on the
//! existing 5 s system chain while the panel is OPEN (no new idle poll —
//! PERF register).

use lme_ui::color::Color;
use lme_ui::font_weight::{BOLD, MEDIUM, NORMAL};
use lme_ui::geom::{Rect, Size};
use lme_ui::theme::Theme;
use lme_ui::widgets::{
    Button, Ctx, FocusScope, HBox, InputEvent, Label, Slider, Spacer, VBox, Widget,
};

/// Panel width (legacy `controlWidth` default 600; the daemon clamps to
/// the output like the QML loader did).
pub const PANEL_W: f32 = 600.0;

pub mod ids {
    /// All ids live in the click_ids::CONTROL_PANEL range [0x8000, 0x8100).
    pub const CLOSE: u64 = 0x8001;
    pub const TAB_BASE: u64 = 0x8010; // +tab index (0..=5)
    pub const LOCK: u64 = 0x8020;
    pub const TILE_WIFI: u64 = 0x8030;
    pub const TILE_BLUETOOTH: u64 = 0x8031;
    pub const TILE_DND: u64 = 0x8032;
    pub const TILE_MOTION: u64 = 0x8033;
    pub const VOL_MUTE: u64 = 0x8040;
    pub const VOL_SLIDER: u64 = 0x8041;
    pub const BRIGHT_SLIDER: u64 = 0x8042;
    pub const PROFILE_BASE: u64 = 0x8050; // +0 power-saver, +1 balanced, +2 performance
    pub const MEDIA_PREV: u64 = 0x8060;
    pub const MEDIA_PLAY: u64 = 0x8061;
    pub const MEDIA_NEXT: u64 = 0x8062;

    pub fn tab(index: usize) -> u64 {
        TAB_BASE + index as u64
    }
    pub fn profile(index: usize) -> u64 {
        PROFILE_BASE + index as u64
    }
}

pub const TABS: [&str; 6] = [
    "Controls",
    "Audio",
    "Devices",
    "Agenda",
    "Notifications",
    "System",
];

/// Which NATIVE_PLAN row fills each tab (shown on the placeholder page).
const TAB_TIERS: [&str; 6] = [
    "",
    "Tier 2.2 (audio mixer)",
    "Tier 2.3/2.4 (Bluetooth, devices)",
    "Tier 2.7 (calendar and to-do)",
    "Tier 2.6 (notification history)",
    "Tier 2.5 (system monitor)",
];

/// Snapshot of everything the Controls tab renders (built by the daemon
/// from system_snapshot / settings / mpris; owned, so the widget tree is
/// self-contained between rebuilds).
pub struct ControlData {
    pub now_hm: String,
    pub now_date: String,
    pub dnd: bool,
    pub animations: bool,
    pub wifi: Option<WifiState>,
    pub bluetooth: Option<BluetoothState>,
    pub audio: Option<AudioState>,
    pub brightness: Option<f64>,
    pub power_profile: Option<String>,
    pub media: Option<MediaState>,
    pub cpu: Option<f64>,
    pub ram: Option<f64>,
    pub battery_percent: Option<f64>,
    pub battery_charging: bool,
}

#[derive(Clone)]
pub struct WifiState {
    pub connected: bool,
    pub enabled: bool,
    pub ssid: String,
}

#[derive(Clone)]
pub struct BluetoothState {
    pub available: bool,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct AudioState {
    pub volume: f64,
    pub muted: bool,
}

#[derive(Clone)]
pub struct MediaState {
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

/// The whole control-center panel (header + tabs + active tab content).
pub struct ControlCenterPanel {
    pub current_tab: usize,
    content: FocusScope,
    rect: Rect,
}

impl ControlCenterPanel {
    pub fn new(theme: &Theme, data: &ControlData, current_tab: usize) -> Self {
        let mut panel = Self {
            current_tab,
            content: FocusScope::new(Box::new(VBox::new(0.0)), false),
            rect: Rect::default(),
        };
        panel.rebuild(theme, data);
        panel
    }

    fn rebuild(&mut self, theme: &Theme, data: &ControlData) {
        let t = theme;
        let mut column = VBox::new(10.0).padded(14.0);
        // header (ControlCenter.qml)
        column = column.push(
            HBox::new(8.0)
                .push(
                    VBox::new(3.0)
                        .push(Label::styled("Control center", 23.0, BOLD, t.text))
                        .push(Label::styled(
                            "Your desktop, within reach.",
                            12.0,
                            NORMAL,
                            t.muted,
                        )),
                )
                .push(Spacer::fill())
                .push(close_button(t)),
        );
        // tab grid: 3 columns x 2 rows of buttons (active = accent, legacy
        // LiquidButton grid)
        let mut rows = VBox::new(5.0);
        for row in 0..2 {
            let mut hbox = HBox::new(5.0);
            for col in 0..3 {
                let index = row * 3 + col;
                hbox = hbox.push(tab_button(t, index, self.current_tab));
            }
            rows = rows.push(hbox);
        }
        column = column.push(rows);
        // tab content
        let body: Box<dyn Widget> = if self.current_tab == 0 {
            Box::new(controls_tab(t, data))
        } else {
            Box::new(placeholder_tab(t, self.current_tab))
        };
        column = column.push(body);
        self.content = FocusScope::new(Box::new(column), false);
    }
}

fn close_button(t: &Theme) -> Button {
    let mut b = Button::new(ids::CLOSE, "×");
    b.label_size = 16.0;
    b.label_weight = BOLD;
    b.fill = t.surface.alpha(0.55);
    b.hover_fill = Some(t.surface);
    b.text_color = t.text;
    b.radius = t.small_radius;
    b.border = Some((t.border, 1.0));
    b.min_h = 34.0;
    b.min_w = 34.0;
    b
}

fn tab_button(t: &Theme, index: usize, current: usize) -> Button {
    let active = index == current;
    let mut b = Button::new(ids::tab(index), TABS[index]);
    b.label_size = 13.0;
    b.label_weight = if active { BOLD } else { MEDIUM };
    // legacy: active = accent bg + background text; inactive = surface
    b.fill = if active { t.purple } else { t.surface };
    b.hover_fill = Some(if active {
        t.purple
    } else {
        t.surface_container_high
    });
    b.text_color = if active { t.background } else { t.text };
    b.radius = t.small_radius;
    b.border = Some((t.border, 1.0));
    b.min_h = 34.0;
    b
}

/// A Controls quick-toggle tile (legacy ControlDashboard `Tile`). The
/// native Button is single-line, so the tile label carries
/// "Label — detail" (the two-line tile body lands with the styling pass).
fn tile(t: &Theme, id: u64, label: &str, detail: &str, active: bool, accent: Color) -> Button {
    let mut b = Button::new(id, format!("{label} — {detail}"));
    b.label_size = 13.0;
    b.label_weight = BOLD;
    b.fill = if active {
        t.panel_fill.mix(accent, 0.30).alpha(t.panel_opacity + 0.08)
    } else {
        t.surface_container_high
    };
    b.hover_fill = Some(if active {
        t.panel_fill.mix(accent, 0.38).alpha(t.panel_opacity + 0.08)
    } else {
        t.surface_container_high
    });
    b.text_color = t.text;
    b.radius = if active { 20.0 } else { 14.0 };
    b.border = Some((if active { accent.alpha(0.48) } else { t.border }, 1.0));
    b.min_h = 56.0;
    b
}

/// Surface card container (legacy `Rectangle { radius; color: surface }`).
struct Card {
    body: Box<dyn Widget>,
    rect: Rect,
}

impl Card {
    fn new(body: Box<dyn Widget>) -> Self {
        Self {
            body,
            rect: Rect::default(),
        }
    }
}

impl Widget for Card {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.body.measure(ctx)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        self.body.layout(ctx, bounds);
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        out.push(lme_ui::draw::DrawCmd::RoundedRect {
            rect: self.rect,
            radius: ctx.theme.small_radius,
            fill: Some(ctx.theme.surface),
            border: Some((ctx.theme.border, 1.0)),
            clip: Some(self.rect),
        });
        self.body.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        self.body.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.body.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn controls_tab(t: &Theme, d: &ControlData) -> VBox {
    let mut body = VBox::new(12.0).padded(8.0);

    // clock row + Lock (legacy header row)
    body = body.push(
        HBox::new(8.0)
            .push(
                VBox::new(4.0)
                    .push(Label::styled(&d.now_hm, 32.0, BOLD, t.text))
                    .push(Label::styled(&d.now_date, 12.0, NORMAL, t.muted)),
            )
            .push(Spacer::fill())
            .push(small_button(t, ids::LOCK, "Lock", false, t.text)),
    );

    // quick-toggle tiles (2 columns, legacy GridLayout)
    let wifi = d.wifi.clone().unwrap_or(WifiState {
        connected: false,
        enabled: false,
        ssid: String::new(),
    });
    let bt = d.bluetooth.clone().unwrap_or(BluetoothState {
        available: false,
        enabled: false,
    });
    let wifi_detail = if d.wifi.is_none() {
        "Unavailable".to_string()
    } else if wifi.connected {
        if wifi.ssid.is_empty() {
            "Connected".to_string()
        } else {
            wifi.ssid.clone()
        }
    } else if wifi.enabled {
        "Not connected".to_string()
    } else {
        "Turned off".to_string()
    };
    let bt_detail = if !bt.available {
        "No adapter".to_string()
    } else if bt.enabled {
        "Powered on".to_string()
    } else {
        "Turned off".to_string()
    };
    body = body.push(
        HBox::new(12.0)
            .push(tile(
                t,
                ids::TILE_WIFI,
                "Wi-Fi",
                &wifi_detail,
                wifi.enabled,
                t.cyan,
            ))
            .push(tile(
                t,
                ids::TILE_BLUETOOTH,
                "Bluetooth",
                &bt_detail,
                bt.enabled,
                t.purple,
            )),
    );
    body = body.push(
        HBox::new(12.0)
            .push(tile(
                t,
                ids::TILE_DND,
                "Do not disturb",
                if d.dnd {
                    "Popups silenced"
                } else {
                    "Notifications enabled"
                },
                d.dnd,
                t.yellow,
            ))
            .push(tile(
                t,
                ids::TILE_MOTION,
                "Motion",
                if d.animations {
                    "Animations on"
                } else {
                    "Reduced motion"
                },
                d.animations,
                t.pink,
            )),
    );

    // sliders card (volume + brightness; mic joins with its feed, Tier 2.2)
    let mut sliders = VBox::new(8.0).padded(13.0);
    let audio = d.audio.clone();
    sliders = sliders.push(
        HBox::new(8.0)
            .push(small_button(
                t,
                ids::VOL_MUTE,
                if audio.as_ref().is_some_and(|a| a.muted) {
                    "Unmute"
                } else {
                    "Volume"
                },
                false,
                t.text,
            ))
            .push(Spacer::fill())
            .push(Label::styled(
                &match &audio {
                    None => "Audio unavailable".to_string(),
                    Some(a) if a.muted => "Muted".to_string(),
                    Some(a) => format!("{}%", a.volume.round() as i64),
                },
                12.0,
                NORMAL,
                t.muted,
            )),
    );
    let (vol, vol_enabled) = match &audio {
        Some(a) => (a.volume as f32, true),
        None => (0.0, false),
    };
    let mut vol_slider = Slider::new(ids::VOL_SLIDER, vol, t);
    if vol_enabled {
        vol_slider.fill = t.purple;
        vol_slider.thumb = t.purple;
    }
    sliders = sliders.push(vol_slider);
    sliders = sliders.push(
        HBox::new(8.0)
            .push(Label::styled("Brightness", 13.0, NORMAL, t.text))
            .push(Spacer::fill())
            .push(Label::styled(
                &match d.brightness {
                    Some(p) => format!("{}%", p.round() as i64),
                    None => "No backlight device".to_string(),
                },
                12.0,
                NORMAL,
                t.muted,
            )),
    );
    let mut bright_slider = Slider::new(ids::BRIGHT_SLIDER, d.brightness.unwrap_or(0.0) as f32, t);
    if d.brightness.is_some() {
        bright_slider.fill = t.yellow;
        bright_slider.thumb = t.yellow;
    }
    sliders = sliders.push(bright_slider);
    body = body.push(Card::new(Box::new(sliders)));

    // power profile row (hidden without a profile feed)
    if let Some(profile) = &d.power_profile {
        let values = ["power-saver", "balanced", "performance"];
        let labels = ["Saver", "Balanced", "Performance"];
        let mut row = HBox::new(8.0)
            .push(Label::styled("Power profile", 13.0, NORMAL, t.text))
            .push(Spacer::fill());
        for (i, label) in labels.iter().enumerate() {
            row = row.push(small_button(
                t,
                ids::profile(i),
                label,
                values[i] == *profile,
                t.text,
            ));
        }
        body = body.push(Card::new(Box::new(row)));
    }

    // media card (MPRIS feed exists since Tier 1.2)
    if let Some(m) = &d.media {
        let mut media = VBox::new(10.0).padded(14.0);
        let title = if m.title.is_empty() {
            "Nothing playing".to_string()
        } else {
            m.title.clone()
        };
        let artist = if m.artist.is_empty() {
            "Play media in your favorite app".to_string()
        } else {
            m.artist.clone()
        };
        media = media.push(
            VBox::new(4.0)
                .push(Label::styled(&title, 14.0, BOLD, t.text))
                .push(Label::styled(&artist, 12.0, NORMAL, t.muted)),
        );
        media = media.push(
            HBox::new(10.0)
                .push(small_button(t, ids::MEDIA_PREV, "Previous", false, t.text))
                .push(small_button(
                    t,
                    ids::MEDIA_PLAY,
                    if m.playing { "Pause" } else { "Play" },
                    true,
                    t.background,
                ))
                .push(small_button(t, ids::MEDIA_NEXT, "Next", false, t.text)),
        );
        body = body.push(Card::new(Box::new(media)));
    }

    // resources card (system feed since Tier 1.2)
    let mut res = VBox::new(8.0).padded(14.0);
    let pct = |v: Option<f64>| {
        v.map(|x| format!("{}%", x.round() as i64))
            .unwrap_or("—".into())
    };
    res = res.push(
        HBox::new(12.0)
            .push(
                VBox::new(5.0)
                    .push(Label::styled("CPU", 12.0, NORMAL, t.muted))
                    .push(Label::styled(pct(d.cpu), 17.0, BOLD, t.cyan)),
            )
            .push(
                VBox::new(5.0)
                    .push(Label::styled("RAM", 12.0, NORMAL, t.muted))
                    .push(Label::styled(pct(d.ram), 17.0, BOLD, t.purple)),
            )
            .push(
                VBox::new(5.0)
                    .push(Label::styled("Battery", 12.0, NORMAL, t.muted))
                    .push(Label::styled(
                        &match d.battery_percent {
                            Some(p) => format!(
                                "{}{}%",
                                p.round() as i64,
                                if d.battery_charging { " +" } else { "" }
                            ),
                            None => "—".to_string(),
                        },
                        17.0,
                        BOLD,
                        t.green,
                    )),
            ),
    );
    res = res.push(Label::styled(
        if d.battery_charging {
            "Charging  ·  system feed refreshes every 5 s"
        } else {
            "System feed refreshes every 5 s"
        },
        10.0,
        NORMAL,
        t.muted,
    ));
    body = body.push(Card::new(Box::new(res)));

    body
}

/// Placeholder for tabs whose service module has not landed (NATIVE_PLAN).
fn placeholder_tab(t: &Theme, tab: usize) -> VBox {
    VBox::new(10.0)
        .padded(24.0)
        .push(Label::styled(TABS[tab], 18.0, BOLD, t.text))
        .push(Label::styled(
            format!("This tab lands with {}.", TAB_TIERS[tab]),
            13.0,
            NORMAL,
            t.muted,
        ))
}

fn small_button(t: &Theme, id: u64, label: &str, primary: bool, text: Color) -> Button {
    let mut b = Button::new(id, label);
    b.label_size = 12.0;
    b.label_weight = MEDIUM;
    b.fill = if primary {
        t.purple
    } else {
        t.panel_fill.alpha(0.55)
    };
    b.hover_fill = Some(if primary { t.purple } else { t.panel_fill });
    b.text_color = text;
    b.radius = t.small_radius;
    b.border = Some((t.border, 1.0));
    b.min_h = 30.0;
    b
}

impl Widget for ControlCenterPanel {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.content.measure(ctx)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        self.content.layout(ctx, bounds);
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        // glass panel background (GlassPanel role controlFill)
        out.push(lme_ui::draw::DrawCmd::RoundedRect {
            rect: self.rect,
            radius: ctx.theme.radius,
            fill: Some(ctx.theme.panel_fill.alpha(ctx.theme.panel_opacity)),
            border: Some((ctx.theme.outline_variant, 1.0)),
            clip: Some(self.rect),
        });
        self.content.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        self.content.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.content.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Build the daemon-side snapshot from Shell state (kept here so the
/// daemon call site stays small).
pub fn control_data(shell: &crate::daemon::Shell) -> ControlData {
    use serde_json::Value;
    let snap = &shell.system_snapshot;
    let get_f = |key: &str| snap.get(key).and_then(Value::as_f64);
    let wifi = snap.get("network").map(|n| WifiState {
        connected: n.get("connected").and_then(Value::as_bool).unwrap_or(false),
        enabled: n
            .get("connected")
            .and_then(Value::as_bool)
            .or_else(|| n.get("enabled").and_then(Value::as_bool))
            .unwrap_or(false),
        ssid: n
            .get("ssid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    });
    let bluetooth = snap.get("bluetooth").map(|b| BluetoothState {
        available: b.get("available").and_then(Value::as_bool).unwrap_or(false),
        enabled: b.get("enabled").and_then(Value::as_bool).unwrap_or(false),
    });
    let audio = snap.get("audio").and_then(|a| {
        if a.get("available").and_then(Value::as_bool).unwrap_or(false) {
            Some(AudioState {
                volume: a.get("volume").and_then(Value::as_f64).unwrap_or(0.0),
                muted: a.get("muted").and_then(Value::as_bool).unwrap_or(false),
            })
        } else {
            None
        }
    });
    let media = shell.mpris.first().map(|p| MediaState {
        title: p.display_title().to_string(),
        artist: p.artist_line(),
        playing: p.is_playing(),
    });
    let battery = snap.get("battery").cloned().unwrap_or(Value::Null);
    let battery_available = battery
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ControlData {
        now_hm: crate::daemon::clock_label(true),
        now_date: crate::daemon::clock_date_label(),
        dnd: crate::notifications::dnd_active(&shell.settings.values),
        animations: shell
            .settings
            .values
            .get("animations")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        wifi,
        bluetooth,
        audio,
        brightness: snap
            .get("brightness")
            .and_then(|b| b.get("percent").and_then(Value::as_f64)),
        power_profile: snap
            .get("powerProfile")
            .and_then(Value::as_str)
            .map(str::to_string),
        media,
        cpu: get_f("cpu"),
        ram: get_f("memoryPercent"),
        battery_percent: if battery_available {
            battery.get("percent").and_then(Value::as_f64)
        } else {
            None
        },
        battery_charging: battery
            .get("charging")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::click_ids;
    use lme_ui::anim::Animator;
    use lme_ui::geom::Point;
    use lme_ui::shadow::ShadowCache;
    use lme_ui::text::FontBook;
    use lme_ui::theme::Palette;
    use lme_ui::widgets::UiEvent;

    fn data() -> ControlData {
        ControlData {
            now_hm: "12:34".into(),
            now_date: "Mon, Oct 12".into(),
            dnd: false,
            animations: true,
            wifi: Some(WifiState {
                connected: true,
                enabled: true,
                ssid: "Golden-5G".into(),
            }),
            bluetooth: Some(BluetoothState {
                available: true,
                enabled: false,
            }),
            audio: Some(AudioState {
                volume: 64.0,
                muted: false,
            }),
            brightness: None,
            power_profile: Some("balanced".into()),
            media: Some(MediaState {
                title: "Native track".into(),
                artist: "The Rust Players".into(),
                playing: true,
            }),
            cpu: Some(12.0),
            ram: Some(34.0),
            battery_percent: Some(77.0),
            battery_charging: true,
        }
    }

    /// The panel test kit used across panels.rs/tray.rs: real Ctx, layout
    /// into a panel-sized window, event delivery through the real path.
    struct Kit {
        theme: Theme,
        fonts: FontBook,
        anims: Animator,
        shadows: ShadowCache,
        queue: Vec<UiEvent>,
    }

    impl Kit {
        fn new() -> Self {
            Self {
                theme: Theme::default_with(&Palette::default()),
                fonts: FontBook::new(),
                anims: Animator::new(),
                shadows: ShadowCache::new(),
                queue: Vec::new(),
            }
        }
        fn ctx(&mut self) -> Ctx<'_> {
            Ctx {
                theme: &mut self.theme,
                fonts: &mut self.fonts,
                anims: &mut self.anims,
                shadows: &mut self.shadows,
                queue: &mut self.queue,
                now: std::time::Instant::now(),
                focused: None,
                repaint: false,
            }
        }
        fn layout(&mut self, panel: &mut ControlCenterPanel) {
            let mut c = self.ctx();
            panel.layout(&mut c, Rect::new(0.0, 0.0, PANEL_W, 740.0));
        }
        /// Grid-scan pointer pass: click every 6 px and return every id
        /// that received a Click (real hit-testing, no tree walking).
        fn click_ids_by_scan(&mut self, panel: &mut ControlCenterPanel) -> Vec<u64> {
            let mut hit = Vec::new();
            let mut y = 4.0;
            while y < 736.0 {
                let mut x = 4.0;
                while x < PANEL_W - 4.0 {
                    let p = Point::new(x, y);
                    let before = self.queue.len();
                    let mut c = self.ctx();
                    panel.deliver(&mut c, &InputEvent::down(p), None);
                    panel.deliver(&mut c, &InputEvent::up(p), None);
                    for ev in &self.queue[before..] {
                        if let UiEvent::Click(id) = ev {
                            if !hit.contains(id) {
                                hit.push(*id);
                            }
                        }
                    }
                    x += 6.0;
                }
                y += 6.0;
            }
            hit
        }
    }

    #[test]
    fn ids_stay_inside_the_registered_range() {
        let all = [
            ids::CLOSE,
            ids::tab(0),
            ids::tab(5),
            ids::LOCK,
            ids::TILE_WIFI,
            ids::TILE_BLUETOOTH,
            ids::TILE_DND,
            ids::TILE_MOTION,
            ids::VOL_MUTE,
            ids::VOL_SLIDER,
            ids::BRIGHT_SLIDER,
            ids::profile(0),
            ids::profile(2),
            ids::MEDIA_PREV,
            ids::MEDIA_PLAY,
            ids::MEDIA_NEXT,
        ];
        for id in all {
            assert!(
                click_ids::CONTROL_PANEL.contains(id),
                "id {id:#x} outside CONTROL_PANEL"
            );
        }
    }

    #[test]
    fn controls_tab_routes_every_section_click() {
        let mut kit = Kit::new();
        let mut panel = ControlCenterPanel::new(&kit.theme, &data(), 0);
        kit.layout(&mut panel);
        let hit = kit.click_ids_by_scan(&mut panel);
        for id in [
            ids::CLOSE,
            ids::tab(0),
            ids::tab(5),
            ids::LOCK,
            ids::TILE_WIFI,
            ids::TILE_BLUETOOTH,
            ids::TILE_DND,
            ids::TILE_MOTION,
            ids::VOL_MUTE,
            ids::profile(0),
            ids::profile(2),
            ids::MEDIA_PREV,
            ids::MEDIA_PLAY,
            ids::MEDIA_NEXT,
        ] {
            assert!(
                hit.contains(&id),
                "click never reached {id:#x}; got {hit:#x?}"
            );
        }
    }

    #[test]
    fn unavailable_media_hides_the_media_card() {
        let mut kit = Kit::new();
        let mut d = data();
        d.media = None;
        d.wifi = None;
        let mut panel = ControlCenterPanel::new(&kit.theme, &d, 0);
        kit.layout(&mut panel);
        let hit = kit.click_ids_by_scan(&mut panel);
        assert!(
            hit.contains(&ids::TILE_WIFI),
            "tile stays with Unavailable text; got {hit:#x?}"
        );
        assert!(
            !hit.contains(&ids::MEDIA_PLAY),
            "media card must be hidden without players"
        );
    }

    #[test]
    fn placeholder_tab_shows_no_controls_content() {
        let mut kit = Kit::new();
        let mut panel = ControlCenterPanel::new(&kit.theme, &data(), 1);
        kit.layout(&mut panel);
        let hit = kit.click_ids_by_scan(&mut panel);
        assert!(hit.contains(&ids::tab(1)), "tab bar present");
        assert!(
            !hit.contains(&ids::TILE_WIFI),
            "controls content must not render on a placeholder tab"
        );
    }

    /// Locates the volume slider (window coordinates for the e2e pointer
    /// spec; SlidePanelRight puts the panel at x=1280-600-14, y=10).
    #[test]
    fn volume_slider_position_is_stable() {
        let mut kit = Kit::new();
        let mut panel = ControlCenterPanel::new(&kit.theme, &data(), 0);
        kit.layout(&mut panel);
        let mut found: Option<(f32, f32)> = None;
        let mut y = 4.0;
        while y < 736.0 && found.is_none() {
            let mut x = 4.0;
            while x < PANEL_W - 4.0 {
                let p = Point::new(x, y);
                let before = kit.queue.len();
                let mut c = kit.ctx();
                panel.deliver(&mut c, &InputEvent::down(p), None);
                panel.deliver(&mut c, &InputEvent::up(p), None);
                if kit.queue[before..]
                    .iter()
                    .any(|e| matches!(e, UiEvent::Slide(id, _) if *id == ids::VOL_SLIDER))
                {
                    found = Some((x, y + 12.0)); // center of the 24px track
                    break;
                }
                x += 2.0;
            }
            y += 2.0;
        }
        let Some((x, y)) = found else {
            panic!("volume slider never hit by the scan")
        };
        // lock the layout: slider sits in the sliders card, mid-panel
        assert!(
            y > 300.0 && y < 620.0,
            "volume slider moved: ({x},{y}) — update the e2e pointer spec"
        );
        eprintln!("E2E_SLIDER_PANEL_POS={x},{y}");
    }
}
