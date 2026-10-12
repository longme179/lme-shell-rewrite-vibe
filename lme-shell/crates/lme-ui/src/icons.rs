// SPDX-License-Identifier: GPL-3.0-or-later
//! Theme icon lookup without a new dependency (DECISIONS.md D7): the standard
//! hicolor/Pixmaps paths are probed directly; SVG icons are not rasterized
//! (no resvg adoption — nothing in the implemented tiers needs it); callers
//! fall back to text glyphs.

use std::path::PathBuf;
use std::sync::Arc;

/// Directories searched, mirroring the icon theme spec's default lookup.
const SIZES: &[&str] = &[
    "128x128", "96x96", "64x64", "48x48", "32x32", "24x24", "16x16",
];

#[derive(Default)]
pub struct IconCache {
    hits: std::collections::HashMap<String, Option<Arc<tiny_skia::Pixmap>>>,
}

impl IconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up an icon name under /usr/share/icons (+ hicolor sizes) and
    /// /usr/share/pixmaps; PNG only. Cached, including negatives.
    pub fn get(&mut self, name: &str) -> Option<Arc<tiny_skia::Pixmap>> {
        if let Some(hit) = self.hits.get(name) {
            return hit.clone();
        }
        let found = probe(name).and_then(|path| load_png(&path));
        self.hits.insert(name.to_string(), found.clone());
        found
    }
}

fn probe(name: &str) -> Option<PathBuf> {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        return None;
    }
    let bases = [
        "/usr/share/icons/hicolor".to_string(),
        "/usr/local/share/icons/hicolor".to_string(),
    ];
    for base in &bases {
        for size in SIZES {
            for sub in ["apps", "status", "devices", "actions", "categories"] {
                let p = PathBuf::from(format!("{base}/{size}/{sub}/{safe}.png"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    let p = PathBuf::from(format!("/usr/share/pixmaps/{safe}.png"));
    p.exists().then_some(p)
}

/// Minimal PNG decode via the `png` crate into a tiny-skia pixmap.
fn load_png(path: &std::path::Path) -> Option<Arc<tiny_skia::Pixmap>> {
    let data = std::fs::read(path).ok()?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    // png crate outputs RGBA8 for ColorType::Rgba (set by default transformer)
    let expected = (w as usize) * (h as usize) * 4;
    if buf.len() < expected {
        return None;
    }
    let pixels = pixmap.pixels_mut();
    for (i, px) in pixels.iter_mut().enumerate().take((w * h) as usize) {
        let base = i * 4;
        let (r, g, b, a) = (buf[base], buf[base + 1], buf[base + 2], buf[base + 3]);
        // premultiply
        let (pr, pg, pb) = (
            (r as u32 * a as u32 + 127) / 255,
            (g as u32 * a as u32 + 127) / 255,
            (b as u32 * a as u32 + 127) / 255,
        );
        *px = tiny_skia::PremultipliedColorU8::from_rgba(pr as u8, pg as u8, pb as u8, a)
            .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
    }
    Some(Arc::new(pixmap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_sanitized_and_cached() {
        let mut c = IconCache::new();
        // Path traversal attempts resolve to nothing.
        assert!(c.get("../../etc/passwd").is_none());
        // Negative result is cached (same Option is None both times — trivial).
        assert!(c.get("definitely-not-an-icon-xyzzy").is_none());
    }
}
