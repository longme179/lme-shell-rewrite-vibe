// SPDX-License-Identifier: GPL-3.0-or-later
//! Theme: the legacy `qml/theme/Theme.qml` ported 1:1 (colors, derived roles,
//! geometry, motion). Sources: Theme.qml, config/palette.example.json,
//! settings.json keys (Phase 0 + upstream additions read with fallbacks).

use crate::color::Color;
use serde_json::{Map, Value};
use std::path::Path;

/// The 12 overridable palette roles from palette.json (Theme.qml loadPalette).
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: Color,
    pub panel: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub purple: Color,
    pub pink: Color,
    pub cyan: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub border: Color,
}

impl Default for Palette {
    fn default() -> Self {
        // Dracula Pro-style defaults (Theme.qml).
        Self {
            background: Color::parse_hex("#22212c").unwrap(),
            panel: Color::parse_hex("#2d293d").unwrap(),
            surface: Color::parse_hex("#393447").unwrap(),
            text: Color::parse_hex("#f8f8f2").unwrap(),
            muted: Color::parse_hex("#aaa6c0").unwrap(),
            purple: Color::parse_hex("#9580ff").unwrap(),
            pink: Color::parse_hex("#ff80bf").unwrap(),
            cyan: Color::parse_hex("#80ffea").unwrap(),
            green: Color::parse_hex("#8aff80").unwrap(),
            yellow: Color::parse_hex("#ffff80").unwrap(),
            red: Color::parse_hex("#ff9580").unwrap(),
            border: Color::parse_hex("#605975").unwrap(),
        }
    }
}

impl Palette {
    /// Load palette.json: only valid `#rrggbb` values for the 12 known roles
    /// are accepted (invalid entries ignored, not fatal — Theme.qml parity).
    pub fn load(path: &Path) -> Palette {
        let mut p = Palette::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return p;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return p;
        };
        let Some(obj) = v.as_object() else { return p };
        for (key, target) in [
            ("background", &mut p.background),
            ("panel", &mut p.panel),
            ("surface", &mut p.surface),
            ("text", &mut p.text),
            ("muted", &mut p.muted),
            ("purple", &mut p.purple),
            ("pink", &mut p.pink),
            ("cyan", &mut p.cyan),
            ("green", &mut p.green),
            ("yellow", &mut p.yellow),
            ("red", &mut p.red),
            ("border", &mut p.border),
        ] {
            if let Some(hex) = obj.get(key).and_then(Value::as_str) {
                if let Some(c) = Color::parse_hex(hex) {
                    *target = c;
                }
            }
        }
        p
    }
}

/// Complete resolved theme. `from_prefs` mirrors Theme.qml bindings, reading
/// settings.json values with the same fallbacks (preferences may be missing
/// upstream-added keys — Theme.qml uses `||` fallbacks everywhere).
#[derive(Debug, Clone)]
pub struct Theme {
    // base palette
    pub background: Color,
    pub panel: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub purple: Color,
    pub pink: Color,
    pub cyan: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub border: Color,
    pub accent: Color,
    // derived roles (Theme.qml)
    pub primary_container: Color,
    pub secondary_container: Color,
    pub tertiary_container: Color,
    pub surface_container: Color,
    pub surface_container_high: Color,
    pub outline_variant: Color,
    pub on_primary: Color,
    pub button_fill: Color,
    pub button_hover: Color,
    pub button_pressed: Color,
    pub button_selected: Color,
    pub button_text: Color,
    pub icon_color: Color,
    pub icon_hover: Color,
    pub field_fill: Color,
    pub field_text: Color,
    pub switch_track: Color,
    pub switch_thumb: Color,
    pub slider_track: Color,
    pub slider_fill: Color,
    pub bar_fill: Color,
    pub bar_text: Color,
    pub bar_muted: Color,
    pub bar_hover: Color,
    pub workspace_active: Color,
    pub workspace_text: Color,
    pub panel_fill: Color,
    pub card_fill: Color,
    pub control_fill: Color,
    pub launcher_fill: Color,
    pub notification_fill: Color,
    pub island_fill: Color,
    pub settings_fill: Color,
    pub panel_opacity: f32,
    // geometry (settings.json with Theme.qml fallbacks)
    pub radius: f32,
    pub small_radius: f32,
    pub bar_height: f32,
    pub bar_font_size: f32,
    pub bar_bottom: bool,
    pub bar_top_margin: f32,
    pub bar_side_margin: f32,
    pub panel_top_margin: f32,
    pub body_size: f32,
    pub label_size: f32,
    pub compact: bool,
    pub card_padding: f32,
    pub card_spacing: f32,
    // motion (Theme.qml)
    pub animations: bool,
    pub animation_duration_ms: u32,
    pub slide_duration_ms: u32,
    pub spatial_curve: [f32; 4],
    pub effects_curve: [f32; 4],
    pub exit_curve: [f32; 4],
    pub font_family: &'static str,
}

