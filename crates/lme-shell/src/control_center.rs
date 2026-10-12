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
use lme_ui::widgets::{Button, Ctx, HBox, InputEvent, Label, Slider, Spacer, VBox, Widget};

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
    pub const TILE_STAY_AWAKE: u64 = 0x8034;
    pub const TILE_NIGHT_LIGHT: u64 = 0x8035;
    pub const VOL_MUTE: u64 = 0x8040;
    pub const VOL_SLIDER: u64 = 0x8041;
    pub const BRIGHT_SLIDER: u64 = 0x8042;
    pub const PROFILE_BASE: u64 = 0x8050; // +0 power-saver, +1 balanced, +2 performance
    pub const MEDIA_PREV: u64 = 0x8060;
    pub const MEDIA_PLAY: u64 = 0x8061;
    pub const MEDIA_NEXT: u64 = 0x8062;
    /// Tier 2.6 weather card refresh button (legacy weatherRefresh action).
    pub const WEATHER_REFRESH: u64 = 0x8070;

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
    pub stay_awake: bool,
    pub night_light: bool,
    pub night_light_temp: i64,
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
    /// Tier 2.6 weather card feed (None hides the card, like media).
    pub weather: Option<WeatherState>,
}

/// Tier 2.6 weather snapshot fields the card renders (WeatherWidget.qml
/// port). Parsed from the daemon `system_snapshot["weather"]` — the backend
/// WeatherClient snapshot shape (frozen in Phase 1, docs/weather.rs).
#[derive(Clone)]
pub struct WeatherState {
    pub available: bool,
    pub loading: bool,
    pub place_name: String,
    pub place_subtitle: String,
    pub temperature: Option<f64>,
    pub unit: String,
    pub condition: String,
    pub icon: String,
    pub feels_like: Option<f64>,
    pub humidity: Option<f64>,
    pub wind: Option<f64>,
    pub wind_unit: String,
    pub daily: Vec<DailyForecast>,
    pub error: String,
}

#[derive(Clone)]
pub struct DailyForecast {
    pub date: String,
    pub max: Option<f64>,
    pub min: Option<f64>,
    pub rain_chance: Option<f64>,
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

/// The whole control-center panel: fixed header + tab grid, and the tab
/// content in a wheel-scrolling Flickable (the QML ControlDashboard
/// Flickable port — KI-3; landed with Tier 2.6 because the weather card
/// pushed the Controls tab past 800 px outputs).
pub struct ControlCenterPanel {
    pub current_tab: usize,
    /// Header + tab grid (never scrolls).
    top: Box<dyn Widget>,
    /// Scrolling tab content.
    body: lme_ui::widgets::Flickable,
    rect: Rect,
}

impl ControlCenterPanel {
    pub fn new(theme: &Theme, data: &ControlData, current_tab: usize) -> Self {
        let mut panel = Self {
            current_tab,
            top: Box::new(VBox::new(0.0)),
            body: lme_ui::widgets::Flickable::new(Box::new(VBox::new(0.0))),
            rect: Rect::default(),
        };
        panel.rebuild(theme, data, current_tab);
        panel
    }

    /// Rebuild from a fresh snapshot, KEEPING the scroll offset (data
    /// refreshes must not move the viewport). Switching tabs resets it.
    pub fn rebuild(&mut self, theme: &Theme, data: &ControlData, tab: usize) {
        if tab != self.current_tab {
            self.current_tab = tab;
            self.body.scroll = 0.0;
        }
        let t = theme;
        let mut top = VBox::new(10.0).padded(14.0);
        // header (ControlCenter.qml)
        top = top.push(
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
                let btn = tab_button(t, index, self.current_tab);
                hbox = hbox.push(lme_ui::widgets::Fill::new(Box::new(btn)));
            }
            rows = rows.push(hbox);
        }
        top = top.push(rows);
        self.top = Box::new(top);
        // tab content, wrapped for the same side margins the old shared
        // column gave the body (14 px + the tab's own 8 px padding)
        let content: Box<dyn Widget> = if self.current_tab == 0 {
            Box::new(controls_tab(t, data))
        } else {
            Box::new(placeholder_tab(t, self.current_tab))
        };
        self.body
            .set_child(Box::new(VBox::new(0.0).padded(14.0).push(content)));
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

