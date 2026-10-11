// SPDX-License-Identifier: GPL-3.0-or-later
//! lme-ui: LME Shell's native UI toolkit.
//!
//! Layers (see docs/adr/0001 + 0002):
//! - [`geom`]/[`color`] — primitives
//! - [`draw`] — display-list commands (the render seam)
//! - [`render`] — `RenderBackend` trait + validated CPU backend (tiny-skia);
//!   a GPU backend is a documented stub, intentionally not implemented here
//! - [`text`] — cosmic-text shaping with bounded caches (vendored Roboto VF)
//! - [`shadow`] — pre-rendered 9-slice shadows (never per-frame blur)
//! - [`theme`] — Theme.qml port: palette.json + settings.json roles
//! - [`anim`] — frame-callback-driven tween tracks, cubic-bezier curves
//! - [`widgets`] — retained widget tree: boxes, Label, Button, Switch,
//!   Slider, TextInput, VirtualList, FocusScope
//! - [`wayland`] — layer-shell windows, seats, shm, the render pass and the
//!   AppHandler seam the daemon implements
//! - [`icons`] — dependency-free hicolor PNG lookup

pub mod anim;
pub mod color;
pub mod draw;
pub mod geom;
pub mod icons;
pub mod render;
pub mod shadow;
pub mod text;
pub mod theme;
pub mod widgets;

pub mod wayland;

pub use color::Color;
pub use geom::{Insets, Point, Rect, Size};
pub use render::{CpuRenderer, RenderBackend};
pub use theme::{Palette, Theme};
pub use widgets::font_weight;
