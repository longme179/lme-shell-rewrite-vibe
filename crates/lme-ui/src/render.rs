// SPDX-License-Identifier: GPL-3.0-or-later
//! Render backends. `RenderBackend` is the documented seam for a future GPU
//! implementation; `CpuRenderer` (tiny-skia) is the validated backend
//! (ADR 0001). A GPU backend is intentionally NOT implemented: it cannot be
//! run or verified in this environment and untested GPU code is a liability
//! (mission decision 2).

use crate::color::Color;
use crate::draw::DrawCmd;
use crate::geom::rounded_rect_path;
use crate::shadow::ShadowSlice;
use crate::text::{FontBook, ShapedText};
use std::sync::Arc;
use tiny_skia::{FillRule, Paint, Path, Pixmap, Stroke, Transform};

/// Interface between the widget tree and a rasterizer.
pub trait RenderBackend {
    /// (Re)allocate the target. Contents become undefined.
    fn resize(&mut self, width: u32, height: u32);
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// Paint everything from scratch (full redraw, ADR 0001). `scale` is
    /// the fractional render scale (Tier 0.9): commands are LOGICAL px,
    /// the pixmap is physical (logical × scale); text glyphs are already
    /// physical (shaped at scale — see FontBook::set_render_scale).
    fn render(&mut self, cmds: &[DrawCmd], fonts: &mut FontBook, background: Color, scale: f32);
    /// The finished frame: BGRA, premultiplied, row stride = width*4 —
    /// exactly what wl_shm Argb8888 expects on little-endian.
    fn bgra(&self) -> &[u8];
}

/// CPU rasterizer over a tiny-skia pixmap. Buffers are reused across frames;
/// `render` performs no heap allocation of its own beyond command cloning.
pub struct CpuRenderer {
    pixmap: Pixmap,
    bgra: Vec<u8>,
}

impl CpuRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        let pixmap = Pixmap::new(width.max(1), height.max(1)).expect("pixmap alloc");
        let bgra = vec![0u8; pixmap.data().len()];
        Self { pixmap, bgra }
    }
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

impl RenderBackend for CpuRenderer {
    fn resize(&mut self, width: u32, height: u32) {
        if self.pixmap.width() == width.max(1) && self.pixmap.height() == height.max(1) {
            return; // reuse
        }
        self.pixmap = Pixmap::new(width.max(1), height.max(1)).expect("pixmap alloc");
        self.bgra = vec![0u8; self.pixmap.data().len()];
    }

    fn width(&self) -> u32 {
        self.pixmap.width()
    }

    fn height(&self) -> u32 {
        self.pixmap.height()
    }

    fn render(&mut self, cmds: &[DrawCmd], fonts: &mut FontBook, background: Color, scale: f32) {
        let s = if scale.is_finite() && scale >= 0.5 && scale <= 10.0 {
            scale
        } else {
            1.0
        };
        let bg = background.to_rgba8();
        self.pixmap
            .fill(tiny_skia::Color::from_rgba8(bg.0, bg.1, bg.2, bg.3));
        for cmd in cmds {
            match cmd {
                DrawCmd::RoundedRect {
                    rect,
                    radius,
                    fill,
                    border,
                    clip,
                } => {
                    let rect = rect.scaled(s);
                    let path = rounded_rect_path(rect.x, rect.y, rect.w, rect.h, radius * s);
                    let clip = clip.map(|c| c.scaled(s));
                    self.fill_path(&path, fill, clip);
                    if let Some((c, w)) = border {
                        self.stroke_path(&path, *c, *w * s, clip);
                    }
                }
                DrawCmd::Circle {
                    cx,
                    cy,
                    r,
                    fill,
                    clip,
                } => {
                    let path = crate::geom::circle_path(*cx * s, *cy * s, *r * s);
                    self.fill_path(&path, &Some(*fill), clip.map(|c| c.scaled(s)));
                }
                DrawCmd::Text {
                    pos,
                    line,
                    color,
                    clip,
                } => {
                    self.blit_text(
                        &pos.scaled(s),
                        line,
                        *color,
                        fonts,
                        clip.map(|c| c.scaled(s)),
                    );
                }
                DrawCmd::Shadow { rect, slice, clip } => {
                    self.blit_shadow(&rect.scaled(s), slice, clip.map(|c| c.scaled(s)));
                }
                DrawCmd::Image {
                    rect,
                    radius,
                    image,
                    clip,
                } => {
                    self.blit_pixmap(
                        &rect.scaled(s),
                        *radius * s,
                        image,
                        clip.map(|c| c.scaled(s)),
                    );
                }
            }
        }
        // RGBA (tiny-skia) -> BGRA (wl_shm Argb8888 on LE), in place of the copy buffer.
        let src = self.pixmap.data();
        let dst = &mut self.bgra;
        for (d, s) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(src.as_chunks::<4>().0.iter())
        {
            *d = [s[2], s[1], s[0], s[3]];
        }
    }

