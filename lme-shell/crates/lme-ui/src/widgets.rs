// SPDX-License-Identifier: GPL-3.0-or-later
//! Small retained widget toolkit: layout boxes, controls, virtual list,
//! single-line text input. Setters mutate and mark dirty; the surface then
//! re-layouts and repaints on the next frame (see wayland.rs).

use crate::anim::Animator;
use crate::color::Color;
use crate::draw::DrawCmd;
use crate::geom::{Insets, Point, Rect, Size};
use crate::shadow::ShadowCache;
use crate::text::FontBook;
use crate::theme::Theme;
use std::sync::Arc;
use std::time::Instant;

// ---- XKB keysym constants used by widgets (raw u32 values) ----
pub const XKB_ESCAPE: u32 = 0xff1b;
pub const XKB_RETURN: u32 = 0xff0d;
pub const XKB_KP_ENTER: u32 = 0xff8d;
pub const XKB_TAB: u32 = 0xff09;
pub const XKB_BACKSPACE: u32 = 0xff08;
pub const XKB_DELETE: u32 = 0xffff;
pub const XKB_LEFT: u32 = 0xff51;
pub const XKB_UP: u32 = 0xff52;
pub const XKB_RIGHT: u32 = 0xff53;
pub const XKB_DOWN: u32 = 0xff54;
pub const XKB_HOME: u32 = 0xff50;
pub const XKB_END: u32 = 0xff57;

#[derive(Debug, Clone)]
pub struct KeyInput {
    pub sym: u32,
    pub utf8: String,
    pub shift: bool,
    pub ctrl: bool,
}

impl KeyInput {
    pub fn is_enter(&self) -> bool {
        self.sym == XKB_RETURN || self.sym == XKB_KP_ENTER
    }
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Move(Point),
    Down(Point),
    Up(Point),
    Leave,
    Scroll(Point, f32),
    Key(KeyInput),
}

/// Events produced by interactive widgets; drained by the shell after input
/// dispatch (command-queue pattern keeps widget code free of shell refs).
#[derive(Debug, Clone)]
pub enum UiEvent {
    Click(u64),
    Toggle(u64, bool),
    Slide(u64, f32),
    Submit(u64, String),
    /// A widget requested keyboard focus (text inputs on click).
    Focus(u64),
}

/// Context passed through measure/layout/paint/input.
pub struct Ctx<'a> {
    pub theme: &'a Theme,
    pub fonts: &'a mut FontBook,
    pub anims: &'a mut Animator,
    pub shadows: &'a mut ShadowCache,
    pub queue: &'a mut Vec<UiEvent>,
    pub now: Instant,
    /// Widget id holding keyboard focus in this surface.
    pub focused: Option<u64>,
    /// Set by any widget that changed visuals during input handling.
    pub repaint: bool,
}

impl<'a> Ctx<'a> {
    pub fn request_repaint(&mut self) {
        self.repaint = true;
    }
}

pub trait Widget {
    /// Intrinsic size (main-axis drives box packing; cross-axis is a maximum).
    fn measure(&mut self, ctx: &mut Ctx) -> Size;
    /// Assign the final bounds. Store for paint/hit-test.
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect);
    /// Emit draw commands (never mutates the tree).
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>);
    /// Handle input at widget-level; return true if consumed. `target`
    /// routes pointer-grab deliveries to a specific widget id.
    fn input(&mut self, ctx: &mut Ctx, ev: &InputEvent) -> bool {
        self.deliver(ctx, ev, None)
    }
    /// Route with explicit target (grab delivery / focus routing).
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool;
    /// Last assigned bounds.
    fn rect(&self) -> Rect;
    /// This widget participates in keyboard navigation.
    fn focusable(&self) -> bool {
        false
    }
    /// Collect focusable descendant ids in paint order.
    fn focusables(&self, out: &mut Vec<u64>) {
        if self.focusable() {
            out.push(self.id());
        }
    }
    /// Stable widget id (0 = none).
    fn id(&self) -> u64 {
        0
    }
    /// How much extra main-axis space this widget wants (0 = intrinsic).
    fn grow(&self) -> f32 {
        0.0
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any;
}

/// Forwarding impl so `push(Box<dyn Widget>)` and `push(concrete)` both work.
impl Widget for Box<dyn Widget> {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        (**self).measure(ctx)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        (**self).layout(ctx, bounds)
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        (**self).paint(ctx, out)
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        (**self).deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        (**self).rect()
    }
    fn focusable(&self) -> bool {
        (**self).focusable()
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        (**self).focusables(out)
    }
    fn id(&self) -> u64 {
        (**self).id()
    }
    fn grow(&self) -> f32 {
        (**self).grow()
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        (**self).as_any()
    }
}

pub mod font_weight {
    pub const NORMAL: u16 = 400;
    pub const MEDIUM: u16 = 500;
    pub const SEMIBOLD: u16 = 600;
    pub const BOLD: u16 = 700;
}

// ---------------------------------------------------------------------------
// Layout boxes
// ---------------------------------------------------------------------------

pub struct HBox {
    pub id: u64,
    pub children: Vec<Box<dyn Widget>>,
    pub spacing: f32,
    pub padding: Insets,
    pub align: Align,
    rect: Rect,
    measured: Size,
}

pub enum Align {
    Start,
    Center,
    /// Children stretch to the box's cross axis (default for bars/rows).
    Stretch,
}