fn pref_f32(prefs: &Map<String, Value>, key: &str, default: f32) -> f32 {
    prefs
        .get(key)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .unwrap_or(default)
}

fn pref_str<'a>(prefs: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    prefs.get(key).and_then(Value::as_str)
}

impl Theme {
    pub fn from_prefs(prefs: &Map<String, Value>, palette: &Palette) -> Theme {
        let p = palette;
        let accent = pref_str(prefs, "accent")
            .and_then(Color::parse_hex)
            .unwrap_or(p.purple);
        let primary_container = p.panel.mix(accent, 0.30);
        let secondary_container = p.panel.mix(p.pink, 0.22);
        let tertiary_container = p.panel.mix(p.cyan, 0.19);
        let surface_container = p.background.mix(p.surface, 0.66);
        let surface_container_high = p.panel.mix(p.surface, 0.74);
        let outline_variant = p.border.alpha(0.52);
        let on_primary = p.background;
        let button_fill = surface_container_high;
        let button_hover = button_fill.mix(p.text, 0.08);
        let button_pressed = button_fill.mix(p.text, 0.14);
        let button_selected = primary_container;
        let button_text = p.text;
        let icon_color = p.text;
        let icon_hover = p.text.alpha(0.09);
        let field_fill = surface_container;
        let field_text = p.text;
        let switch_track = accent;
        let switch_thumb = on_primary;
        let slider_track = surface_container_high;
        let slider_fill = accent;
        let bar_fill = surface_container;
        let bar_text = p.text;
        let bar_muted = p.muted;
        let bar_hover = primary_container;
        let workspace_active = accent;
        let workspace_text = on_primary;
        let panel_fill = surface_container;
        let card_fill = surface_container_high;
        let control_fill = panel_fill;
        let launcher_fill = panel_fill;
        let notification_fill = panel_fill;
        let island_fill = panel_fill;
        let settings_fill = panel_fill;

        let animations = prefs
            .get("animations")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let motion_speed = pref_f32(prefs, "motionSpeed", 1.0).max(0.1);
        let motion_bounce = prefs
            .get("motionBounce")
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .unwrap_or(0.35);
        let motion_preset = pref_str(prefs, "motionPreset").unwrap_or("expressive");
        let animation_duration_ms = if animations {
            (190.0 / motion_speed).round() as u32
        } else {
            0
        };
        let slide_duration_ms = if animations {
            let base = match motion_preset {
                "jelly" => 600.0,
                "smooth" => 360.0,
                _ => 460.0,
            };
            (base / motion_speed).round() as u32
        } else {
            0
        };
        // DankMaterialShell-derived curves (MIT; see third_party/ and Theme.qml).
        let spatial_curve: [f32; 4] = match motion_preset {
            "smooth" => [0.18, 0.8, 0.22, 1.0],
            "jelly" => [0.32, 1.0 + 0.9 * motion_bounce, 0.18, 1.0],
            _ => [0.38, 1.0 + 0.6 * motion_bounce, 0.22, 1.0],
        };
        let compact = pref_str(prefs, "density") == Some("compact");
        let bar_height = pref_f32(prefs, "barHeight", 42.0);
        let bar_top_margin = prefs
            .get("barEdgeMargin")
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .unwrap_or(8.0);

        Theme {
            background: p.background,
            panel: p.panel,
            surface: p.surface,
            text: p.text,
            muted: p.muted,
            purple: p.purple,
            pink: p.pink,
            cyan: p.cyan,
            green: p.green,
            yellow: p.yellow,
            red: p.red,
            border: p.border,
            accent,
            primary_container,
            secondary_container,
            tertiary_container,
            surface_container,
            surface_container_high,
            outline_variant,
            on_primary,
            button_fill,
            button_hover,
            button_pressed,
            button_selected,
            button_text,
            icon_color,
            icon_hover,
            field_fill,
            field_text,
            switch_track,
            switch_thumb,
            slider_track,
            slider_fill,
            bar_fill,
            bar_text,
            bar_muted,
            bar_hover,
            workspace_active,
            workspace_text,
            panel_fill,
            card_fill,
            control_fill,
            launcher_fill,
            notification_fill,
            island_fill,
            settings_fill,
            panel_opacity: prefs
                .get("opacity")
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .unwrap_or(0.90)
                .clamp(0.60, 1.0),
            radius: 28.0,
            small_radius: 16.0,
            bar_height,
            bar_font_size: pref_f32(prefs, "barFontSize", 13.0).clamp(12.0, 16.0),
            bar_bottom: pref_str(prefs, "barEdge") == Some("bottom"),
            bar_top_margin,
            bar_side_margin: prefs
                .get("barSideMargin")
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .unwrap_or(12.0),
            panel_top_margin: bar_top_margin + bar_height + 8.0,
            body_size: pref_f32(prefs, "fontSize", 14.0),
            label_size: pref_f32(prefs, "fontSize", 14.0) - 2.0,
            compact,
            card_padding: if compact { 14.0 } else { 20.0 },
            card_spacing: if compact { 10.0 } else { 16.0 },
            animations,
            animation_duration_ms,
            slide_duration_ms,
            spatial_curve,
            effects_curve: [0.34, 0.8, 0.34, 1.0],
            exit_curve: [0.3, 0.0, 0.8, 0.15],
            font_family: "Roboto",
        }
    }

