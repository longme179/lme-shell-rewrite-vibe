// SPDX-License-Identifier: GPL-3.0-or-later
//! Pre-rendered 9-slice drop shadows. Shadows are rasterized once per
//! (corner-radius, blur-extent, color) combination and stretched as nine
//! slices at paint time — never a per-frame blur (mission quality bar).

use crate::color::Color;
use crate::geom::Rect;
use std::collections::HashMap;
use std::sync::Arc;
use tiny_skia::{FillRule, Paint, Pixmap, Transform};

/// A rendered 9-slice patch: shadow of a rounded square of `patch*2` extent.
pub struct ShadowSlice {
    pub pixmap: Pixmap,
    /// Slice inset in px: corners are `patch` wide/tall.
    pub patch: u32,
}

type SliceKey = (u32, u32, (u8, u8, u8, u8));

/// Cache: keyed by (radius ×4, extent, color rgba8).
pub struct ShadowCache {
    slices: HashMap<SliceKey, Arc<ShadowSlice>>,
    /// Render scale (Tier 0.9): slices are rendered at physical radius /
    /// patch; the 9-slice stretch rect is scaled by the renderer.
    render_scale: f32,
}

impl ShadowCache {
    pub fn new() -> Self {
        Self {
            slices: HashMap::new(),
            render_scale: 1.0,
        }
    }

    /// Set the render scale for subsequent `get`s (Tier 0.9).
    pub fn set_render_scale(&mut self, scale: f32) {
        self.render_scale = if scale.is_finite() && (0.5..=10.0).contains(&scale) {
            scale
        } else {
            1.0
        };
    }

    /// Get or render the 9-slice for a shadow whose source shape is a
    /// rounded square of side `2*patch` with corner `radius` (LOGICAL px;
    /// rendered at physical radius × patch so slices stay sharp).
    pub fn get(&mut self, radius: f32, patch: u32, color: Color) -> Arc<ShadowSlice> {
        let radius = radius * self.render_scale;
        let patch = (patch as f32 * self.render_scale).round().max(1.0) as u32;
        let key = ((radius * 4.0).round() as u32, patch, color.to_rgba8());
        if let Some(hit) = self.slices.get(&key) {
            return Arc::clone(hit);
        }
        let slice = Arc::new(render_slice(radius, patch, color));
        // Bounded: real UIs use a handful of shadow styles.
        if self.slices.len() > 64 {
            self.slices.clear();
        }
        self.slices.insert(key, Arc::clone(&slice));
        slice
    }
}

impl Default for ShadowCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one 9-slice: a blurred rounded square. Source image is 3×patch
/// wide/tall with the stamp centered, so the blur (extent ≤ patch) decays to
/// zero before the image edges — corners composite cleanly. Computed once;
/// this is the only place shadow math runs.
fn render_slice(radius: f32, patch: u32, color: Color) -> ShadowSlice {
    let patch = patch.max(8);
    let side = (patch * 4) as i32;
    let mut pixmap = Pixmap::new(side as u32, side as u32).expect("shadow pixmap");
    // Soft falloff: three box-blur passes ≈ gaussian, extent ≤ patch.
    let r = radius.min(patch as f32 / 2.0);
    let stamp_side = 2.0 * patch as f32; // solid interior for the center slice
    let path =
        crate::geom::rounded_rect_path(patch as f32, patch as f32, stamp_side, stamp_side, r);
    // 1) stamp the rounded square alpha
    let mut alpha = vec![0f32; (side * side) as usize];
    {
        let mut paint = Paint::default();
        let (cr, cg, cb, _ca) = color.to_rgba8();
        paint.set_color_rgba8(cr, cg, cb, 255);
        paint.anti_alias = true;
        let mut stamp = Pixmap::new(side as u32, side as u32).expect("stamp");
        stamp.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        for (i, px) in stamp.pixels().iter().enumerate() {
            alpha[i] = px.alpha() as f32 / 255.0;
        }
    }
    // 2) three box-blur passes on the scalar alpha (≈ gaussian, run once)
    let passes = 3;
    let radius_px = (patch as i32 / 3).max(1);
    for _ in 0..passes {
        alpha = box_blur(&alpha, side, side, radius_px);
    }
    // 3) write out premultiplied color with blurred alpha
    let (cr, cg, cb, _ca) = color.to_rgba8();
    let out = pixmap.pixels_mut();
    for (i, a) in alpha.iter().enumerate() {
        let a8 = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        let pr = (cr as u32 * a8 as u32 + 127) / 255;
        let pg = (cg as u32 * a8 as u32 + 127) / 255;
        let pb = (cb as u32 * a8 as u32 + 127) / 255;
        out[i] = tiny_skia::PremultipliedColorU8::from_rgba(pr as u8, pg as u8, pb as u8, a8)
            .expect("valid premult");
    }
    ShadowSlice { pixmap, patch }
}

/// Separable box blur on f32 alpha (single channel), sliding window.
fn box_blur(src: &[f32], w: i32, h: i32, r: i32) -> Vec<f32> {
    let mut tmp = vec![0f32; src.len()];
    let mut dst = vec![0f32; src.len()];
    // horizontal
    for y in 0..h {
        let mut acc = 0f32;
        let row = (y * w) as usize;
        for x in -r..=r {
            let xi = x.clamp(0, w - 1) as usize;
            acc += src[row + xi];
        }
        let n = (2 * r + 1) as f32;
        for x in 0..w {
            tmp[row + x as usize] = acc / n;
            let x_add = (x + r + 1).clamp(0, w - 1) as usize;
            let x_sub = (x - r).clamp(0, w - 1) as usize;
            acc += src[row + x_add] - src[row + x_sub];
        }
    }
    // vertical
    for x in 0..w {
        let mut acc = 0f32;
        for y in -r..=r {
            let yi = y.clamp(0, h - 1) as usize;
            acc += tmp[yi * w as usize + x as usize];
        }
        let n = (2 * r + 1) as f32;
        for y in 0..h {
            dst[y as usize * w as usize + x as usize] = acc / n;
            let y_add = (y + r + 1).clamp(0, h - 1) as usize;
            let y_sub = (y - r).clamp(0, h - 1) as usize;
            acc += tmp[y_add * w as usize + x as usize] - tmp[y_sub * w as usize + x as usize];
        }
    }
    dst
}

/// Convenience: the rect covered by a shadow of `extent` around `rect`.
pub fn shadow_rect(rect: Rect, extent: f32) -> Rect {
    Rect::new(
        rect.x - extent,
        rect.y - extent,
        rect.w + 2.0 * extent,
        rect.h + 2.0 * extent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_renders_and_caches() {
        let mut c = ShadowCache::new();
        let a = c.get(24.0, 40, Color::rgb(0.0, 0.0, 0.0));
        let b = c.get(24.0, 40, Color::rgb(0.0, 0.0, 0.0));
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.pixmap.width(), 160);
        // Corner must be transparent (blur decayed), stamp center opaque.
        let corner = a.pixmap.pixel(1, 1).unwrap();
        assert!(corner.alpha() < 6, "corner alpha {}", corner.alpha());
        let center = a.pixmap.pixel(80, 80).unwrap();
        assert!(center.alpha() > 200, "center alpha {}", center.alpha());
    }
}