impl HBox {
    pub fn new(spacing: f32) -> Self {
        Self {
            id: 0,
            children: Vec::new(),
            spacing,
            padding: Insets::all(0.0),
            align: Align::Stretch,
            rect: Rect::default(),
            measured: Size::ZERO,
        }
    }
    pub fn padded(mut self, pad: f32) -> Self {
        self.padding = Insets::all(pad);
        self
    }
    pub fn push(mut self, child: impl Widget + 'static) -> Self {
        self.children.push(Box::new(child));
        self
    }
}

impl Widget for HBox {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        let mut w = self.padding.left + self.padding.right;
        let mut h = 0.0f32;
        let mut first = true;
        for c in &mut self.children {
            let s = c.measure(ctx);
            if !first {
                w += self.spacing;
            }
            first = false;
            w += s.w;
            h = h.max(s.h);
        }
        h += self.padding.top + self.padding.bottom;
        self.measured = Size::new(w, h);
        self.measured
    }

    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        let inner = Rect::new(
            bounds.x + self.padding.left,
            bounds.y + self.padding.top,
            bounds.w - self.padding.left - self.padding.right,
            bounds.h - self.padding.top - self.padding.bottom,
        );
        // (Re)measure every child: sizes come from measurement, not stale rects.
        let sizes: Vec<Size> = self.children.iter_mut().map(|c| c.measure(ctx)).collect();
        let total_grow: f32 = self.children.iter().map(|c| c.grow()).sum();
        let intrinsic: f32 = sizes.iter().map(|s| s.w).sum::<f32>()
            + self.spacing * self.children.len().saturating_sub(1) as f32;
        let extra = (inner.w - intrinsic).max(0.0);
        let mut x = inner.x;
        for (i, c) in self.children.iter_mut().enumerate() {
            let mut cw = sizes[i].w;
            if c.grow() > 0.0 && total_grow > 0.0 {
                cw += extra * (c.grow() / total_grow);
            }
            let ch = match self.align {
                Align::Stretch => inner.h,
                Align::Center => sizes[i].h.min(inner.h),
                Align::Start => sizes[i].h,
            };
            let cy = match self.align {
                Align::Center => inner.y + (inner.h - ch) / 2.0,
                _ => inner.y,
            };
            c.layout(ctx, Rect::new(x, cy, cw.max(0.0), ch.max(0.0)));
            x += cw + self.spacing;
        }
    }

    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        for c in &mut self.children {
            c.paint(ctx, out);
        }
    }

    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if let Some(tid) = target {
            for c in &mut self.children {
                if c.deliver(ctx, ev, Some(tid)) {
                    return true;
                }
            }
            return false;
        }
        // hit-test in reverse paint order
        for c in self.children.iter_mut().rev() {
            let hit = match ev {
                InputEvent::Move(p)
                | InputEvent::Down(p)
                | InputEvent::Up(p)
                | InputEvent::Scroll(p, _) => c.rect().contains(*p),
                InputEvent::Leave => true,
                InputEvent::Key(_) => false,
            };
            if hit && c.deliver(ctx, ev, None) {
                return true;
            }
        }
        false
    }

    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        for c in &self.children {
            c.focusables(out);
        }
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub struct VBox {
    pub id: u64,
    pub children: Vec<Box<dyn Widget>>,
    pub spacing: f32,
    pub padding: Insets,
    pub align: Align,
    rect: Rect,
    measured: Size,
}

impl VBox {
    pub fn new(spacing: f32) -> Self {
        Self {
            id: 0,
            children: Vec::new(),
            spacing,
            padding: Insets::all(0.0),
            align: Align::Stretch,
            rect: Rect::default(),
            measured: Size::ZERO,
        }
    }
    pub fn padded(mut self, pad: f32) -> Self {
        self.padding = Insets::all(pad);
        self
    }
    pub fn push(mut self, child: impl Widget + 'static) -> Self {
        self.children.push(Box::new(child));
        self
    }
}

impl Widget for VBox {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        let mut h = self.padding.top + self.padding.bottom;
        let mut w = 0.0f32;
        let mut first = true;
        for c in &mut self.children {
            let s = c.measure(ctx);
            if !first {
                h += self.spacing;
            }
            first = false;
            h += s.h;
            w = w.max(s.w);
        }
        w += self.padding.left + self.padding.right;
        self.measured = Size::new(w, h);
        self.measured
    }

    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        let inner = Rect::new(
            bounds.x + self.padding.left,
            bounds.y + self.padding.top,
            bounds.w - self.padding.left - self.padding.right,
            bounds.h - self.padding.top - self.padding.bottom,
        );
        let sizes: Vec<Size> = self.children.iter_mut().map(|c| c.measure(ctx)).collect();
        let total_grow: f32 = self.children.iter().map(|c| c.grow()).sum();
        let intrinsic_h: f32 = sizes.iter().map(|s| s.h).sum::<f32>()
            + self.spacing * self.children.len().saturating_sub(1) as f32;
        let extra = (inner.h - intrinsic_h).max(0.0);
        let mut y = inner.y;
        for (i, c) in self.children.iter_mut().enumerate() {
            let mut ch = sizes[i].h;
            if c.grow() > 0.0 && total_grow > 0.0 {
                ch += extra * (c.grow() / total_grow);
            }
            let cw = match self.align {
                Align::Stretch => inner.w,
                Align::Center => sizes[i].w.min(inner.w),
                Align::Start => sizes[i].w,
            };
            let cx = match self.align {
                Align::Center => inner.x + (inner.w - cw) / 2.0,
                _ => inner.x,
            };
            c.layout(ctx, Rect::new(cx, y, cw.max(0.0), ch.max(0.0)));
            y += ch + self.spacing;
        }
    }

    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        for c in &mut self.children {
            c.paint(ctx, out);
        }
    }

    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if let Some(tid) = target {
            for c in &mut self.children {
                if c.deliver(ctx, ev, Some(tid)) {
                    return true;
                }
            }
            return false;
        }
        for c in self.children.iter_mut().rev() {
            let hit = match ev {
                InputEvent::Move(p)
                | InputEvent::Down(p)
                | InputEvent::Up(p)
                | InputEvent::Scroll(p, _) => c.rect().contains(*p),
                InputEvent::Leave => true,
                InputEvent::Key(_) => false,
            };
            if hit && c.deliver(ctx, ev, None) {
                return true;
            }
        }
        false
    }

    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        for c in &self.children {
            c.focusables(out);
        }
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Fixed-size spacer / flexible filler.
pub struct Spacer {
    pub w: f32,
    pub h: f32,
    pub grow_factor: f32,
    rect: Rect,
}

