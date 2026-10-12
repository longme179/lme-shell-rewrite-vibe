// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-output top bar: one layer-shell surface (namespace `lme-bar`) per
//! output, three zones from `barLayout`, connected pill / islands styles,
//! module rows ported from `qml/panels/Bar.qml` + `qml/bar/BarModule.qml`.

use crate::daemon::Shell;
use lme_ui::color::Color;
use lme_ui::font_weight;
use lme_ui::geom::{Rect, Size};
use lme_ui::wayland::{KeyboardInteractivity, Layer, UiState, WindowSpec};
use lme_ui::widgets::{
    font_weight::MEDIUM, Button, HBox, Icon, Label, Spacer, WheelRegion, Widget,
};
use serde_json::Value;

/// Widget ids for bar module interactions (clicks routed through the shell).
pub mod ids {
    pub const LAUNCHER: u64 = 0x1000;
    pub const OVERVIEW: u64 = 0x1001;
    pub const NOTIFICATIONS: u64 = 0x1002;
    pub const SETTINGS: u64 = 0x1003;
    pub const CLIPBOARD: u64 = 0x1004;
    pub const AUDIO: u64 = 0x1005;
    pub const NETWORK: u64 = 0x1006;
    pub const BATTERY: u64 = 0x1007;
    pub const TRAY: u64 = 0x1008;
    /// Wheel events over the workspaces row (finding e: focus up/down).
    pub const WORKSPACES: u64 = 0x1009;
    pub const WORKSPACE_BASE: u64 = 0x2000; // + workspace id
}

pub struct BarLayout {
    pub left: Vec<String>,
    pub center: Vec<String>,
    pub right: Vec<String>,
}

