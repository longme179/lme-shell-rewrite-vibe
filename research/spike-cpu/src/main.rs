// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway Phase 2 stack-spike A (step 2.0): sctk layer-shell + tiny-skia + cosmic-text, CPU/wl_shm.
//
// Renders a 420x560 "power menu" panel: translucent rounded rectangle, title and
// five item rows of Roboto text. Keyboard: Esc exits. Any other key is logged.
// --animate: redraw on every frame callback (pulsing panel opacity).

use std::sync::Arc;
use std::time::Instant;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{slot::SlotPool, Shm, ShmHandler};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

const W: u32 = 420;
const H: u32 = 560;
const ROBOTO: &[u8] = include_bytes!("../../assets/fonts/Roboto-VF.ttf");

const ITEMS: &[(&str, &str)] = &[
    ("Suspend", "system-suspend-symbolic"),
    ("Restart", "system-reboot-symbolic"),
    ("Shutdown", "system-shutdown-symbolic"),
    ("Lock", "system-lock-screen-symbolic"),
    ("Logout", "system-log-out-symbolic"),
];

fn main() {
    let animate = std::env::args().any(|a| a == "--animate");

    let conn = Connection::connect_to_env().expect("connect to WAYLAND_DISPLAY");
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    let surface = compositor.create_surface(&qh);

    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("lme-spike-cpu"), None);
    // Modal power-menu semantics: no anchor (centered), exclusive keyboard.
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(W, H);
    layer.commit();

    // Fonts: vendored Roboto VF (deterministic across machines) + system fonts.
    let roboto = cosmic_text::fontdb::Source::Binary(Arc::new(ROBOTO));
    let font_system = cosmic_text::FontSystem::new_with_fonts(vec![roboto]);
    let swash_cache = cosmic_text::SwashCache::new();

    let pool = SlotPool::new((W * H * 4) as usize, &shm).expect("pool");

    let mut app = Spike {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        first_configure: true,
        animate,
        pool,
        layer,
        font_system,
        swash_cache,
        t0: Instant::now(),
        frames: 0u64,
        keys: 0u64,
    };

    loop {
        // Exit cleanly when the compositor connection dies (e.g. mock stops).
        if event_queue.blocking_dispatch(&mut app).is_err() {
            eprintln!("compositor connection closed");
            break;
        }
        if app.exit {
            break;
        }
    }
    eprintln!(
        "spike-cpu done: frames={} keys={} uptime={:?}",
        app.frames,
        app.keys,
        app.t0.elapsed()
    );
}

struct Spike {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,

    exit: bool,
    first_configure: bool,
    animate: bool,
    pool: SlotPool,
    layer: LayerSurface,
    font_system: cosmic_text::FontSystem,
    swash_cache: cosmic_text::SwashCache,
    t0: Instant,
    frames: u64,
    keys: u64,
}

impl CompositorHandler for Spike {
    fn scale_factor_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &wl_surface::WlSurface,
        _f: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &wl_surface::WlSurface,
        _t: wl_output::Transform,
    ) {
    }
    fn surface_enter(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &wl_surface::WlSurface,
        _o: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &wl_surface::WlSurface,
        _o: &wl_output::WlOutput,
    ) {
    }

    fn frame(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        _s: &wl_surface::WlSurface,
        _t: u32,
    ) {
        // In animate mode redraw forever; in idle mode we are done after the first draw.
        if self.animate {
            self.draw(qh);
        }
    }
}

impl OutputHandler for Spike {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: wl_output::WlOutput) {}
    fn update_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: wl_output::WlOutput) {}
    fn output_destroyed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _o: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Spike {
    fn closed(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _l: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        _l: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _s: u32,
    ) {
        debug_assert_eq!(configure.new_size.0, W);
        if self.first_configure {
            self.first_configure = false;
            self.draw(qh);
        }
    }
}

impl SeatHandler for Spike {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _c: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            let _ = self.seat_state.get_keyboard(qh, &seat, None);
        }
    }
    fn remove_capability(
        &mut self,
        _c: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _capability: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Spike {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _s: &wl_surface::WlSurface,
        _serial: u32,
        _keys: &[u32],
        _keysyms: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _s: &wl_surface::WlSurface,
        _serial: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _k: &wl_keyboard::WlKeyboard,
        _s: u32,
        event: KeyEvent,
    ) {
        self.keys += 1;
        eprintln!("key: sym={:?} utf8={:?}", event.keysym, event.utf8);
        if event.keysym == Keysym::Escape {
            self.exit = true;
        }
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _e: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _e: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _m: Modifiers,
        _r: RawModifiers,
        _l: u32,
    ) {
    }
}

