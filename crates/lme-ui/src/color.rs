// SPDX-License-Identifier: GPL-3.0-or-later
//! Straight sRGB color (non-premultiplied) matching QML's Qt.rgba semantics.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Parse `#rrggbb` (hex, case-insensitive) — palette.json format.
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#')?;
        if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self::from_rgba8(
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
            255,
        ))
    }

    pub fn alpha(self, opacity: f32) -> Self {
        Self {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a * opacity,
        }
    }

    /// Theme.mix: straight lerp toward `b` by `amount`, alpha stays 1.
    pub fn mix(self, b: Color, amount: f32) -> Self {
        Self::rgb(
            self.r * (1.0 - amount) + b.r * amount,
            self.g * (1.0 - amount) + b.g * amount,
            self.b * (1.0 - amount) + b.b * amount,
        )
    }

    pub fn to_rgba8(self) -> (u8, u8, u8, u8) {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        (c(self.r), c(self.g), c(self.b), c(self.a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_convert() {
        let c = Color::parse_hex("#9580FF").expect("hex");
        assert_eq!(c.to_rgba8().0, 0x95);
        assert_eq!(c.to_rgba8().1, 0x80);
        assert_eq!(c.to_rgba8().2, 0xff);
        assert!(Color::parse_hex("#9580").is_none());
        assert!(Color::parse_hex("zzzzzz").is_none());
        assert!(Color::parse_hex("9580ff").is_none());
    }

    #[test]
    fn mix_matches_qml_semantics() {
        let a = Color::rgb(0.2, 0.2, 0.2);
        let b = Color::rgb(1.0, 0.0, 0.5);
        let m = a.mix(b, 0.5);
        assert!((m.r - 0.6).abs() < 1e-6 && (m.g - 0.1).abs() < 1e-6);
    }
}