impl Spacer {
    pub fn fixed(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            grow_factor: 0.0,
            rect: Rect::default(),
        }
    }
    pub fn fill() -> Self {
        Self {
            w: 0.0,
            h: 0.0,
            grow_factor: 1.0,
            rect: Rect::default(),
        }
    }
}

impl Widget for Spacer {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(self.w, self.h)
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, _ctx: &mut Ctx, _out: &mut Vec<DrawCmd>) {}
    fn deliver(&mut self, _ctx: &mut Ctx, _ev: &InputEvent, _t: Option<u64>) -> bool {
        false
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn grow(&self) -> f32 {
        self.grow_factor
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

pub struct Label {
    pub text: String,
    pub size: f32,
    pub weight: u16,
    pub color: Color,
    pub align: TextAlign,
    pub max_width: Option<f32>,
    rect: Rect,
    shaped: Option<Arc<crate::text::ShapedText>>,
    id: u64,
}

impl Label {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            size: 14.0,
            weight: font_weight::NORMAL,
            color: Color::WHITE,
            align: TextAlign::Left,
            max_width: None,
            rect: Rect::default(),
            shaped: None,
            id: 0,
        }
    }
    pub fn styled(text: impl Into<String>, size: f32, weight: u16, color: Color) -> Self {
        let mut l = Self::new(text);
        l.size = size;
        l.weight = weight;
        l.color = color;
        l
    }
    pub fn set_text(&mut self, text: impl Into<String>) {
        let t: String = text.into();
        if t != self.text {
            self.text = t;
            self.shaped = None;
        }
    }
    fn ensure_shaped(&mut self, ctx: &mut Ctx) {
        if self.shaped.is_none() {
            self.shaped = Some(match self.max_width {
                Some(w) if w.is_finite() && w > 0.0 => {
                    ctx.fonts
                        .line_truncated(&self.text, self.size, self.weight, w)
                }
                _ => ctx.fonts.line(&self.text, self.size, self.weight),
            });
        }
    }
}