impl ShmHandler for Spike {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl Spike {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        self.frames += 1;
        let width = W as i32;
        let height = H as i32;
        let stride = width * 4;

        let (buffer, canvas) = self
            .pool
            .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        // ---- CPU render: tiny-skia RGBA pixmap, then swizzle into the BGRA canvas ----
        let mut pixmap = tiny_skia::Pixmap::new(W, H).expect("pixmap");
        let phase = self.t0.elapsed().as_secs_f32() * 2.0 * std::f32::consts::PI;
        let pulse = if self.animate {
            0.72 + 0.13 * phase.sin()
        } else {
            0.85
        };

        draw_panel(&mut pixmap, W as f32, H as f32, pulse);

        let fs = &mut self.font_system;
        let cache = &mut self.swash_cache;
        // Title
        draw_text(
            &mut pixmap,
            fs,
            cache,
            "Power Menu",
            28.0,
            40.0,
            34.0,
            tiny_skia::Color::from_rgba8(0xF2, 0xF2, 0xF7, 0xFF),
        );
        // Items
        let mut y = 110.0;
        for (label, _icon) in ITEMS {
            draw_text(
                &mut pixmap,
                fs,
                cache,
                label,
                26.0,
                44.0,
                y,
                tiny_skia::Color::from_rgba8(0xE8, 0xE8, 0xEC, 0xFF),
            );
            y += 64.0;
        }

        // RGBA (tiny-skia) -> BGRA (wl_shm Argb8888 on LE), premultiplied in place
        rgba_to_bgra(canvas, pixmap.data());

        self.layer.wl_surface().damage_buffer(0, 0, width, height);
        self.layer
            .wl_surface()
            .frame(qh, FrameCallbackData(self.layer.wl_surface().clone()));
        buffer.attach_to(self.layer.wl_surface()).expect("attach");
        self.layer.commit();
    }
}

/// Translucent rounded-rect panel with a header separator, drawn with tiny-skia.
fn draw_panel(pixmap: &mut tiny_skia::Pixmap, w: f32, h: f32, opacity: f32) {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(24, 24, 30, (opacity * 255.0) as u8);
    paint.anti_alias = true;

    let path = rounded_rect_path(16.0, 16.0, w - 32.0, h - 32.0, 24.0);
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );

    // header separator
    let mut stroke = tiny_skia::Paint::default();
    stroke.set_color_rgba8(255, 255, 255, 40);
    stroke.anti_alias = true;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(32.0, 86.0);
    pb.line_to(w - 32.0, 86.0);
    let sep = pb.finish().unwrap();
    pixmap.stroke_path(
        &sep,
        &stroke,
        &tiny_skia::Stroke {
            width: 1.5,
            ..Default::default()
        },
        tiny_skia::Transform::identity(),
        None,
    );
}

/// Rounded rectangle path (hand-rolled: tiny-skia has no built-in round-rect).
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
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
    pb.finish().unwrap()
}

/// Shape + rasterize one line of text via cosmic-text and composite it into the pixmap.
#[allow(clippy::too_many_arguments)] // throwaway spike: plain parameter list kept simple
fn draw_text(
    pixmap: &mut tiny_skia::Pixmap,
    fs: &mut cosmic_text::FontSystem,
    cache: &mut cosmic_text::SwashCache,
    text: &str,
    size: f32,
    x: f32,
    y: f32,
    color: tiny_skia::Color,
) {
    let metrics = cosmic_text::Metrics::new(size, size * 1.3);
    let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name("Roboto"));
    let mut buffer = cosmic_text::Buffer::new(fs, metrics);
    buffer.set_size(Some(1_000.0), Some(1_000.0));
    buffer.set_text(text, &attrs, cosmic_text::Shaping::Advanced, None);
    buffer.shape_until_scroll(fs, false);

    let c8 = color.to_color_u8();
    let run_color = cosmic_text::Color::rgb(c8.red(), c8.green(), c8.blue());
    for line in buffer.layout_runs() {
        // layout y positions glyphs relative to the line baseline; pass `y` as the
        // intended top of the text block, so compensate with the run's ascent.
        let base = y + line.line_y - line.line_top;
        for glyph in line.glyphs.iter() {
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let image = cache.get_image(fs, physical.cache_key);
            if let Some(img) = image {
                // absolute top-left of the glyph bitmap
                let ix = x as i32 + physical.x + img.placement.left;
                let iy = base as i32 + physical.y - img.placement.top;
                match img.content {
                    cosmic_text::SwashContent::Mask => {
                        composite_mask(pixmap, img, ix, iy, run_color);
                    }
                    cosmic_text::SwashContent::Color => {
                        composite_color_bitmap(pixmap, img, ix, iy);
                    }
                    cosmic_text::SwashContent::SubpixelMask => {}
                }
            }
        }
    }
}