    /// Theme with pure defaults (tests, previews).
    pub fn default_with(palette: &Palette) -> Theme {
        Theme::from_prefs(&Map::new(), palette)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_qml_fallbacks() {
        let t = Theme::default_with(&Palette::default());
        assert_eq!(t.radius, 28.0);
        assert_eq!(t.small_radius, 16.0);
        assert_eq!(t.bar_height, 42.0);
        assert_eq!(t.bar_font_size, 13.0);
        assert!((t.panel_top_margin - 58.0).abs() < 0.01);
        assert!(t.animations);
        assert_eq!(t.slide_duration_ms, 460); // expressive @ speed 1
        assert_eq!(t.accent, t.purple);
    }

    #[test]
    fn palette_overrides_apply() {
        let p = Palette {
            background: Color::parse_hex("#101014").unwrap(),
            ..Palette::default()
        };
        let t = Theme::default_with(&p);
        assert_eq!(t.background, p.background);
        // surface_container = mix(background, surface, 0.66)
        let expect = Color::parse_hex("#101014")
            .unwrap()
            .mix(Color::parse_hex("#393447").unwrap(), 0.66);
        assert!((t.surface_container.r - expect.r).abs() < 1e-6);
    }

    #[test]
    fn prefs_override_motion_and_bar() {
        let mut prefs = Map::new();
        prefs.insert("motionPreset".into(), Value::String("jelly".into()));
        prefs.insert("motionSpeed".into(), serde_json::json!(2.0));
        prefs.insert("barHeight".into(), serde_json::json!(50));
        prefs.insert("barEdge".into(), Value::String("bottom".into()));
        let t = Theme::from_prefs(&prefs, &Palette::default());
        assert_eq!(t.slide_duration_ms, 300); // 600/2
        assert_eq!(t.bar_height, 50.0);
        assert!(t.bar_bottom);
        assert!(t.spatial_curve[1] > 1.0, "jelly overshoots");
    }
}