    // weather card (legacy controlLayout order: weather is the FIRST
    // module card, right after the clock row; Tier 2.6)
    if let Some(wx) = &d.weather {
        body = body.push(weather_card(t, wx));
    }

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
            .push(lme_ui::widgets::Fill::new(Box::new(tile(
                t,
                ids::TILE_WIFI,
                "Wi-Fi",
                &wifi_detail,
                wifi.enabled,
                t.cyan,
            ))))
            .push(lme_ui::widgets::Fill::new(Box::new(tile(
                t,
                ids::TILE_BLUETOOTH,
                "Bluetooth",
                &bt_detail,
                bt.enabled,
                t.purple,
            )))),
    );
    body = body.push(
        HBox::new(12.0)
            .push(lme_ui::widgets::Fill::new(Box::new(tile(
                t,
                ids::TILE_STAY_AWAKE,
                "Stay awake",
                if d.stay_awake {
                    "Idle sleep inhibited"
                } else {
                    "Normal idle behavior"
                },
                d.stay_awake,
                t.green,
            ))))
            .push(lme_ui::widgets::Fill::new(Box::new(tile(
                t,
                ids::TILE_NIGHT_LIGHT,
                "Night Light",
                &if d.night_light {
                    format!("{} K", d.night_light_temp)
                } else {
                    "Turned off".to_string()
                },
                d.night_light,
                t.yellow,
            )))),
    );
    body = body.push(
        HBox::new(12.0)
            .push(lme_ui::widgets::Fill::new(Box::new(tile(
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
            ))))
            .push(lme_ui::widgets::Fill::new(Box::new(tile(
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
            )))),
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

// ---------------------------------------------------------------------------
// Tier 2.6 weather card (WeatherWidget.qml port)
// ---------------------------------------------------------------------------

/// Parse the backend WeatherClient snapshot (weather.rs `snapshot()` shape)
/// into the owned card state. Pure + unit-tested against the frozen shape.
pub fn weather_state(value: &serde_json::Value) -> WeatherState {
    let get_f = |key: &str| value.get(key).and_then(serde_json::Value::as_f64);
    let get_s = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let location = value
        .get("location")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let place_name = location
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let automatic = value
        .get("automatic")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let place_subtitle = if automatic {
        "Approximate location · by IP".to_string()
    } else {
        location
            .get("country")
            .and_then(serde_json::Value::as_str)
            .filter(|c| !c.is_empty())
            .unwrap_or("Selected location")
            .to_string()
    };
    let daily = value
        .get("daily")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .take(3)
                .map(|row| DailyForecast {
                    date: row
                        .get("date")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    max: row.get("max").and_then(serde_json::Value::as_f64),
                    min: row.get("min").and_then(serde_json::Value::as_f64),
                    rain_chance: row.get("rainChance").and_then(serde_json::Value::as_f64),
                })
                .collect()
        })
        .unwrap_or_default();
    WeatherState {
        available: value
            .get("available")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        loading: value
            .get("loading")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        place_name,
        place_subtitle,
        temperature: get_f("temperature"),
        unit: get_s("unit"),
        condition: get_s("condition"),
        icon: get_s("icon"),
        feels_like: get_f("feelsLike"),
        humidity: get_f("humidity"),
        wind: get_f("wind"),
        wind_unit: get_s("windUnit"),
        daily,
        error: get_s("error"),
    }
}

/// `Math.round(value) + suffix` with the legacy "—" for null.
fn wx_number(value: Option<f64>, suffix: &str) -> String {
    value
        .map(|v| format!("{}{suffix}", v.round() as i64))
        .unwrap_or_else(|| "—".into())
}

/// Weather icon name → text glyph. Monochrome presentation symbols from
/// the system fallback fonts (DejaVu Sans on Fedora) — colored by the Text
/// command like the legacy Theme.yellow line-art (no hicolor PNG dependency
/// for the deterministic goldens; D7 keeps SVG out of scope).
fn weather_glyph(icon: &str) -> &'static str {
    match icon {
        "weather-sun" => "☀",
        "weather-rain" => "☂",
        "weather-snow" => "❄",
        "weather-storm" => "⚡",
        // weather-cloud and anything unknown
        _ => "☁",
    }
}