/// Composite an 8-bit alpha-mask glyph onto a premultiplied-RGBA tiny-skia pixmap
/// (standard "source-over" with constant color).
fn composite_mask(
    pixmap: &mut tiny_skia::Pixmap,
    image: &cosmic_text::SwashImage,
    dx: i32,
    dy: i32,
    color: cosmic_text::Color,
) {
    let (iw, ih) = (image.placement.width as i32, image.placement.height as i32);
    let data = &image.data;
    let (cr, cg, cb) = (color.r() as u32, color.g() as u32, color.b() as u32);
    for row in 0..ih {
        for col in 0..iw {
            let alpha = data.get((row * iw + col) as usize).copied().unwrap_or(0);
            if alpha == 0 {
                continue;
            }
            let px = dx + col;
            let py = dy + row;
            if px < 0 || py < 0 || px >= pixmap.width() as i32 || py >= pixmap.height() as i32 {
                continue;
            }
            let idx = (py as u32 * pixmap.width() + px as u32) as usize;
            let dst = pixmap.pixels_mut()[idx];
            let (dr, dg, db, da) = (
                dst.red() as u32,
                dst.green() as u32,
                dst.blue() as u32,
                dst.alpha() as u32,
            );
            let sa = alpha as u32;
            let inv = 255 - sa;
            // premultiplied source
            let sr = (cr * sa + 127) / 255;
            let sg = (cg * sa + 127) / 255;
            let sb = (cb * sa + 127) / 255;
            let or = (sr + (dr * inv + 127) / 255).min(255) as u8;
            let og = (sg + (dg * inv + 127) / 255).min(255) as u8;
            let ob = (sb + (db * inv + 127) / 255).min(255) as u8;
            let oa = (sa + (da * inv + 127) / 255).min(255) as u8;
            pixmap.pixels_mut()[idx] =
                tiny_skia::PremultipliedColorU8::from_rgba(or, og, ob, oa).expect("valid premult");
        }
    }
}

fn composite_color_bitmap(
    pixmap: &mut tiny_skia::Pixmap,
    image: &cosmic_text::SwashImage,
    dx: i32,
    dy: i32,
) {
    let (iw, ih) = (image.placement.width as i32, image.placement.height as i32);
    let data = &image.data;
    for row in 0..ih {
        for col in 0..iw {
            let base = ((row * iw + col) * 4) as usize;
            let (b, g, r, a) = (
                data.get(base).copied().unwrap_or(0),
                data.get(base + 1).copied().unwrap_or(0),
                data.get(base + 2).copied().unwrap_or(0),
                data.get(base + 3).copied().unwrap_or(0),
            );
            if a == 0 {
                continue;
            }
            let px = dx + col;
            let py = dy + row;
            if px < 0 || py < 0 || px >= pixmap.width() as i32 || py >= pixmap.height() as i32 {
                continue;
            }
            let idx = (py as u32 * pixmap.width() + px as u32) as usize;
            // NOTE: swash Color content assumed premultiplied (only used for color glyphs)
            pixmap.pixels_mut()[idx] =
                tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a).expect("valid premult");
        }
    }
}

/// Swizzle a premultiplied RGBA byte buffer into a BGRA (wl_shm Argb8888 LE) canvas.
fn rgba_to_bgra(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0.iter())
    {
        *d = [s[2], s[1], s[0], s[3]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swizzle_swaps_r_and_b_only() {
        let src = [10u8, 20, 30, 40]; // R=10 G=20 B=30 A=40
        let mut dst = [0u8; 4];
        rgba_to_bgra(&mut dst, &src);
        assert_eq!(dst, [30, 20, 10, 40]);
    }

    #[test]
    fn rounded_rect_path_stays_in_bounds() {
        let path = rounded_rect_path(16.0, 16.0, 388.0, 528.0, 24.0);
        let bounds = path.bounds();
        assert!(bounds.left() >= 15.0 && bounds.right() <= 405.0);
        assert!(bounds.top() >= 15.0 && bounds.bottom() <= 545.0);
    }
}

delegate_registry!(Spike);

impl ProvidesRegistryState for Spike {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(Spike);
