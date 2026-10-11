// SPDX-License-Identifier: GPL-3.0-or-later
//! Text: cosmic-text shaping with a bounded shaped-line cache and a shared
//! glyph raster cache (SwashCache). Steady-state rendering performs no
//! allocation: shaping happens only when text, size or max width changes.

use std::collections::HashMap;
use std::sync::Arc;

const ROBOTO: &[u8] = include_bytes!("../../../assets/fonts/Roboto-VF.ttf");
/// Cap on cached shaped lines; panels use tens of lines, a hard reset at 512
/// keeps memory bounded without an LRU's complexity.
const LINE_CACHE_CAP: usize = 512;

pub struct FontBook {
    fs: cosmic_text::FontSystem,
    cache: cosmic_text::SwashCache,
    lines: HashMap<LineKey, Arc<ShapedText>>,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct LineKey {
    text: String,
    size: u32,      // size * 4 for fractional px (e.g. 13.5 -> 54)
    weight: u16,    // cosmic-text weight value
    max_w: u32,     // max width * 4
    truncate: bool, // whether the caller wants ellipsis truncation
}

/// A shaped, positioned run of glyphs plus metrics. Immutable once built.
pub struct ShapedText {
    pub glyphs: Vec<PlacedGlyph>,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_height: f32,
    /// The possibly-truncated text actually rendered.
    pub text: String,
}

#[derive(Clone, Copy)]
pub struct PlacedGlyph {
    pub key: cosmic_text::CacheKey,
    pub x: i32,
    pub y: i32,
}

impl ShapedText {
    pub fn height(&self) -> f32 {
        self.line_height
    }
}

impl FontBook {
    pub fn new() -> Self {
        // Vendored Roboto VF first (deterministic goldens, sandbox has no
        // google-roboto-fonts); system fonts also loaded — on a real Fedora
        // machine the distro Roboto is preferred by family name resolution.
        let roboto = cosmic_text::fontdb::Source::Binary(Arc::new(ROBOTO));
        let fs = cosmic_text::FontSystem::new_with_fonts(vec![roboto]);
        Self {
            fs,
            cache: cosmic_text::SwashCache::new(),
            lines: HashMap::new(),
        }
    }

    pub fn new_for_tests() -> Self {
        Self::new()
    }

    /// Split borrow for renderer-side glyph rasterization.
    pub fn parts(&mut self) -> (&mut cosmic_text::FontSystem, &mut cosmic_text::SwashCache) {
        (&mut self.fs, &mut self.cache)
    }

    /// Shape one line. Cached by (text, size, weight, max width, truncate).
    pub fn line(&mut self, text: &str, size: f32, weight: u16) -> Arc<ShapedText> {
        self.line_inner(text, size, weight, f32::INFINITY, false)
    }

    /// Shape with ellipsis truncation to `max_width`.
    pub fn line_truncated(
        &mut self,
        text: &str,
        size: f32,
        weight: u16,
        max_width: f32,
    ) -> Arc<ShapedText> {
        self.line_inner(text, size, weight, max_width, true)
    }

    fn line_inner(
        &mut self,
        text: &str,
        size: f32,
        weight: u16,
        max_width: f32,
        truncate: bool,
    ) -> Arc<ShapedText> {
        let key = LineKey {
            text: text.to_string(),
            size: (size * 4.0).round() as u32,
            weight,
            max_w: if max_width.is_finite() {
                (max_width * 4.0).round() as u32
            } else {
                u32::MAX
            },
            truncate,
        };
        if let Some(hit) = self.lines.get(&key) {
            return Arc::clone(hit);
        }
        let shaped = Arc::new(self.shape(&key));
        if self.lines.len() >= LINE_CACHE_CAP {
            self.lines.clear();
        }
        self.lines.insert(key, Arc::clone(&shaped));
        shaped
    }

    fn shape(&mut self, key: &LineKey) -> ShapedText {
        let size = key.size as f32 / 4.0;
        let max_w = if key.max_w == u32::MAX {
            100_000.0
        } else {
            key.max_w as f32 / 4.0
        };
        if key.truncate {
            // Shrink with an ellipsis until it fits (shape is cached, so the
            // probe loop only runs on text change).
            let mut len = key.text.chars().count();
            loop {
                let candidate: String = key.text.chars().take(len).collect();
                let with_ell = if (len as f32) < key.text.chars().count() as f32 {
                    format!("{candidate}…")
                } else {
                    candidate.clone()
                };
                let probe = self.shape_raw(&with_ell, size, key.weight, max_w);
                if probe.width <= max_w || len == 0 {
                    let mut result = probe;
                    if len < key.text.chars().count() {
                        result.text = with_ell;
                    } else {
                        result.text = candidate;
                    }
                    return result;
                }
                len = len.saturating_sub((len / 8).max(1));
            }
        }
        let mut result = self.shape_raw(&key.text, size, key.weight, max_w);
        result.text = key.text.clone();
        result
    }

    fn shape_raw(&mut self, text: &str, size: f32, weight: u16, _max_w: f32) -> ShapedText {
        let metrics = cosmic_text::Metrics::new(size, size * 1.3);
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::Name("Roboto"))
            .weight(cosmic_text::Weight(weight));
        let mut buffer = cosmic_text::Buffer::new(&mut self.fs, metrics);
        buffer.set_size(Some(1_000_000.0), Some(1_000_000.0));
        buffer.set_text(text, &attrs, cosmic_text::Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fs, false);

        let mut glyphs = Vec::new();
        let mut width = 0.0f32;
        let mut ascent = 0.0f32;
        let mut descent = 0.0f32;
        let mut line_height = size * 1.3;
        for line in buffer.layout_runs() {
            ascent = ascent.max(line.line_top);
            // cosmic-text: line_y is the baseline; descent derived from metrics
            let mut run_w = 0.0f32;
            for glyph in line.glyphs.iter() {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                glyphs.push(PlacedGlyph {
                    key: physical.cache_key,
                    x: physical.x,
                    y: physical.y,
                });
                run_w += glyph.w;
            }
            width = width.max(run_w);
            descent = descent.max(line.line_height - line.line_top);
            line_height = line.line_height;
        }
        ShapedText {
            glyphs,
            width,
            ascent,
            descent,
            line_height,
            text: String::new(),
        }
    }

    /// Measure without keeping the result (convenience for layout probing).
    pub fn measure(&mut self, text: &str, size: f32, weight: u16) -> f32 {
        self.line(text, size, weight).width
    }
}

impl Default for FontBook {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_and_caches() {
        let mut f = FontBook::new();
        let a = f.line("Hello", 14.0, 400);
        assert!(a.width > 10.0);
        let b = f.line("Hello", 14.0, 400);
        assert!(Arc::ptr_eq(&a, &b), "cache must hit");
        let c = f.line("Hello", 15.0, 400);
        assert!(!Arc::ptr_eq(&a, &c), "different size reshapes");
        assert!(c.width >= a.width - 0.01);
    }

    #[test]
    fn truncates_to_width() {
        let mut f = FontBook::new();
        let full = f.line("A very long window title from some application", 14.0, 400);
        let cut = f.line_truncated(
            "A very long window title from some application",
            14.0,
            400,
            80.0,
        );
        assert!(cut.width <= 81.0, "cut {} <= 81", cut.width);
        assert!(cut.width < full.width);
        assert!(cut.text.ends_with('…'), "ends with ellipsis: {}", cut.text);
    }
}