    fn bgra(&self) -> &[u8] {
        &self.bgra
    }
}

impl CpuRenderer {
    fn fill_path(&mut self, path: &Path, color: &Option<Color>, clip: Option<crate::geom::Rect>) {
        let Some(color) = color else { return };
        let mut paint = Paint::default();
        let (r, g, b, a) = color.to_rgba8();
        paint.set_color_rgba8(r, g, b, a);
        paint.anti_alias = true;
        let _ = clip;
        self.pixmap
            .fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    fn stroke_path(
        &mut self,
        path: &Path,
        color: Color,
        width: f32,
        _clip: Option<crate::geom::Rect>,
    ) {
        let mut paint = Paint::default();
        let (r, g, b, a) = color.to_rgba8();
        paint.set_color_rgba8(r, g, b, a);
        paint.anti_alias = true;
        self.pixmap.stroke_path(
            path,
            &paint,
            &Stroke {
                width,
                ..Default::default()
            },
            Transform::identity(),
            None,
        );
    }

    /// Composite a shaped text run: glyph masks are blended source-over with a
    /// constant color; color bitmaps (emoji) are blitted directly.
    fn blit_text(
        &mut self,
        pos: &crate::geom::Point,
        line: &ShapedText,
        color: Color,
        fonts: &mut FontBook,
        clip: Option<crate::geom::Rect>,
    ) {
        let (fs, cache) = fonts.parts();
        let c8 = color.to_rgba8();
        for g in &line.glyphs {
            let Some(img) = cache.get_image(fs, g.key) else {
                continue;
            };
            let ix = pos.x as i32 + g.x + img.placement.left;
            let iy = pos.y as i32 + g.y - img.placement.top;
            match img.content {
                cosmic_text::SwashContent::Mask => {
                    let (iw, ih) = (img.placement.width, img.placement.height);
                    let data = &img.data;
                    for row in 0..ih {
                        for col in 0..iw {
                            let alpha = data.get((row * iw + col) as usize).copied().unwrap_or(0);
                            if alpha == 0 {
                                continue;
                            }
                            let (px, py) = (ix + col as i32, iy + row as i32);
                            if !in_clip(px, py, clip) {
                                continue;
                            }
                            let idx = match pixel_index(&self.pixmap, px, py) {
                                Some(i) => i,
                                None => continue,
                            };
                            let dst = self.pixmap.pixels_mut()[idx];
                            blend_mask_px(self.pixmap.pixels_mut(), idx, dst, alpha, c8);
                        }
                    }
                }
                cosmic_text::SwashContent::Color => {
                    // swash color bitmaps are straight-alpha RGBA (e.g.
                    // emoji); premultiply while blitting.
                    let (iw, ih) = (img.placement.width, img.placement.height);
                    let data = &img.data;
                    for row in 0..ih {
                        for col in 0..iw {
                            let base = ((row * iw + col) * 4) as usize;
                            let (r, g, b, a) = (
                                data.get(base).copied().unwrap_or(0),
                                data.get(base + 1).copied().unwrap_or(0),
                                data.get(base + 2).copied().unwrap_or(0),
                                data.get(base + 3).copied().unwrap_or(0),
                            );
                            if a == 0 {
                                continue;
                            }
                            let (pr, pg, pb) = (
                                (r as u32 * a as u32 + 127) / 255,
                                (g as u32 * a as u32 + 127) / 255,
                                (b as u32 * a as u32 + 127) / 255,
                            );
                            let (px, py) = (ix + col as i32, iy + row as i32);
                            if !in_clip(px, py, clip) {
                                continue;
                            }
                            if let Some(idx) = pixel_index(&self.pixmap, px, py) {
                                self.pixmap.pixels_mut()[idx] =
                                    tiny_skia::PremultipliedColorU8::from_rgba(
                                        pr as u8, pg as u8, pb as u8, a,
                                    )
                                    .expect("valid premult");
                            }
                        }
                    }
                }
                cosmic_text::SwashContent::SubpixelMask => {}
            }
        }
    }

    /// 9-slice stretch of a pre-rendered shadow.
    fn blit_shadow(
        &mut self,
        rect: &crate::geom::Rect,
        slice: &ShadowSlice,
        clip: Option<crate::geom::Rect>,
    ) {
        let s = slice.patch as i32; // insets of the 9-slice
        let src = &slice.pixmap;
        let (sw, sh) = (src.width() as i32, src.height() as i32);
        let (x0, y0) = (rect.x as i32, rect.y as i32);
        let (x1, y1) = (rect.right() as i32, rect.bottom() as i32);
        let corners = [
            // (src rect, dst rect)
            (0i32, 0i32, s, s, x0, y0, x0 + s, y0 + s),
            (sw - s, 0, sw, s, x1 - s, y0, x1, y0 + s),
            (0, sh - s, s, sh, x0, y1 - s, x0 + s, y1),
            (sw - s, sh - s, sw, sh, x1 - s, y1 - s, x1, y1),
        ];
        for (sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1) in corners {
            self.blit_sub(src, sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1, clip, false);
        }
        // edges (stretched)
        self.blit_sub(src, s, 0, sw - s, s, x0 + s, y0, x1 - s, y0 + s, clip, true);
        self.blit_sub(
            src,
            s,
            sh - s,
            sw - s,
            sh,
            x0 + s,
            y1 - s,
            x1 - s,
            y1,
            clip,
            true,
        );
        self.blit_sub(src, 0, s, s, sh - s, x0, y0 + s, x0 + s, y1 - s, clip, true);
        self.blit_sub(
            src,
            sw - s,
            s,
            sw,
            sh - s,
            x1 - s,
            y0 + s,
            x1,
            y1 - s,
            clip,
            true,
        );
        // center
        self.blit_sub(
            src,
            s,
            s,
            sw - s,
            sh - s,
            x0 + s,
            y0 + s,
            x1 - s,
            y1 - s,
            clip,
            true,
        );
    }

    /// Blit a scaled sub-rect of `src` (premultiplied RGBA) onto the target,
    /// alpha-composited. `stretch` only toggles nearest/bilinear choice —
    /// nearest everywhere keeps shadows crisp and is cheap.
    #[allow(clippy::too_many_arguments)]
    fn blit_sub(
        &mut self,
        src: &Pixmap,
        sx0: i32,
        sy0: i32,
        sx1: i32,
        sy1: i32,
        dx0: i32,
        dy0: i32,
        dx1: i32,
        dy1: i32,
        clip: Option<crate::geom::Rect>,
        _stretch: bool,
    ) {
        if sx1 <= sx0 || sy1 <= sy0 || dx1 <= dx0 || dy1 <= dy0 {
            return;
        }
        let (dw, dh) = ((dx1 - dx0) as usize, (dy1 - dy0) as usize);
        let (sw_f, sh_f) = ((sx1 - sx0) as f32, (sy1 - sy0) as f32);
        for py in 0..dh as i32 {
            for px in 0..dw as i32 {
                let (dstx, dsty) = (dx0 + px, dy0 + py);
                if !in_clip(dstx, dsty, clip) {
                    continue;
                }
                let Some(idx) = pixel_index(&self.pixmap, dstx, dsty) else {
                    continue;
                };
                // nearest-neighbor source sample
                let sx = sx0 + ((px as f32 / dw as f32) * sw_f) as i32;
                let sy = sy0 + ((py as f32 / dh as f32) * sh_f) as i32;
                let sidx = match pixel_index(src, sx, sy) {
                    Some(i) => i,
                    None => continue,
                };
                let sc = src.pixels()[sidx];
                let sa = sc.alpha() as u32;
                if sa == 0 {
                    continue;
                }
                let dst = self.pixmap.pixels_mut()[idx];
                let (dr, dg, db, da) = (
                    dst.red() as u32,
                    dst.green() as u32,
                    dst.blue() as u32,
                    dst.alpha() as u32,
                );
                let inv = 255 - sa;
                let (sr, sg, sb) = (sc.red() as u32, sc.green() as u32, sc.blue() as u32);
                let or = (sr + (dr * inv + 127) / 255).min(255) as u8;
                let og = (sg + (dg * inv + 127) / 255).min(255) as u8;
                let ob = (sb + (db * inv + 127) / 255).min(255) as u8;
                let oa = (sa + (da * inv + 127) / 255).min(255) as u8;
                self.pixmap.pixels_mut()[idx] =
                    tiny_skia::PremultipliedColorU8::from_rgba(or, og, ob, oa)
                        .expect("valid premult");
            }
        }
    }

    /// Blit a whole pixmap into a rounded rect (app icons, images).
    fn blit_pixmap(
        &mut self,
        rect: &crate::geom::Rect,
        _radius: f32,
        image: &Arc<Pixmap>,
        clip: Option<crate::geom::Rect>,
    ) {
        // Draw via the 9-slice-less path: reuse blit_sub with the whole image,
        // then (for radius>0) we accept square corners inside the clip caller
        // provides. Rounded image corners are handled by clipping the region
        // caller-side for now (documented simplification).
        self.blit_sub(
            image,
            0,
            0,
            image.width() as i32,
            image.height() as i32,
            rect.x as i32,
            rect.y as i32,
            rect.right() as i32,
            rect.bottom() as i32,
            clip,
            true,
        );
    }
}

fn pixel_index(pm: &Pixmap, x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 || x >= pm.width() as i32 || y >= pm.height() as i32 {
        return None;
    }
    Some((y as u32 * pm.width() + x as u32) as usize)
}

fn in_clip(x: i32, y: i32, clip: Option<crate::geom::Rect>) -> bool {
    match clip {
        None => true,
        Some(c) => {
            (x as f32) >= c.x
                && (x as f32) < c.right()
                && (y as f32) >= c.y
                && (y as f32) < c.bottom()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blend_mask_px(
    pixels: &mut [tiny_skia::PremultipliedColorU8],
    idx: usize,
    _dst: tiny_skia::PremultipliedColorU8,
    alpha: u8,
    color: (u8, u8, u8, u8),
) {
    let dst = pixels[idx];
    let (dr, dg, db, da) = (
        dst.red() as u32,
        dst.green() as u32,
        dst.blue() as u32,
        dst.alpha() as u32,
    );
    // color.a folds into the glyph alpha (Text with color.alpha < 1)
    let sa = (alpha as u32 * color.3 as u32 + 127) / 255;
    if sa == 0 {
        return;
    }
    let inv = 255 - sa;
    let cr = (color.0 as u32 * sa + 127) / 255;
    let cg = (color.1 as u32 * sa + 127) / 255;
    let cb = (color.2 as u32 * sa + 127) / 255;
    let or = (cr + (dr * inv + 127) / 255).min(255) as u8;
    let og = (cg + (dg * inv + 127) / 255).min(255) as u8;
    let ob = (cb + (db * inv + 127) / 255).min(255) as u8;
    let oa = (sa + (da * inv + 127) / 255).min(255) as u8;
    pixels[idx] =
        tiny_skia::PremultipliedColorU8::from_rgba(or, og, ob, oa).expect("valid premult");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    #[test]
    fn renderer_clears_and_draws_rounded_rect() {
        let mut r = CpuRenderer::new(64, 64);
        let cmds = [DrawCmd::RoundedRect {
            rect: Rect::new(8.0, 8.0, 48.0, 48.0),
            radius: 12.0,
            fill: Some(Color::rgb(1.0, 0.0, 0.0)),
            border: None,
            clip: None,
        }];
        let mut fonts = FontBook::new_for_tests();
        r.render(&cmds, &mut fonts, Color::rgb(0.0, 0.0, 0.0), 1.0);
        assert_eq!(r.bgra().len(), 64 * 64 * 4);
        // center pixel should be red (BGRA in memory: B,G,R,A)
        let idx = (32 * 64 + 32) * 4;
        assert_eq!(&r.bgra()[idx..idx + 4], &[0, 0, 255, 255]);
        // corner (outside radius) should be background
        let idx = (9 * 64 + 9) * 4;
        assert_eq!(&r.bgra()[idx..idx + 4], &[0, 0, 0, 255]);
    }

    #[test]
    fn resize_reuses_or_reallocates() {
        let mut r = CpuRenderer::new(32, 32);
        r.resize(32, 32);
        assert_eq!(r.width(), 32);
        r.resize(48, 24);
        assert_eq!((r.width(), r.height()), (48, 24));
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;
    use crate::color::Color;
    use crate::draw::DrawCmd;
    use crate::geom::{Point, Rect};

    fn px_at(r: &CpuRenderer, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * r.pixmap.width() + x) * 4) as usize;
        let d = r.bgra();
        (d[i + 2], d[i + 1], d[i], d[i + 3]) // BGRA -> RGBA
    }

    #[test]
    fn rect_scales_with_render_scale() {
        // logical 128x32 surface at scale 1.25 -> 160x40 pixmap
        let mut r = CpuRenderer::new(160, 40);
        let cmd = DrawCmd::RoundedRect {
            rect: Rect::new(10.0, 10.0, 100.0, 20.0),
            radius: 0.0,
            fill: Some(Color::rgb(1.0, 1.0, 1.0)),
            border: None,
            clip: None,
        };
        r.render(&[cmd], &mut FontBook::new_for_tests(), Color::rgba(0.0, 0.0, 0.0, 0.0), 1.25);
        // center of the rect: logical (60,20) -> physical (75,25)
        assert_eq!(px_at(&r, 75, 25).3, 255, "rect center filled at 1.25x");
        // logical x=111 (outside the 10..110 rect) -> physical 139: empty
        assert_eq!(px_at(&r, 139, 25).3, 0, "right of rect empty");
        // logical x=111 at scale 1.0 would be 111 — the scaled surface is wider
        assert!(r.pixmap.width() == 160);
    }

    #[test]
    fn text_metrics_stay_logical_across_scales() {
        let mut f1 = FontBook::new_for_tests();
        let mut f2 = FontBook::new_for_tests();
        let w1 = f1.line("Shell", 13.0, 400).width;
        f2.set_render_scale(1.25);
        let w2 = f2.line("Shell", 13.0, 400).width;
        assert!(
            (w1 - w2).abs() < 1.5,
            "logical width must not change with scale: {w1} vs {w2}"
        );
    }
}

#[cfg(test)]
mod premul_tests {
    use super::*;
    use crate::color::Color;
    use crate::draw::DrawCmd;
    use crate::geom::Rect;
    use crate::text::FontBook;

    #[test]
    fn premultiplied_invariant_holds_at_fractional_scale() {
        let mut r = CpuRenderer::new(1600, 53);
        let cmds = vec![
            DrawCmd::RoundedRect {
                rect: Rect::new(0.0, 0.0, 1280.0, 42.0),
                radius: 0.0,
                fill: Some(Color::rgba(0.22, 0.20, 0.28, 0.9)),
                border: None,
                clip: None,
            },
            DrawCmd::RoundedRect {
                rect: Rect::new(1000.0, 8.0, 260.0, 26.0),
                radius: 13.0,
                fill: Some(Color::rgba(0.58, 0.50, 1.0, 0.9)),
                border: None,
                clip: None,
            },
        ];
        r.render(&cmds, &mut FontBook::new_for_tests(), Color::rgba(0.0, 0.0, 0.0, 0.0), 1.25);
        let d = r.bgra();
        for i in (0..d.len()).step_by(4) {
            let (b, g, rr, a) = (d[i], d[i + 1], d[i + 2], d[i + 3]);
            assert!(
                rr <= a && g <= a && b <= a,
                "premultiplication violated at px {}: rgb=({rr},{g},{b}) a={a}",
                i / 4
            );
        }
    }
}