impl Widget for Label {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.ensure_shaped(ctx);
        let s = self.shaped.as_ref().expect("shaped");
        Size::new(s.width.ceil(), s.height().ceil())
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        self.ensure_shaped(ctx);
        let s = self.shaped.as_ref().expect("shaped");
        let x = match self.align {
            TextAlign::Left => self.rect.x,
            TextAlign::Center => self.rect.x + (self.rect.w - s.width) / 2.0,
            TextAlign::Right => self.rect.x + self.rect.w - s.width,
        };
        let y = self.rect.y + (self.rect.h - s.height()) / 2.0;
        out.push(DrawCmd::Text {
            pos: Point::new(x.max(self.rect.x), y.max(self.rect.y)),
            line: Arc::clone(s),
            color: self.color,
            clip: None,
        });
    }
    fn deliver(&mut self, _ctx: &mut Ctx, _ev: &InputEvent, _t: Option<u64>) -> bool {
        false
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Symbol/emoji glyph or a theme icon name rendered as a colored glyph.
pub struct Icon {
    pub glyph: String,
    pub size: f32,
    pub color: Color,
    rect: Rect,
}

impl Icon {
    pub fn new(glyph: impl Into<String>, size: f32, color: Color) -> Self {
        Self {
            glyph: glyph.into(),
            size,
            color,
            rect: Rect::default(),
        }
    }
}

impl Widget for Icon {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        let s = ctx.fonts.line(&self.glyph, self.size, font_weight::NORMAL);
        Size::new(s.width.ceil().max(self.size * 0.6), s.height().ceil())
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        let s = ctx.fonts.line(&self.glyph, self.size, font_weight::NORMAL);
        let x = self.rect.x + (self.rect.w - s.width) / 2.0;
        let y = self.rect.y + (self.rect.h - s.height()) / 2.0;
        out.push(DrawCmd::Text {
            pos: Point::new(x, y),
            line: s,
            color: self.color,
            clip: None,
        });
    }
    fn deliver(&mut self, _ctx: &mut Ctx, _ev: &InputEvent, _t: Option<u64>) -> bool {
        false
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

pub struct Button {
    pub id: u64,
    pub label: String,
    pub label_size: f32,
    pub label_weight: u16,
    pub fill: Color,
    pub hover_fill: Option<Color>,
    pub text_color: Color,
    pub radius: f32,
    pub min_w: f32,
    pub min_h: f32,
    pub border: Option<(Color, f32)>,
    hover: bool,
    pressed: bool,
    rect: Rect,
    shaped: Option<Arc<crate::text::ShapedText>>,
}

impl Button {
    pub fn new(id: u64, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            label_size: 14.0,
            label_weight: font_weight::MEDIUM,
            fill: Color::rgb(0.2, 0.2, 0.25),
            hover_fill: None,
            text_color: Color::WHITE,
            radius: 12.0,
            min_w: 0.0,
            min_h: 36.0,
            border: None,
            hover: false,
            pressed: false,
            rect: Rect::default(),
            shaped: None,
        }
    }
    pub fn themed(id: u64, label: impl Into<String>, theme: &Theme) -> Self {
        Button::new(id, label)
            .with_fill(theme.button_fill)
            .with_hover(theme.button_hover)
            .with_text(theme.button_text)
            .with_radius(theme.small_radius)
    }
    pub fn with_fill(mut self, c: Color) -> Self {
        self.fill = c;
        self
    }
    pub fn with_hover(mut self, c: Color) -> Self {
        self.hover_fill = Some(c);
        self
    }
    pub fn with_text(mut self, c: Color) -> Self {
        self.text_color = c;
        self
    }
    pub fn with_radius(mut self, r: f32) -> Self {
        self.radius = r;
        self
    }
    pub fn set_label(&mut self, label: impl Into<String>) {
        let l: String = label.into();
        if l != self.label {
            self.label = l;
            self.shaped = None;
        }
    }
    fn ensure_shaped(&mut self, ctx: &mut Ctx) {
        if self.shaped.is_none() {
            self.shaped = Some(
                ctx.fonts
                    .line(&self.label, self.label_size, self.label_weight),
            );
        }
    }
}

impl Widget for Button {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.ensure_shaped(ctx);
        let s = self.shaped.as_ref().expect("shaped");
        Size::new(
            s.width + (28.0f32).max(self.min_w * 0.2),
            (s.height() + 16.0).max(self.min_h),
        )
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        let fill = if self.pressed {
            self.hover_fill
                .unwrap_or(self.fill)
                .mix(self.text_color, 0.10)
        } else if self.hover {
            self.hover_fill
                .unwrap_or_else(|| self.fill.mix(self.text_color, 0.08))
        } else {
            self.fill
        };
        let border = if ctx.focused == Some(self.id) {
            Some((ctx.theme.accent, 2.0))
        } else {
            self.border
        };
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: self.radius,
            fill: Some(fill),
            border,
            clip: None,
        });
        self.ensure_shaped(ctx);
        let s = self.shaped.as_ref().expect("shaped");
        out.push(DrawCmd::Text {
            pos: Point::new(
                self.rect.x + (self.rect.w - s.width) / 2.0,
                self.rect.y + (self.rect.h - s.height()) / 2.0,
            ),
            line: Arc::clone(s),
            color: self.text_color,
            clip: None,
        });
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if target.is_some() && target != Some(self.id) {
            return false;
        }
        match ev {
            InputEvent::Move(p) => {
                let over = self.rect.contains(*p);
                if over != self.hover {
                    self.hover = over;
                    ctx.request_repaint();
                }
                over
            }
            InputEvent::Down(p) => {
                if self.rect.contains(*p) {
                    self.pressed = true;
                    ctx.request_repaint();
                    true
                } else {
                    false
                }
            }
            InputEvent::Up(p) => {
                let was = self.pressed;
                self.pressed = false;
                if was && self.rect.contains(*p) {
                    ctx.queue.push(UiEvent::Click(self.id));
                    ctx.request_repaint();
                }
                was
            }
            InputEvent::Leave => {
                if self.hover || self.pressed {
                    self.hover = false;
                    self.pressed = false;
                    ctx.request_repaint();
                }
                false
            }
            InputEvent::Key(k) => {
                if k.is_enter() && ctx.focused == Some(self.id) {
                    ctx.queue.push(UiEvent::Click(self.id));
                    return true;
                }
                false
            }
            InputEvent::Scroll(_, _) => false,
        }
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusable(&self) -> bool {
        true
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub struct Switch {
    pub id: u64,
    pub on: bool,
    pub track: Color,
    pub thumb: Color,
    pub accent: Color,
    hover: bool,
    rect: Rect,
}

impl Switch {
    pub fn new(id: u64, on: bool, theme: &Theme) -> Self {
        Self {
            id,
            on,
            track: theme.switch_track,
            thumb: theme.switch_thumb,
            accent: theme.accent,
            hover: false,
            rect: Rect::default(),
        }
    }
}

impl Widget for Switch {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(44.0, 24.0)
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        let r = self.rect;
        let track = if self.on {
            self.accent
        } else {
            self.track.alpha(0.35)
        };
        out.push(DrawCmd::RoundedRect {
            rect: Rect::new(r.x, r.y + (r.h - 24.0) / 2.0, 44.0, 24.0),
            radius: 12.0,
            fill: Some(track),
            border: Some((ctx.theme.outline_variant, 1.0)),
            clip: None,
        });
        let thumb_x = if self.on {
            r.x + 44.0 - 20.0
        } else {
            r.x + 4.0
        };
        out.push(DrawCmd::Circle {
            cx: thumb_x + 8.0,
            cy: r.y + r.h / 2.0,
            r: 8.0,
            fill: self.thumb,
            clip: None,
        });
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if target.is_some() && target != Some(self.id) {
            return false;
        }
        match ev {
            InputEvent::Move(p) => {
                self.hover = self.rect.contains(*p);
                self.hover
            }
            InputEvent::Up(p) | InputEvent::Down(p) => {
                if self.rect.contains(*p) {
                    if matches!(ev, InputEvent::Up(_)) {
                        self.on = !self.on;
                        ctx.queue.push(UiEvent::Toggle(self.id, self.on));
                        ctx.request_repaint();
                    }
                    true
                } else {
                    false
                }
            }
            InputEvent::Key(k) => {
                if (k.is_enter() || k.sym == XKB_SPACE) && ctx.focused == Some(self.id) {
                    self.on = !self.on;
                    ctx.queue.push(UiEvent::Toggle(self.id, self.on));
                    ctx.request_repaint();
                    return true;
                }
                false
            }
            InputEvent::Leave => false,
            InputEvent::Scroll(_, _) => false,
        }
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusable(&self) -> bool {
        true
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub const XKB_SPACE: u32 = 0x0020;

pub struct Slider {
    pub id: u64,
    pub value: f32, // 0..=100
    pub track: Color,
    pub fill: Color,
    pub thumb: Color,
    rect: Rect,
    drag: bool,
}

impl Slider {
    pub fn new(id: u64, value: f32, theme: &Theme) -> Self {
        Self {
            id,
            value: value.clamp(0.0, 100.0),
            track: theme.slider_track,
            fill: theme.slider_fill,
            thumb: theme.slider_fill,
            rect: Rect::default(),
            drag: false,
        }
    }
    fn value_at(&self, x: f32) -> f32 {
        let t = ((x - self.rect.x) / self.rect.w).clamp(0.0, 1.0);
        t * 100.0
    }
}

impl Widget for Slider {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(120.0, 24.0)
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, _ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        let r = self.rect;
        let cy = r.y + r.h / 2.0;
        out.push(DrawCmd::RoundedRect {
            rect: Rect::new(r.x, cy - 2.0, r.w, 4.0),
            radius: 2.0,
            fill: Some(self.track),
            border: None,
            clip: None,
        });
        let fill_w = r.w * (self.value / 100.0);
        out.push(DrawCmd::RoundedRect {
            rect: Rect::new(r.x, cy - 2.0, fill_w, 4.0),
            radius: 2.0,
            fill: Some(self.fill),
            border: None,
            clip: None,
        });
        let tx = r.x + fill_w;
        out.push(DrawCmd::Circle {
            cx: tx,
            cy,
            r: 9.0,
            fill: self.thumb,
            clip: None,
        });
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if target.is_some() && target != Some(self.id) {
            return false;
        }
        match ev {
            InputEvent::Down(p) => {
                if self.rect.contains(*p) {
                    self.drag = true;
                    self.value = self.value_at(p.x);
                    ctx.queue.push(UiEvent::Slide(self.id, self.value));
                    ctx.request_repaint();
                    true
                } else {
                    false
                }
            }
            InputEvent::Move(p) => {
                if self.drag {
                    self.value = self.value_at(p.x);
                    ctx.queue.push(UiEvent::Slide(self.id, self.value));
                    ctx.request_repaint();
                    true
                } else {
                    self.rect.contains(*p)
                }
            }
            InputEvent::Up(_) => {
                let was = self.drag;
                self.drag = false;
                was
            }
            InputEvent::Key(k) => {
                if ctx.focused == Some(self.id) {
                    let delta = if k.sym == XKB_LEFT {
                        -5.0
                    } else if k.sym == XKB_RIGHT {
                        5.0
                    } else {
                        return false;
                    };
                    self.value = (self.value + delta).clamp(0.0, 100.0);
                    ctx.queue.push(UiEvent::Slide(self.id, self.value));
                    ctx.request_repaint();
                    return true;
                }
                false
            }
            InputEvent::Leave => false,
            InputEvent::Scroll(_, _) => false,
        }
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusable(&self) -> bool {
        true
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Single-line text input with cursor, placeholder and Enter submission.
pub struct TextInput {
    pub id: u64,
    pub text: String,
    pub placeholder: String,
    pub cursor: usize, // char index
    pub size: f32,
    pub fill: Color,
    pub text_color: Color,
    pub radius: f32,
    focused_visual: bool,
    rect: Rect,
}

impl TextInput {
    pub fn new(id: u64, placeholder: impl Into<String>) -> Self {
        Self {
            id,
            text: String::new(),
            placeholder: placeholder.into(),
            cursor: 0,
            size: 15.0,
            fill: Color::rgb(0.15, 0.15, 0.2),
            text_color: Color::WHITE,
            radius: 14.0,
            focused_visual: false,
            rect: Rect::default(),
        }
    }
    pub fn themed(id: u64, placeholder: impl Into<String>, theme: &Theme) -> Self {
        let mut t = TextInput::new(id, placeholder);
        t.fill = theme.field_fill;
        t.text_color = theme.field_text;
        t.radius = theme.small_radius;
        t
    }
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.chars().count();
    }
}

impl Widget for TextInput {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        let h = ctx
            .fonts
            .line("Ag", self.size, font_weight::NORMAL)
            .height()
            + 18.0;
        Size::new(240.0, h)
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        self.focused_visual = ctx.focused == Some(self.id);
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: self.radius,
            fill: Some(self.fill),
            border: Some((
                if self.focused_visual {
                    ctx.theme.accent
                } else {
                    ctx.theme.outline_variant
                },
                if self.focused_visual { 2.0 } else { 1.0 },
            )),
            clip: None,
        });
        let shown: String = if self.text.is_empty() {
            self.placeholder.clone()
        } else {
            self.text.clone()
        };
        let color = if self.text.is_empty() {
            self.text_color.alpha(0.5)
        } else {
            self.text_color
        };
        let line = ctx.fonts.line(&shown, self.size, font_weight::NORMAL);
        let line_h = line.height();
        let text_y = self.rect.y + (self.rect.h - line_h) / 2.0;
        out.push(DrawCmd::Text {
            pos: Point::new(self.rect.x + 14.0, text_y),
            line,
            color,
            clip: Some(self.rect),
        });
        // cursor
        if self.focused_visual && !self.text.is_empty() {
            let prefix: String = self.text.chars().take(self.cursor).collect();
            let pre = ctx.fonts.line(&prefix, self.size, font_weight::NORMAL);
            let cx = self.rect.x + 14.0 + pre.width + 1.0;
            out.push(DrawCmd::RoundedRect {
                rect: Rect::new(cx, text_y + 2.0, 2.0, line_h - 4.0),
                radius: 1.0,
                fill: Some(ctx.theme.accent),
                border: None,
                clip: Some(self.rect),
            });
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if target.is_some() && target != Some(self.id) {
            return false;
        }
        match ev {
            InputEvent::Down(p) | InputEvent::Up(p) => {
                if self.rect.contains(*p) {
                    if matches!(ev, InputEvent::Down(_)) && ctx.focused != Some(self.id) {
                        ctx.queue.push(UiEvent::Focus(self.id));
                    }
                    true
                } else {
                    false
                }
            }
            InputEvent::Key(k) => {
                if ctx.focused != Some(self.id) {
                    return false;
                }
                self.handle_key(ctx, k)
            }
            InputEvent::Move(_) => self.rect.contains(match ev {
                InputEvent::Move(p) => *p,
                _ => Point::default(),
            }),
            _ => false,
        }
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusable(&self) -> bool {
        true
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl TextInput {
    fn handle_key(&mut self, ctx: &mut Ctx, k: &KeyInput) -> bool {
        let char_count = self.text.chars().count();
        match k.sym {
            XKB_BACKSPACE => {
                if self.cursor > 0 {
                    let idx = char_to_byte(&self.text, self.cursor - 1);
                    self.text.replace_range(idx..next_byte(&self.text, idx), "");
                    self.cursor -= 1;
                    ctx.request_repaint();
                }
                true
            }
            XKB_DELETE => {
                if self.cursor < char_count {
                    let idx = char_to_byte(&self.text, self.cursor);
                    self.text.replace_range(idx..next_byte(&self.text, idx), "");
                    ctx.request_repaint();
                }
                true
            }
            XKB_LEFT => {
                self.cursor = self.cursor.saturating_sub(1);
                ctx.request_repaint();
                true
            }
            XKB_RIGHT => {
                self.cursor = (self.cursor + 1).min(char_count);
                ctx.request_repaint();
                true
            }
            XKB_HOME => {
                self.cursor = 0;
                ctx.request_repaint();
                true
            }
            XKB_END => {
                self.cursor = char_count;
                ctx.request_repaint();
                true
            }
            _ if k.is_enter() => {
                ctx.queue.push(UiEvent::Submit(self.id, self.text.clone()));
                true
            }
            _ => {
                if !k.utf8.is_empty() && !k.ctrl && k.utf8.chars().all(|c| !c.is_control()) {
                    let idx = char_to_byte(&self.text, self.cursor);
                    self.text.insert_str(idx, &k.utf8);
                    self.cursor += k.utf8.chars().count();
                    ctx.request_repaint();
                    true
                } else {
                    false
                }
            }
        }
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn next_byte(s: &str, byte_idx: usize) -> usize {
    s[byte_idx..]
        .char_indices()
        .nth(1)
        .map(|(b, _)| byte_idx + b)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Virtual list
// ---------------------------------------------------------------------------

/// Data source for a virtualized list: rows are recycled widgets rebound to
/// indexes; no allocation in the steady state.
pub trait ListSource {
    fn count(&self) -> usize;
    fn row_height(&self) -> f32;
    /// (Re)bind a recycled row widget to `index`.
    fn bind(&mut self, row: &mut Box<dyn Widget>, index: usize);
    /// Build a fresh row widget (only called when the visible window grows).
    fn build_row(&mut self) -> Box<dyn Widget>;
    /// Optional: which index should be highlighted as selected.
    fn selected(&self) -> Option<usize> {
        None
    }
}

pub struct VirtualList {
    pub id: u64,
    pub src: Box<dyn ListSource>,
    pub scroll: f32,
    rows: Vec<(usize, Box<dyn Widget>)>,
    rect: Rect,
    viewport_h: f32,
}

impl VirtualList {
    pub fn new(src: Box<dyn ListSource>) -> Self {
        Self {
            id: 0,
            src,
            scroll: 0.0,
            rows: Vec::new(),
            rect: Rect::default(),
            viewport_h: 0.0,
        }
    }
    fn visible_range(&self) -> (usize, usize) {
        let rh = self.src.row_height().max(1.0);
        let first = (self.scroll / rh).floor() as usize;
        let count = ((self.viewport_h / rh).ceil() as usize) + 1;
        (
            first.min(self.src.count()),
            (first + count).min(self.src.count()),
        )
    }
    pub fn scroll_to(&mut self, index: usize) {
        let rh = self.src.row_height().max(1.0);
        let y = index as f32 * rh;
        if y < self.scroll {
            self.scroll = y;
        } else if y + rh > self.scroll + self.viewport_h {
            self.scroll = (y + rh - self.viewport_h).max(0.0);
        }
    }
}

impl Widget for VirtualList {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(0.0, self.src.count() as f32 * self.src.row_height())
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        self.viewport_h = bounds.h;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        let (first, last) = self.visible_range();
        // recycle rows outside the window
        self.rows.retain(|(idx, _)| *idx >= first && *idx < last);
        let mut wanted: Vec<usize> = (first..last).collect();
        for (idx, row) in self.rows.iter_mut() {
            wanted.retain(|w| *w != *idx);
            let _ = idx;
            let _ = row;
        }
        for idx in wanted {
            // reuse a detached row if any, else build
            let row = self.src.build_row();
            self.rows.push((idx, row));
        }
        self.rows.sort_by_key(|(idx, _)| *idx);
        let rh = self.src.row_height();
        let selected = self.src.selected();
        let mut i = 0;
        let rows = std::mem::take(&mut self.rows);
        for (idx, mut row) in rows {
            let y = self.rect.y + idx as f32 * rh - self.scroll;
            row.layout(ctx, Rect::new(self.rect.x, y, self.rect.w, rh));
            self.src.bind(&mut row, idx);
            let _ = selected;
            row.layout(ctx, Rect::new(self.rect.x, y, self.rect.w, rh));
            row.paint(ctx, out);
            let _ = &mut i;
            i += 1;
            self.rows.push((idx, row));
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        let (first, last) = self.visible_range();
        let mut consumed = false;
        let rows = std::mem::take(&mut self.rows);
        for (idx, mut row) in rows {
            let y = self.rect.y + idx as f32 * self.src.row_height() - self.scroll;
            row.layout(
                ctx,
                Rect::new(self.rect.x, y, self.rect.w, self.src.row_height()),
            );
            self.src.bind(&mut row, idx);
            if !consumed {
                consumed = row.deliver(ctx, ev, target);
            }
            self.rows.push((idx, row));
        }
        let _ = (first, last);
        if !consumed {
            if let InputEvent::Scroll(_, dy) = ev {
                self.scroll = (self.scroll + dy * 40.0).clamp(
                    0.0,
                    (self.src.count() as f32 * self.src.row_height() - self.viewport_h).max(0.0),
                );
                ctx.request_repaint();
                return true;
            }
        }
        consumed
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Keyboard focus navigation over the focusable descendants of `child`.
/// Arrows/Home/End move focus; Enter is forwarded (buttons click themselves).
pub struct FocusScope {
    pub child: Box<dyn Widget>,
    horizontal: bool,
    rect: Rect,
}

impl FocusScope {
    pub fn new(child: Box<dyn Widget>, horizontal: bool) -> Self {
        Self {
            child,
            horizontal,
            rect: Rect::default(),
        }
    }
}

impl Widget for FocusScope {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.child.measure(ctx)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        self.child.layout(ctx, bounds);
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        self.child.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        if let InputEvent::Key(k) = ev {
            let mut ids = Vec::new();
            self.child.focusables(&mut ids);
            if ids.is_empty() {
                return false;
            }
            let forward = match k.sym {
                XKB_RIGHT if self.horizontal => true,
                XKB_DOWN if !self.horizontal => true,
                XKB_LEFT if self.horizontal => false,
                XKB_UP if !self.horizontal => false,
                XKB_TAB => !k.shift,
                _ => return self.child.deliver(ctx, ev, target),
            };
            let cur = ctx
                .focused
                .and_then(|f| ids.iter().position(|i| *i == f))
                .unwrap_or(usize::MAX - 1);
            let next = if forward {
                (cur.wrapping_add(1)).min(ids.len() - 1)
            } else {
                cur.saturating_sub(1)
            };
            let next_id = if cur == usize::MAX - 1 {
                ids[if forward { 0 } else { ids.len() - 1 }]
            } else {
                ids[next]
            };
            ctx.focused = Some(next_id);
            ctx.request_repaint();
            return true;
        }
        self.child.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.child.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> (Theme, FontBook, Animator, ShadowCache) {
        (
            Theme::default_with(&crate::theme::Palette::default()),
            FontBook::new(),
            Animator::new(),
            ShadowCache::new(),
        )
    }

    fn with_ctx<R>(
        parts: &mut (Theme, FontBook, Animator, ShadowCache),
        f: impl FnOnce(Ctx) -> R,
    ) -> R {
        let (theme, fonts, anims, shadows) = parts;
        let mut queue = Vec::new();
        let ctx = Ctx {
            theme,
            fonts,
            anims,
            shadows,
            queue: &mut queue,
            now: Instant::now(),
            focused: None,
            repaint: false,
        };
        f(ctx)
    }

    #[test]
    fn hbox_layout_and_hit_test() {
        let mut parts = test_ctx();
        let mut row = HBox::new(8.0);
        row = row.push(Button::new(1, "One"));
        row = row.push(Button::new(2, "Two"));
        let m = with_ctx(&mut parts, |mut c| row.measure(&mut c));
        assert!(m.w > 40.0);
        with_ctx(&mut parts, |mut c| {
            row.layout(&mut c, Rect::new(0.0, 0.0, 300.0, 40.0))
        });
        // click inside first button
        let clicked = with_ctx(&mut parts, |mut c| {
            let mut consumed = row.deliver(&mut c, &InputEvent::Down(Point::new(10.0, 20.0)), None);
            consumed |= row.deliver(&mut c, &InputEvent::Up(Point::new(10.0, 20.0)), None);
            consumed
        });
        assert!(clicked);
    }

    #[test]
    fn button_click_emits_event() {
        let mut parts = test_ctx();
        let mut b = Button::new(7, "OK");
        with_ctx(&mut parts, |mut c| {
            b.layout(&mut c, Rect::new(0.0, 0.0, 60.0, 30.0))
        });
        let events = with_ctx(&mut parts, |mut c| {
            b.deliver(&mut c, &InputEvent::Down(Point::new(30.0, 15.0)), None);
            b.deliver(&mut c, &InputEvent::Up(Point::new(30.0, 15.0)), None);
            std::mem::take(c.queue)
        });
        assert!(matches!(events.as_slice(), [UiEvent::Click(7)]));
    }

    #[test]
    fn text_input_types_and_submits() {
        let mut parts = test_ctx();
        let mut t = TextInput::new(3, "Type…");
        with_ctx(&mut parts, |mut c| {
            t.layout(&mut c, Rect::new(0.0, 0.0, 200.0, 36.0))
        });
        let events = with_ctx(&mut parts, |mut c| {
            c.focused = Some(3);
            t.deliver(
                &mut c,
                &InputEvent::Key(KeyInput {
                    sym: b'h' as u32,
                    utf8: "h".into(),
                    shift: false,
                    ctrl: false,
                }),
                None,
            );
            t.deliver(
                &mut c,
                &InputEvent::Key(KeyInput {
                    sym: b'i' as u32,
                    utf8: "i".into(),
                    shift: false,
                    ctrl: false,
                }),
                None,
            );
            t.deliver(
                &mut c,
                &InputEvent::Key(KeyInput {
                    sym: XKB_RETURN,
                    utf8: String::new(),
                    shift: false,
                    ctrl: false,
                }),
                None,
            );
            std::mem::take(c.queue)
        });
        assert_eq!(t.text, "hi");
        assert_eq!(t.cursor, 2);
        assert!(matches!(events.as_slice(), [UiEvent::Submit(3, s)] if s == "hi"));
        // backspace removes the last char
        with_ctx(&mut parts, |mut c| {
            c.focused = Some(3);
            t.deliver(
                &mut c,
                &InputEvent::Key(KeyInput {
                    sym: XKB_BACKSPACE,
                    utf8: String::new(),
                    shift: false,
                    ctrl: false,
                }),
                None,
            );
        });
        assert_eq!(t.text, "h");
    }

    #[test]
    fn focus_scope_cycles_buttons() {
        let mut parts = test_ctx();
        let mut scope = FocusScope::new(
            Box::new(
                HBox::new(6.0)
                    .push(Button::new(11, "A"))
                    .push(Button::new(22, "B")),
            ),
            true,
        );
        with_ctx(&mut parts, |mut c| {
            scope.layout(&mut c, Rect::new(0.0, 0.0, 200.0, 36.0))
        });
        let f = with_ctx(&mut parts, |mut c| {
            scope.deliver(
                &mut c,
                &InputEvent::Key(KeyInput {
                    sym: XKB_RIGHT,
                    utf8: String::new(),
                    shift: false,
                    ctrl: false,
                }),
                None,
            );
            scope.deliver(
                &mut c,
                &InputEvent::Key(KeyInput {
                    sym: XKB_RIGHT,
                    utf8: String::new(),
                    shift: false,
                    ctrl: false,
                }),
                None,
            );
            c.focused
        });
        assert_eq!(f, Some(22), "second Right stays on the last button");
    }

    struct Nums;
    impl ListSource for Nums {
        fn count(&self) -> usize {
            100
        }
        fn row_height(&self) -> f32 {
            30.0
        }
        fn bind(&mut self, row: &mut Box<dyn Widget>, index: usize) {
            if let Some(l) = row.as_any().downcast_mut::<Label>() {
                l.set_text(format!("item {index}"));
            }
        }
        fn build_row(&mut self) -> Box<dyn Widget> {
            Box::new(Label::new("…"))
        }
    }

    #[test]
    fn virtual_list_windows_and_binds() {
        let mut parts = test_ctx();
        let mut list = VirtualList::new(Box::new(Nums));
        with_ctx(&mut parts, |mut c| {
            list.layout(&mut c, Rect::new(0.0, 0.0, 200.0, 120.0))
        });
        let cmds = with_ctx(&mut parts, |mut c| {
            let mut out = Vec::new();
            list.paint(&mut c, &mut out);
            out
        });
        // 120px viewport / 30px rows + 1 => 5 rows bound
        let labels: Vec<&String> = cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text { line, .. } => Some(&line.text),
                _ => None,
            })
            .collect();
        assert_eq!(labels.len(), 5);
        assert_eq!(labels[0], "item 0");
        assert_eq!(labels[4], "item 4");
        // scroll_to(90) makes index 90 visible as the LAST row (5 visible)
        list.scroll_to(90);
        let cmds = with_ctx(&mut parts, |mut c| {
            let mut out = Vec::new();
            list.paint(&mut c, &mut out);
            out
        });
        let first = cmds
            .iter()
            .find_map(|c| match c {
                DrawCmd::Text { line, .. } => Some(line.text.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(first, "item 87");
    }
}