/// "YYYY-MM-DD" → short weekday (legacy `Qt.locale("en_US")... "ddd"` on
/// date + "T12:00:00"). Pure civil-date math, days-from-epoch (1970-01-01
/// was a Thursday, i.e. days-from-epoch 0 maps to index 0); unparseable
/// dates return None.
fn weekday_short(date: &str) -> Option<&'static str> {
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // per-month day validity (leap-year aware) — "2026-02-30" is rejected
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    if day > dim {
        return None;
    }
    // Howard Hinnant's days_from_civil (proleptic Gregorian).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    Some(DAYS[days.rem_euclid(7) as usize])
}

/// One daily mini-card (legacy delegate: panel @0.6, radius 16, three
/// centered lines).
struct MiniCard {
    body: Box<dyn Widget>,
    rect: Rect,
}

impl MiniCard {
    fn new(body: Box<dyn Widget>) -> Self {
        Self {
            body,
            rect: Rect::default(),
        }
    }
}

impl Widget for MiniCard {
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
            fill: Some(ctx.theme.panel_fill.alpha(0.6)),
            border: None,
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

/// The weather module card (WeatherWidget.qml port: title row + refresh,
/// condition row, humidity/wind line, 3-day forecast, status + attribution).
fn weather_card(t: &Theme, w: &WeatherState) -> Card {
    let mut card = VBox::new(10.0).padded(14.0);
    // title row: place + subtitle | refresh (legacy IconButton row)
    card = card.push(
        HBox::new(10.0)
            .push(
                VBox::new(3.0)
                    .push(Label::styled(
                        &if w.place_name.is_empty() {
                            "Local weather".to_string()
                        } else {
                            w.place_name.clone()
                        },
                        16.0,
                        BOLD,
                        t.text,
                    ))
                    .push(Label::styled(&w.place_subtitle, 11.0, NORMAL, t.muted)),
            )
            .push(Spacer::fill())
            .push(small_button(
                t,
                ids::WEATHER_REFRESH,
                if w.loading { "Updating…" } else { "Refresh" },
                false,
                t.text,
            )),
    );
    if w.available {
        // condition row (legacy wide variant: icon 42, temperature 34)
        card = card.push(
            HBox::new(14.0)
                .push(lme_ui::widgets::Icon::new(
                    weather_glyph(&w.icon),
                    42.0,
                    t.yellow,
                ))
                .push(Label::styled(
                    wx_number(w.temperature, &w.unit),
                    34.0,
                    BOLD,
                    t.text,
                ))
                .push(
                    VBox::new(4.0)
                        .push(Label::styled(&w.condition, 14.0, MEDIUM, t.text))
                        .push(Label::styled(
                            format!("Feels like {}", wx_number(w.feels_like, &w.unit)),
                            12.0,
                            NORMAL,
                            t.muted,
                        )),
                ),
        );
        card = card.push(Label::styled(
            format!(
                "Humidity {}  ·  Wind {}",
                wx_number(w.humidity, "%"),
                wx_number(w.wind, &format!(" {}", w.wind_unit))
            ),
            12.0,
            NORMAL,
            t.muted,
        ));
        // 3-day forecast row (panel width is 600: always the wide variant)
        if !w.daily.is_empty() {
            let mut row = HBox::new(8.0);
            for (index, day) in w.daily.iter().take(3).enumerate() {
                let name = if index == 0 {
                    "Today".to_string()
                } else {
                    weekday_short(&day.date)
                        .map(str::to_string)
                        .unwrap_or_else(|| "—".into())
                };
                let mut lines = VBox::new(2.0).padded(9.0);
                for (text, size, weight, color) in [
                    (name, 11.0, BOLD, t.text),
                    (
                        format!("{} / {}", wx_number(day.max, "°"), wx_number(day.min, "°")),
                        12.0,
                        NORMAL,
                        t.text,
                    ),
                    (wx_number(day.rain_chance, "% rain"), 10.0, NORMAL, t.cyan),
                ] {
                    let mut l = Label::styled(&text, size, weight, color);
                    l.align = lme_ui::widgets::TextAlign::Center;
                    lines = lines.push(l);
                }
                row = row.push(lme_ui::widgets::Fill::new(Box::new(MiniCard::new(
                    Box::new(lines),
                ))));
            }
            card = card.push(row);
        }
    } else if !w.error.is_empty() || w.loading {
        // status line (yellow on error, muted otherwise)
        let text = if !w.error.is_empty() {
            if w.available {
                format!("Last forecast · {}", w.error)
            } else {
                w.error.clone()
            }
        } else if !w.place_name.is_empty() {
            "Updating forecast…".to_string()
        } else {
            "Finding your approximate location…".to_string()
        };
        let color = if w.error.is_empty() {
            t.muted
        } else {
            t.yellow
        };
        card = card.push(Label::styled(text, 12.0, NORMAL, color));
    } else {
        card = card.push(Label::styled(
            "Choose a location in Weather settings.",
            12.0,
            NORMAL,
            t.muted,
        ));
    }
    card = card.push(Label::styled(
        "Weather by Open-Meteo",
        10.0,
        NORMAL,
        t.muted,
    ));
    Card::new(Box::new(card))
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
        let top = self.top.measure(ctx);
        let content = self.body.measure(ctx);
        Size::new(top.w.max(content.w), top.h + content.h)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        // fixed top: natural height (capped at the panel); the body gets
        // the remainder as its scroll viewport
        let top_h = self.top.measure(ctx).h.min(bounds.h);
        self.top
            .layout(ctx, Rect::new(bounds.x, bounds.y, bounds.w, top_h));
        let gap = 10.0;
        let body_h = (bounds.h - top_h - gap).max(0.0);
        self.body.layout(
            ctx,
            Rect::new(bounds.x, bounds.y + top_h + gap, bounds.w, body_h),
        );
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
        self.top.paint(ctx, out);
        self.body.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if let InputEvent::Down { pos: p, .. } = ev {
            if std::env::var_os("LME_PTR_DEBUG").is_some() {
                let mut ids = Vec::new();
                self.top.focusables(&mut ids);
                self.body.focusables(&mut ids);
                eprintln!(
                    "cc-panel: down ({:.0},{:.0}) rect={:?} scroll={} max={} viewport={:?} body_rect={:?} focusables={ids:#x?}",
                    p.x, p.y, self.rect, self.body.scroll, self.body.max_scroll(), self.body.rect(), self.top.rect()
                );
            }
        }
        // the top bar and the scroll viewport are disjoint; either may hit
        let top_hit = self.top.deliver(ctx, ev, target);
        let body_hit = self.body.deliver(ctx, ev, target);
        top_hit || body_hit
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.top.focusables(out);
        self.body.focusables(out);
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
        stay_awake: shell.stay_awake,
        night_light: shell
            .settings
            .values
            .get("nightLightEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        night_light_temp: shell
            .settings
            .values
            .get("nightLightTemperature")
            .and_then(Value::as_i64)
            .unwrap_or(4500),
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
        weather: shell.system_snapshot.get("weather").map(weather_state),
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
            stay_awake: false,
            night_light: false,
            night_light_temp: 4500,
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
            weather: Some(WeatherState {
                available: true,
                loading: false,
                place_name: "Hanoi".into(),
                place_subtitle: "Vietnam".into(),
                temperature: Some(29.0),
                unit: "°C".into(),
                condition: "Light rain".into(),
                icon: "weather-rain".into(),
                feels_like: Some(32.0),
                humidity: Some(76.0),
                wind: Some(10.0),
                wind_unit: "km/h".into(),
                daily: vec![
                    DailyForecast {
                        date: "2026-10-13".into(),
                        max: Some(31.0),
                        min: Some(24.0),
                        rain_chance: Some(60.0),
                    },
                    DailyForecast {
                        date: "2026-10-14".into(),
                        max: Some(30.0),
                        min: Some(24.0),
                        rain_chance: Some(40.0),
                    },
                    DailyForecast {
                        date: "2026-10-15".into(),
                        max: Some(29.0),
                        min: Some(23.0),
                        rain_chance: Some(20.0),
                    },
                ],
                error: String::new(),
            }),
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
            ids::WEATHER_REFRESH,
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
        // the tab content overflows the viewport (KI-3 Flickable): scan the
        // visible part, scroll to the bottom, scan again, union.
        let mut hit = kit.click_ids_by_scan(&mut panel);
        let max = panel.body.max_scroll();
        assert!(max > 200.0, "expected overflow, max_scroll={max}");
        panel.body.scroll = max;
        kit.layout(&mut panel);
        for id in kit.click_ids_by_scan(&mut panel) {
            if !hit.contains(&id) {
                hit.push(id);
            }
        }
        for id in [
            ids::CLOSE,
            ids::tab(0),
            ids::tab(5),
            ids::LOCK,
            ids::TILE_WIFI,
            ids::TILE_BLUETOOTH,
            ids::TILE_DND,
            ids::TILE_MOTION,
            ids::TILE_STAY_AWAKE,
            ids::TILE_NIGHT_LIGHT,
            ids::VOL_MUTE,
            ids::profile(0),
            ids::profile(2),
            ids::MEDIA_PREV,
            ids::MEDIA_PLAY,
            ids::MEDIA_NEXT,
            ids::WEATHER_REFRESH,
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
    /// E2E coordinate proof: the full stack (SlidePanelRight wrapper +
    /// panel laid out in a 1280x800 window) must route a click at the
    /// recorded bluetooth-tile window coords to TILE_BLUETOOTH.
    #[test]
    fn bluetooth_tile_clicks_through_the_wrapper() {
        use crate::panels::SlidePanelRight;
        let mut kit = Kit::new();
        let data = data();
        let panel = ControlCenterPanel::new(&kit.theme, &data, 0);
        let mut root = SlidePanelRight::new(Box::new(panel), 600.0, 758.0, 0);
        let mut c = kit.ctx();
        root.layout(&mut c, Rect::new(0.0, 0.0, 1280.0, 800.0));
        // bt tile panel-local center comes from
        // volume_slider_position_is_stable (E2E_BT_TILE_PANEL_POS); window
        // coords = panel offset (666,10) + panel-local
        let p = Point::new(666.0 + 441.0, 10.0 + 563.0);
        let before = kit.queue.len();
        let mut c = kit.ctx();
        root.deliver(&mut c, &InputEvent::down(p), None);
        root.deliver(&mut c, &InputEvent::up(p), None);
        let events: Vec<String> = kit.queue[before..]
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        assert!(
            kit.queue[before..]
                .iter()
                .any(|e| matches!(e, UiEvent::Click(id) if *id == ids::TILE_BLUETOOTH)),
            "click at (1107,573) must hit the bluetooth tile, got {events:?}"
        );
    }

    /// One scan at the CURRENT scroll offset; positions are panel-local.
    fn scan_find(
        kit: &mut Kit,
        panel: &mut ControlCenterPanel,
        want: u64,
        slide: bool,
    ) -> Option<(f32, f32)> {
        let mut y = 4.0;
        while y < 736.0 {
            let mut x = 4.0;
            while x < PANEL_W - 4.0 {
                let p = Point::new(x, y);
                let before = kit.queue.len();
                let mut c = kit.ctx();
                panel.deliver(&mut c, &InputEvent::down(p), None);
                panel.deliver(&mut c, &InputEvent::up(p), None);
                let got = kit.queue[before..].iter().any(|e| {
                    matches!(e, UiEvent::Click(id) if *id == want)
                        || (slide && matches!(e, UiEvent::Slide(id, _) if *id == want))
                });
                if got {
                    return Some((x, y));
                }
                x += if slide { 2.0 } else { 4.0 };
            }
            y += if slide { 2.0 } else { 4.0 };
        }
        None
    }

    #[test]
    fn volume_slider_position_is_stable() {
        let mut kit = Kit::new();
        let mut panel = ControlCenterPanel::new(&kit.theme, &data(), 0);
        kit.layout(&mut panel);
        // the slider lives below the fold (weather card + tiles first):
        // the e2e scrolls to the bottom, so lock its position AT max scroll
        let max = panel.body.max_scroll();
        assert!(max > 200.0, "expected overflow, max_scroll={max}");
        panel.body.scroll = max;
        kit.layout(&mut panel);
        let Some((x, y)) = scan_find(&mut kit, &mut panel, ids::VOL_SLIDER, true) else {
            panic!("volume slider never hit by the scan (scrolled to bottom)")
        };
        let (x, y) = (x + 20.0, y + 12.0); // center of the 24px track
                                           // lock the layout: the sliders card sits in the lower half of the
                                           // scrolled viewport
        assert!(
            y > 300.0 && y < 620.0,
            "volume slider moved: ({x},{y}) — update the e2e pointer spec"
        );
        eprintln!("E2E_SLIDER_PANEL_POS={x},{y}");
        // the tiles are above the fold: lock their positions at scroll 0
        panel.body.scroll = 0.0;
        kit.layout(&mut panel);
        let Some((tx, ty)) = scan_find(&mut kit, &mut panel, ids::TILE_BLUETOOTH, false) else {
            panic!("bluetooth tile never hit by the scan")
        };
        let (tx, ty) = (tx + 130.0, ty + 28.0); // center of the 56px tile, right column
        eprintln!("E2E_BT_TILE_PANEL_POS={tx},{ty}");
        assert!(
            ty > 400.0 && ty < 700.0,
            "bluetooth tile moved: ({tx},{ty})"
        );
        let (sx, sy) = scan_find(&mut kit, &mut panel, ids::TILE_STAY_AWAKE, false)
            .expect("stay-awake tile never hit");
        let (sx, sy) = (sx + 130.0, sy + 28.0);
        eprintln!("E2E_STAY_AWAKE_PANEL_POS={sx},{sy}");
        panel.body.scroll = max;
        kit.layout(&mut panel);
        let (px, py) = scan_find(&mut kit, &mut panel, ids::profile(2), false)
            .expect("performance button never hit");
        let (px, py) = (px + 60.0, py + 15.0);
        eprintln!("E2E_PERF_BUTTON_PANEL_POS={px},{py}");
    }

    /// Tier 2.6: the parser must accept the frozen backend WeatherClient
    /// snapshot shape (weather.rs `snapshot()`), including the automatic
    /// subtitle rule and the 3-day cap.
    #[test]
    fn weather_state_parses_backend_snapshot() {
        let snap = serde_json::json!({
            "available": true,
            "configured": true,
            "automatic": false,
            "loading": false,
            "stale": false,
            "error": "",
            "location": {"name": "Hanoi", "country": "Vietnam",
                         "latitude": 21.03, "longitude": 105.85},
            "temperature": 29, "feelsLike": 32, "humidity": 76, "wind": 10,
            "condition": "Light rain", "icon": "weather-rain",
            "unit": "°C", "windUnit": "km/h", "updatedAt": 1760000000,
            "daily": [
                {"date": "2026-10-13", "min": 24, "max": 31, "rainChance": 60},
                {"date": "2026-10-14", "min": 24, "max": 30, "rainChance": 40},
                {"date": "2026-10-15", "min": 23, "max": 29, "rainChance": 20},
                {"date": "2026-10-16", "min": 23, "max": 29, "rainChance": 10}
            ]
        });
        let w = weather_state(&snap);
        assert!(w.available);
        assert_eq!(w.place_name, "Hanoi");
        assert_eq!(w.place_subtitle, "Vietnam"); // manual mode: country
        assert_eq!(w.temperature, Some(29.0));
        assert_eq!(w.icon, "weather-rain");
        assert_eq!(w.unit, "°C");
        assert_eq!(w.wind_unit, "km/h");
        assert_eq!(w.daily.len(), 3, "the card shows at most 3 days");
        assert_eq!(w.daily[2].date, "2026-10-15");
        // automatic mode: the IP-geolocation subtitle wins over the country
        let mut auto = snap.clone();
        auto["automatic"] = serde_json::json!(true);
        let w2 = weather_state(&auto);
        assert_eq!(w2.place_subtitle, "Approximate location · by IP");
        // null forecast numbers survive as None (the card prints "—")
        let mut nulls = snap.clone();
        nulls["temperature"] = serde_json::Value::Null;
        nulls["wind"] = serde_json::Value::Null;
        let w3 = weather_state(&nulls);
        assert_eq!(w3.temperature, None);
        assert_eq!(w3.wind, None);
    }

    /// days_from_civil math against the system calendar (2026-10-13 is a
    /// Tuesday; 1970-01-01 was a Thursday; 2000-02-29 a Tuesday).
    #[test]
    fn weekday_short_matches_calendar() {
        assert_eq!(weekday_short("2026-10-13"), Some("Tue"));
        assert_eq!(weekday_short("2026-10-14"), Some("Wed"));
        assert_eq!(weekday_short("2026-10-15"), Some("Thu"));
        assert_eq!(weekday_short("1970-01-01"), Some("Thu"));
        assert_eq!(weekday_short("2000-02-29"), Some("Tue"));
        assert_eq!(weekday_short("2024-12-31"), Some("Tue"));
        assert_eq!(weekday_short("not-a-date"), None);
        assert_eq!(weekday_short("2026-13-01"), None);
        assert_eq!(weekday_short("2026-02-30"), None);
    }

    /// No weather feed -> no card (like the media card), and the refresh
    /// button position is locked for the e2e pointer spec.
    #[test]
    fn weather_card_states_and_refresh_position() {
        // 1) hidden without a feed
        let mut kit = Kit::new();
        let mut d = data();
        d.weather = None;
        let mut panel = ControlCenterPanel::new(&kit.theme, &d, 0);
        kit.layout(&mut panel);
        let hit = kit.click_ids_by_scan(&mut panel);
        assert!(
            !hit.contains(&ids::WEATHER_REFRESH),
            "weather card must be hidden without a feed"
        );

        // 2) loading state keeps the refresh button (Updating…), error state
        // too — the click is always available, the daemon decides what it does
        let mut d2 = data();
        let mut wx = d2.weather.clone().unwrap();
        wx.available = false;
        wx.loading = true;
        wx.place_name.clear();
        d2.weather = Some(wx);
        let mut panel2 = ControlCenterPanel::new(&kit.theme, &d2, 0);
        kit.layout(&mut panel2);
        let hit2 = kit.click_ids_by_scan(&mut panel2);
        assert!(
            hit2.contains(&ids::WEATHER_REFRESH),
            "loading card keeps the refresh button"
        );

        // 3) lock the refresh-button panel coords for the e2e pointer spec
        let mut kit3 = Kit::new();
        let mut panel3 = ControlCenterPanel::new(&kit3.theme, &data(), 0);
        kit3.layout(&mut panel3);
        let mut pos: Option<(f32, f32)> = None;
        let mut y = 4.0;
        while y < 736.0 && pos.is_none() {
            let mut x = 8.0;
            while x < PANEL_W - 4.0 {
                let p = Point::new(x, y);
                let before = kit3.queue.len();
                let mut c = kit3.ctx();
                panel3.deliver(&mut c, &InputEvent::down(p), None);
                panel3.deliver(&mut c, &InputEvent::up(p), None);
                if kit3.queue[before..]
                    .iter()
                    .any(|e| matches!(e, UiEvent::Click(id) if *id == ids::WEATHER_REFRESH))
                {
                    pos = Some((x + 30.0, y + 15.0)); // center of the button
                    break;
                }
                x += 4.0;
            }
            y += 4.0;
        }
        let Some((x, y)) = pos else {
            panic!("refresh button never hit by the scan")
        };
        eprintln!("E2E_WEATHER_REFRESH_PANEL_POS={x},{y}");
        assert!(
            y > 200.0 && y < 380.0,
            "refresh button moved: ({x},{y}) — update the e2e pointer spec"
        );
    }

    /// Tier 2.6 / KI-3: the tab content scrolls (Flickable), the offset
    /// survives a data rebuild on the SAME tab, and switching tabs resets
    /// it — the daemon's 5 s refresh must never snap the viewport to the
    /// top.
    #[test]
    fn tab_content_scrolls_and_rebuild_keeps_offset() {
        let mut kit = Kit::new();
        let mut panel = ControlCenterPanel::new(&kit.theme, &data(), 0);
        kit.layout(&mut panel);
        assert!(
            panel.body.max_scroll() > 0.0,
            "content must overflow at 740"
        );
        // wheel down over the viewport center
        let vp_center = lme_ui::geom::Point::new(300.0, 400.0);
        {
            let mut c = kit.ctx();
            panel.deliver(&mut c, &InputEvent::Scroll(vp_center, 3.0), None);
        }
        let scrolled = panel.body.scroll;
        assert!(scrolled > 50.0, "wheel must scroll, got {scrolled}");
        // same-tab rebuild keeps the offset
        panel.rebuild(&kit.theme, &data(), 0);
        assert_eq!(
            panel.body.scroll, scrolled,
            "data refresh must keep the scroll offset"
        );
        // tab switch resets it
        panel.rebuild(&kit.theme, &data(), 3);
        assert_eq!(panel.body.scroll, 0.0, "tab switch resets the scroll");
        assert_eq!(panel.current_tab, 3);
    }
}
