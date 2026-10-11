// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway Phase 2 stack-spike B (step 2.0): sctk layer-shell + glutin EGL + femtovg (GLES).
//
// Renders the same 420x560 "power menu" panel as spike-cpu, but on the GPU via an
// EGL window surface bound to the layer-shell wl_surface.
//
// CLOUD LIMITATION: this sandbox has no libEGL, so this spike BUILDS and links
// (glutin dlopens libEGL at runtime) but cannot be RUN here.
// Runtime numbers (idle/animated RSS, CPU under animation, frame pacing) are
// "needs real-machine validation". Build time + binary size + LOC are measured here.

use std::num::NonZeroU32;
use std::time::Instant;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::Display as GlutinDisplay;
use glutin::prelude::*;
use glutin::surface::{Surface as GlutinSurface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

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
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_surface};
use wayland_client::Proxy;
use wayland_client::{backend::ObjectId, Connection, QueueHandle};

const W: u32 = 420;
const H: u32 = 560;
const ROBOTO: &[u8] = include_bytes!("../../assets/fonts/Roboto-VF.ttf");

const ITEMS: &[&str] = &["Suspend", "Restart", "Shutdown", "Lock", "Logout"];

type GlWindowSurface = GlutinSurface<WindowSurface>;

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
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("lme-spike-gpu"), None);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(W, H);
    layer.commit();

    let mut app = Spike {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        first_configure: true,
        animate,
        layer,
        egl: None,
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
        "spike-gpu done: frames={} keys={} uptime={:?}",
        app.frames,
        app.keys,
        app.t0.elapsed()
    );
}

struct Egl {
    // Kept alive for the lifetime of the surface/context (the EGL display owns
    // config/context state); not directly read after init.
    #[allow(dead_code)]
    display: GlutinDisplay,
    surface: GlWindowSurface,
    context: glutin::context::PossiblyCurrentContext,
    canvas: femtovg::Canvas<femtovg::renderer::OpenGl>,
    roboto: femtovg::FontId,
}

struct Spike {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,

    exit: bool,
    first_configure: bool,
    animate: bool,
    layer: LayerSurface,
    egl: Option<Egl>,
    t0: Instant,
    frames: u64,
    keys: u64,
}

