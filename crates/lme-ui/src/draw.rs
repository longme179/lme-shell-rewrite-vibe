// SPDX-License-Identifier: GPL-3.0-or-later
//! Display-list commands. Widgets emit `DrawCmd`s; a `RenderBackend` executes
//! them. This is the seam between the UI toolkit and the rasterizer: a GPU
//! backend would implement the same list (ADR 0001 / mission decision 2).

use crate::color::Color;
use crate::geom::Rect;
use crate::shadow::ShadowSlice;
use crate::text::ShapedText;
use std::sync::Arc;

#[derive(Clone)]
pub enum DrawCmd {
    /// Rounded rectangle with optional fill and 1px-ish border, optional clip.
    RoundedRect {
        rect: Rect,
        radius: f32,
        fill: Option<Color>,
        border: Option<(Color, f32)>,
        clip: Option<Rect>,
    },
    /// Solid circle (switch thumbs, workspace dots).
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Color,
        clip: Option<Rect>,
    },
    /// Pre-shaped text run blitted at `pos` (pos = top-left of the line box).
    Text {
        pos: crate::geom::Point,
        line: Arc<ShapedText>,
        color: Color,
        clip: Option<Rect>,
    },
    /// Pre-rendered 9-slice shadow stretched over `rect`.
    Shadow {
        rect: Rect,
        slice: Arc<ShadowSlice>,
        clip: Option<Rect>,
    },
    /// Decoded RGBA image drawn into `rect` (rounded corners if radius > 0).
    Image {
        rect: Rect,
        radius: f32,
        image: Arc<tiny_skia::Pixmap>,
        clip: Option<Rect>,
    },
}

impl DrawCmd {
    /// Bounding rect of the command (for damage bookkeeping if ever needed).
    pub fn bounds(&self) -> Rect {
        match self {
            DrawCmd::RoundedRect { rect, .. } => *rect,
            DrawCmd::Circle { cx, cy, r, .. } => Rect::new(cx - r, cy - r, r * 2.0, r * 2.0),
            DrawCmd::Text { pos, line, .. } => Rect::new(pos.x, pos.y, line.width, line.height()),
            DrawCmd::Shadow { rect, .. } => *rect,
            DrawCmd::Image { rect, .. } => *rect,
        }
    }
}
