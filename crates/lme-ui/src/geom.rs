// SPDX-License-Identifier: GPL-3.0-or-later
//! Geometry primitives (logical pixels, f32).

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    /// Scale by a render factor (Tier 0.9).
    pub fn scaled(self, s: f32) -> Self {
        Self {
            x: self.x * s,
            y: self.y * s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };
    pub fn max_of(self, other: Size) -> Size {
        Size::new(self.w.max(other.w), self.h.max(other.h))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    /// Scale by a render factor (Tier 0.9).
    pub fn scaled(self, s: f32) -> Self {
        Self {
            x: self.x * s,
            y: self.y * s,
            w: self.w * s,
            h: self.h * s,
        }
    }
    pub fn from_size(size: Size) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: size.w,
            h: size.h,
        }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    /// Shrink by uniform insets.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }
    /// Intersection (used for clipping).
    pub fn intersect(&self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    pub const fn all(v: f32) -> Self {
        Self {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }
    pub const fn hv(h: f32, v: f32) -> Self {
        Self {
            left: h,
            top: v,
            right: h,
            bottom: v,
        }
    }
}

/// Rounded-rect path builder for tiny-skia (tiny-skia has no built-in rrect).
pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    // Clamp radius to half of the smaller side.
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let k = 0.5523 * r;
    let (x0, y0) = (x, y);
    let (x1, y1) = (x + w, y + h);
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    pb.finish().expect("valid path")
}

/// Circle path (for round thumbs / dots).
pub fn circle_path(cx: f32, cy: f32, r: f32) -> tiny_skia::Path {
    let k = 0.5523 * r;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(cx - r, cy);
    pb.cubic_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
    pb.cubic_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
    pb.cubic_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
    pb.cubic_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
    pb.close();
    pb.finish().expect("valid path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rect_stays_in_bounds() {
        let path = rounded_rect_path(16.0, 16.0, 388.0, 528.0, 24.0);
        let b = path.bounds();
        assert!(b.left() >= 15.0 && b.right() <= 405.0);
        assert!(b.top() >= 15.0 && b.bottom() <= 545.0);
    }

    #[test]
    fn rect_intersect_and_contains() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(50.0, 50.0, 100.0, 100.0);
        let i = a.intersect(b).expect("overlap");
        assert_eq!((i.x, i.y, i.w, i.h), (50.0, 50.0, 50.0, 50.0));
        assert!(a.contains(Point::new(99.0, 99.0)));
        assert!(!a.contains(Point::new(100.0, 50.0)));
    }
}