/// Create an EGL display/context/window-surface for a wayland-client surface.
/// The surface id pointers are wayland-backend protocol object pointers, the same
/// trick glutin's wayland path documents for non-winit clients.
fn create_egl(
    conn: &Connection,
    surface_id: &ObjectId,
    width: NonZeroU32,
    height: NonZeroU32,
) -> Result<Egl, Box<dyn std::error::Error>> {
    // Raw pointers only exist on the system (libwayland) backend, which this crate
    // selects via the `system` feature of wayland-client.
    let display_ptr = conn.backend().display_ptr();
    let surface_ptr = surface_id.as_ptr();
    let raw_display = RawDisplayHandle::Wayland(unsafe {
        WaylandDisplayHandle::new(std::ptr::NonNull::new_unchecked(
            display_ptr.cast::<std::ffi::c_void>(),
        ))
    });
    let raw_window = RawWindowHandle::Wayland(unsafe {
        WaylandWindowHandle::new(std::ptr::NonNull::new_unchecked(
            surface_ptr.cast::<std::ffi::c_void>(),
        ))
    });

    // SAFETY: the pointers stay alive for the lifetime of the connection & surface,
    // which outlive the EGL objects.
    let display =
        unsafe { GlutinDisplay::new(raw_display, glutin::display::DisplayApiPreference::Egl)? };

    let template = ConfigTemplateBuilder::new()
        .with_alpha_size(8)
        .with_transparency(true)
        .compatible_with_native_window(raw_window);
    let config = unsafe {
        display
            .find_configs(template.build())?
            .reduce(|c1, c2| {
                if c1.num_samples() > c2.num_samples() {
                    c1
                } else {
                    c2
                }
            })
            .ok_or("no EGL config with alpha")?
    };

    let context_attributes = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
        .build(None);
    // The raw window handle is passed via surface attributes, not context attributes.
    let not_current = unsafe { display.create_context(&config, &context_attributes)? };
    let attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(raw_window, width, height);
    let surface = unsafe { display.create_window_surface(&config, &attrs)? };

    let context = not_current.make_current(&surface)?;

    // femtovg on an arbitrary EGL context: only needs a proc-address loader.
    let renderer = unsafe {
        femtovg::renderer::OpenGl::new_from_function_cstr(|s| display.get_proc_address(s))
    }?;
    let text_context = femtovg::TextContext::default();
    let roboto = text_context.add_font_mem(ROBOTO)?;
    let mut canvas = femtovg::Canvas::new_with_text_context(renderer, text_context)?;
    canvas.set_size(width.get(), height.get(), 1.0);

    Ok(Egl {
        display,
        surface,
        context,
        canvas,
        roboto,
    })
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
        if self.first_configure {
            self.first_configure = false;
            let conn = _c.clone();
            let w = NonZeroU32::new(configure.new_size.0).unwrap_or(NonZeroU32::new(W).unwrap());
            let h = NonZeroU32::new(configure.new_size.1).unwrap_or(NonZeroU32::new(H).unwrap());
            match create_egl(&conn, &self.layer.wl_surface().id(), w, h) {
                Ok(egl) => {
                    self.egl = Some(egl);
                    self.draw(qh);
                }
                Err(e) => {
                    eprintln!("EGL init failed (expected in cloud sandbox without libEGL): {e}");
                    eprintln!("spike-gpu: build/size numbers are the deliverable; runtime = needs real-machine validation");
                    self.exit = true;
                }
            }
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
        let Some(egl) = self.egl.as_mut() else {
            return;
        };
        self.frames += 1;
        let phase = self.t0.elapsed().as_secs_f32() * 2.0 * std::f32::consts::PI;
        let opacity = if self.animate {
            0.72 + 0.13 * phase.sin()
        } else {
            0.85
        };

        let _ = egl.context.make_current(&egl.surface);
        egl.canvas.set_size(W, H, 1.0);
        egl.canvas
            .clear_rect(0, 0, W, H, femtovg::Color::rgba(0, 0, 0, 0));

        // translucent rounded panel
        let mut path = femtovg::Path::new();
        path.rounded_rect(16.0, 16.0, (W - 32) as f32, (H - 32) as f32, 24.0);
        let panel =
            femtovg::Paint::color(femtovg::Color::rgba(24, 24, 30, (opacity * 255.0) as u8));
        egl.canvas.fill_path(&path, &panel);

        // separator
        let mut sep = femtovg::Path::new();
        sep.move_to(32.0, 86.0);
        sep.line_to((W - 32) as f32, 86.0);
        let mut line = femtovg::Paint::color(femtovg::Color::rgba(255, 255, 255, 40));
        line.set_line_width(1.5);
        egl.canvas.stroke_path(&sep, &line);

        // title
        let mut text = femtovg::Paint::color(femtovg::Color::rgba(242, 242, 247, 255));
        text.set_font_size(34.0);
        text.set_font(&[egl.roboto]);
        text.set_text_baseline(femtovg::Baseline::Top);
        let _ = egl.canvas.fill_text(40.0, 40.0, "Power Menu", &text);

        let mut item_paint = femtovg::Paint::color(femtovg::Color::rgba(232, 232, 236, 255));
        item_paint.set_font_size(26.0);
        item_paint.set_font(&[egl.roboto]);
        item_paint.set_text_baseline(femtovg::Baseline::Top);
        let mut y = 110.0;
        for item in ITEMS {
            let _ = egl.canvas.fill_text(44.0, y, item, &item_paint);
            y += 64.0;
        }

        egl.canvas.flush();
        let _ = egl.surface.swap_buffers(&egl.context);

        self.layer
            .wl_surface()
            .frame(qh, FrameCallbackData(self.layer.wl_surface().clone()));
        self.layer.commit();
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