impl BarLayout {
    pub fn from_prefs(prefs: &serde_json::Map<String, Value>) -> BarLayout {
        let default = BarLayout {
            left: ["launcher", "overview", "workspaces"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            center: vec!["activity".to_string()],
            right: [
                "clipboard",
                "tray",
                "notifications",
                "audio",
                "network",
                "battery",
                "clock",
                "settings",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        };
        let Some(layout) = prefs.get("barLayout").and_then(Value::as_object) else {
            return default;
        };
        let zone = |key: &str, fallback: Vec<String>| -> Vec<String> {
            layout
                .get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or(fallback)
        };
        BarLayout {
            left: zone("left", default.left),
            center: zone("center", default.center),
            right: zone("right", default.right),
        }
    }
}

/// Create the bar window for one output.
pub fn create_bar(
    state: &mut UiState,
    shell: &mut Shell,
    output: &lme_ui::wayland::OutputRec,
) -> u64 {
    let theme = &shell.theme;
    let spec = WindowSpec {
        namespace: "lme-bar".into(),
        layer: Layer::Top,
        // (top, right, bottom, left)
        anchor: (!theme.bar_bottom, true, theme.bar_bottom, true),
        exclusive_zone: theme.bar_height as i32,
        // width 0 = "derive from output" (anchored both horizontal edges)
        size: Some((0, theme.bar_height as u32)),
        keyboard: KeyboardInteractivity::None,
        margins: (
            if theme.bar_bottom {
                0
            } else {
                theme.bar_top_margin as i32
            },
            theme.bar_side_margin as i32,
            if theme.bar_bottom {
                theme.bar_top_margin as i32
            } else {
                0
            },
            theme.bar_side_margin as i32,
        ),
        output: Some(output.output.clone()),
        background: Color::rgba(0.0, 0.0, 0.0, 0.0),
    };
    let id = state.create_window(spec);
    let bar_width = output.logical.0.max(1) as f32 - 2.0 * theme.bar_side_margin;
    let output_name = output.name.clone();
    let root = build_bar_root(shell, &output_name, bar_width);
    state.set_root(id, root);
    id
}

/// The full bar widget tree.
pub fn build_bar_root(shell: &Shell, output_name: &str, bar_width: f32) -> Box<dyn Widget> {
    let theme = &shell.theme;
    let layout = BarLayout::from_prefs(&shell.settings.values);
    let compact = bar_width < 1200.0;
    let row_h = theme.bar_height;

    let mut row = HBox::new(4.0).padded(6.0);
    row = row.push(Spacer::fixed(4.0, 0.0));
    row = row.push(build_group(
        shell,
        &layout.left,
        output_name,
        compact,
        bar_width * 0.39,
    ));
    row = row.push(Spacer::fill());
    row = row.push(build_group(
        shell,
        &layout.center,
        output_name,
        compact,
        bar_width * 0.2,
    ));
    row = row.push(Spacer::fill());
    row = row.push(build_group(
        shell,
        &layout.right,
        output_name,
        compact,
        bar_width * 0.58,
    ));
    row = row.push(Spacer::fixed(4.0, 0.0));

    Box::new(BarRoot {
        row,
        connected: shell
            .settings
            .values
            .get("barStyle")
            .and_then(Value::as_str)
            != Some("islands"),
        rect: Rect::default(),
        fill: theme.bar_fill,
        opacity: theme.panel_opacity,
        radius: row_h / 2.0,
    })
}

fn build_group(
    shell: &Shell,
    modules: &[String],
    output_name: &str,
    compact: bool,
    budget: f32,
) -> Box<dyn Widget> {
    let mut group = HBox::new(4.0);
    for module_id in modules {
        if let Some(w) = build_module(shell, module_id, output_name, compact, budget) {
            group = group.push(w);
        }
    }
    Box::new(group)
}

/// One module row (port of BarModule.qml). `None` = module not available.
fn build_module(
    shell: &Shell,
    module_id: &str,
    output_name: &str,
    compact: bool,
    _budget: f32,
) -> Option<Box<dyn Widget>> {
    let theme = &shell.theme;
    let size = theme.bar_font_size;
    let muted = theme.bar_muted;
    let text = theme.bar_text;
    match module_id {
        // action buttons (30×30 icon buttons)
        "launcher" => Some(action_button(
            shell,
            ids::LAUNCHER,
            " Apps",
            "application launcher",
        )),
        "overview" => Some(action_button(shell, ids::OVERVIEW, "⌘", "overview")),
        "notifications" => Some(action_button(
            shell,
            ids::NOTIFICATIONS,
            "🔔",
            "notifications",
        )),
        "settings" => Some(action_button(shell, ids::SETTINGS, "⚙", "settings")),
        "clipboard" => Some(action_button(shell, ids::CLIPBOARD, "📋", "clipboard")),
        "tray" => {
            let count = shell.tray_items;
            if count == 0 {
                None
            } else {
                Some(action_button(shell, ids::TRAY, "▤", "tray"))
            }
        }
        "clock" => {
            let label = crate::daemon::clock_label(compact);
            Some(Box::new(
                HBox::new(0.0)
                    .push(Spacer::fixed(10.0, 0.0))
                    .push(Label::styled(label, size, MEDIUM, text))
                    .push(Spacer::fixed(10.0, 0.0)),
            ))
        }
        "windowTitle" => {
            let title = if shell.niri.available() {
                let t = shell.niri.focused_title();
                if t.is_empty() {
                    "Desktop".to_string()
                } else {
                    t
                }
            } else {
                "niri unavailable".to_string()
            };
            let mut l = Label::styled(title, size, MEDIUM, muted);
            l.max_width = Some(360.0);
            Some(Box::new(
                HBox::new(0.0).push(Spacer::fixed(8.0, 0.0)).push(l),
            ))
        }
        "workspaces" => {
            let workspaces = shell.niri.workspaces_for(output_name);
            if workspaces.is_empty() {
                return None;
            }
            let mut row = HBox::new(4.0);
            for ws in &workspaces {
                let focused = ws
                    .get("is_focused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let ws_id = ws.get("id").and_then(Value::as_i64).unwrap_or(0);
                let name = ws
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        ws.get("idx")
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                            .to_string()
                    });
                let (w, radius, fill, color) = if focused {
                    (42.0, 12.0, theme.workspace_active, theme.workspace_text)
                } else {
                    (34.0, 15.0, Color::rgba(0.0, 0.0, 0.0, 0.0), theme.bar_muted)
                };
                let mut b = Button::new(ids::WORKSPACE_BASE + ws_id.max(0) as u64, name)
                    .with_fill(fill)
                    .with_text(color)
                    .with_radius(radius);
                b.label_size = size;
                b.label_weight = MEDIUM;
                b.min_h = 30.0;
                b.min_w = w;
                row = row.push(b);
            }
            // NEW behavior (finding e): wheel over the workspace row focuses
            // the previous/next workspace. Legacy QML has no equivalent.
            Some(Box::new(WheelRegion::new(ids::WORKSPACES, Box::new(row))))
        }
        "battery" => {
            let battery = shell
                .system_snapshot
                .get("battery")
                .cloned()
                .unwrap_or(Value::Null);
            let available = battery
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !available {
                return None;
            }
            let charging = battery
                .get("charging")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let percent = battery
                .get("percent")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let caption = format!(
                "{}{}%",
                if charging { "+" } else { "" },
                percent.round() as i64
            );
            let icon_color = if percent < 20.0 { theme.red } else { muted };
            Some(Box::new(status_button(
                shell,
                ids::BATTERY,
                "🔋",
                icon_color,
                caption,
            )))
        }
        "network" => {
            let network = shell
                .system_snapshot
                .get("network")
                .cloned()
                .unwrap_or(Value::Null);
            let connected = network
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let caption = if !connected {
                "Offline".to_string()
            } else if network.get("ssid").and_then(Value::as_str) == Some("Ethernet") {
                "Ethernet".to_string()
            } else {
                "Wi-Fi".to_string()
            };
            Some(Box::new(status_button(
                shell,
                ids::NETWORK,
                "📶",
                muted,
                caption,
            )))
        }
        "audio" => {
            let audio = shell
                .system_snapshot
                .get("audio")
                .cloned()
                .unwrap_or(Value::Null);
            let available = audio
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let caption = if !available {
                "—".to_string()
            } else if audio.get("muted").and_then(Value::as_bool).unwrap_or(false) {
                "Mute".to_string()
            } else {
                format!(
                    "{}%",
                    audio
                        .get("volume")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                        .round() as i64
                )
            };
            // Hover+click opens the control center notice (finding a);
            // wheel previews volume ±5%/notch in dry-run (finding e).
            Some(Box::new(WheelRegion::new(
                ids::AUDIO,
                Box::new(status_button(shell, ids::AUDIO, "🔊", muted, caption)),
            )))
        }
        "resources" => {
            let cpu = shell
                .system_snapshot
                .get("cpu")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let ram = shell
                .system_snapshot
                .get("memoryPercent")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            Some(Box::new(
                HBox::new(6.0)
                    .push(Spacer::fixed(10.0, 0.0))
                    .push(Icon::new("📊", 16.0, muted))
                    .push(Label::styled(
                        format!("{}% CPU · {}% RAM", cpu.round() as i64, ram.round() as i64),
                        size,
                        MEDIUM,
                        text,
                    ))
                    .push(Spacer::fixed(10.0, 0.0)),
            ))
        }
        // media / activity: real feeds arrive with the Tier 2 services
        // (MPRIS, timer) — the rows render when data exists.
        "media" => shell.media_title.as_ref().map(|title| {
            Box::new(
                HBox::new(6.0)
                    .push(Spacer::fixed(10.0, 0.0))
                    .push(Icon::new("▶", 16.0, muted))
                    .push(Label::styled(title.clone(), size, MEDIUM, text))
                    .push(Spacer::fixed(10.0, 0.0)),
            ) as Box<dyn Widget>
        }),
        "activity" => {
            if shell
                .settings
                .values
                .get("activityEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                Some(action_button(shell, ids::AUDIO, "◐", "activity island"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn action_button(shell: &Shell, id: u64, glyph: &str, _tooltip: &str) -> Box<dyn Widget> {
    let theme = &shell.theme;
    let mut b = Button::new(id, glyph);
    b.label_size = 15.0;
    b.label_weight = font_weight::NORMAL;
    b.fill = Color::rgba(0.0, 0.0, 0.0, 0.0);
    b.hover_fill = Some(theme.bar_hover);
    b.text_color = theme.bar_text;
    b.radius = 15.0;
    b.min_h = 30.0;
    b.min_w = 30.0;
    Box::new(b)
}

/// Status-module pill (audio / network / battery): icon + caption that acts
/// as a hover+click button (finding a). Until the Tier-2 panels exist the
/// daemon answers the click with an OSD notice naming the panel.
fn status_button(
    shell: &Shell,
    id: u64,
    glyph: &str,
    icon_color: Color,
    caption: String,
) -> Button {
    let theme = &shell.theme;
    let mut b = Button::new(id, caption);
    b.icon = Some((glyph.to_string(), 16.0));
    b.icon_color = Some(icon_color);
    b.label_size = theme.bar_font_size;
    b.label_weight = MEDIUM;
    b.fill = Color::rgba(0.0, 0.0, 0.0, 0.0);
    b.hover_fill = Some(theme.bar_hover);
    b.text_color = theme.bar_text;
    b.radius = 15.0;
    b.min_h = 30.0;
    b
}

/// Root: draws the connected pill (islands render transparent groups).
struct BarRoot {
    row: HBox,
    connected: bool,
    rect: Rect,
    fill: Color,
    opacity: f32,
    radius: f32,
}

impl Widget for BarRoot {
    fn measure(&mut self, ctx: &mut lme_ui::widgets::Ctx) -> Size {
        self.row.measure(ctx)
    }
    fn layout(&mut self, ctx: &mut lme_ui::widgets::Ctx, bounds: Rect) {
        self.rect = bounds;
        self.row.layout(ctx, bounds);
    }
    fn paint(&mut self, ctx: &mut lme_ui::widgets::Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        use lme_ui::draw::DrawCmd;
        if self.connected {
            out.push(DrawCmd::RoundedRect {
                rect: self.rect,
                radius: self.radius,
                fill: Some(self.fill.alpha(self.opacity)),
                border: Some((ctx.theme.outline_variant, 1.0)),
                clip: None,
            });
        }
        self.row.paint(ctx, out);
    }
    fn deliver(
        &mut self,
        ctx: &mut lme_ui::widgets::Ctx,
        ev: &lme_ui::widgets::InputEvent,
        target: Option<u64>,
    ) -> bool {
        self.row.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.row.focusables(out)
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
