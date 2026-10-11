// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway Phase 2 stack-spike C (step 2.0): sctk layer-shell + Slint software renderer.
//
// Renders the same 420x560 "power menu" panel as spike-cpu, but the UI is declared in
// .slint markup and rendered by Slint's software renderer into the wl_shm buffer.
// Esc exits (slint FocusScope + Key::Escape). --animate: pulsing opacity via a Timer.

use std::cell::Cell;
use std::num::NonZeroU32;
use std::rc::Rc;
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

use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType},
    Platform, WindowAdapter,
};
use slint::SharedString;

const W: u32 = 420;
const H: u32 = 560;

slint::slint! {
export component App inherits Window {
    in property <bool> animate: false;
    in-out property <float> t: 0.0;
    callback quit();
    callback tick(float);

    forward-focus: rootFocus;
    background: transparent;

    Rectangle {
        x: 16px; y: 16px;
        width: parent.width - 32px;
        height: parent.height - 32px;
        border-radius: 24px;
        background: rgba(24, 24, 30, 0.72 + 0.13 * Math.sin(root.t * 360deg));

        Rectangle {
            y: 70px; x: 16px;
            width: parent.width - 32px;
            height: 1.5px;
            background: rgba(255, 255, 255, 0.16);
        }

        Text {
            text: "Power Menu";
            x: 24px; y: 20px;
            font-size: 34px;
            color: #F2F2F7FF;
            font-family: "Roboto";
        }

        VerticalLayout {
            y: 86px; x: 28px;
            width: parent.width - 56px;
            spacing: 24px;
            for item[i] in ["Suspend", "Restart", "Shutdown", "Lock", "Logout"]: Text {
                text: item;
                font-size: 26px;
                color: #E8E8ECFF;
                font-family: "Roboto";
            }
        }
    }

    rootFocus := FocusScope {
        key-pressed(event) => {
            if (event.text == "\u{1b}") {
                root.quit();
                return accept;
            }
            return reject;
        }
    }

    Timer {
        interval: 16ms;
        running: root.animate;
        triggered => {
            root.t = Math.mod(root.t + 0.016, 1.0);
            root.tick(root.t);
        }
    }
}
}

fn main() {
    let animate = std::env::args().any(|a| a == "--animate");

    let conn = Connection::connect_to_env().expect("connect to WAYLAND_DISPLAY");
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    let surface = compositor.create_surface(&qh);

    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("lme-spike-slint"),
        None,
    );
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(W, H);
    layer.commit();

    // Slint on a custom platform: a MinimalSoftwareWindow renders into our buffer.
    let msw = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    struct WaylandPlatform {
        window: Rc<MinimalSoftwareWindow>,
    }
    impl Platform for WaylandPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.window.clone())
        }
    }
    slint::platform::set_platform(Box::new(WaylandPlatform {
        window: msw.clone(),
    }))
    .expect("set_platform");

    let ui = App::new().expect("App::new");
    let quit_requested = Rc::new(Cell::new(false));
    {
        let flag = quit_requested.clone();
        ui.on_quit(move || {
            flag.set(true);
        });
    }
    {
        // NOTE (spike finding): after timer-driven imperative property changes, the
        // MinimalSoftwareWindow needs_redraw flag was not set automatically in this
        // setup; request it explicitly (documented in docs/adr/0001).
        let ui2 = ui.as_weak();
        ui.on_tick(move |_t| {
            if let Some(u) = ui2.upgrade() {
                u.window().request_redraw();
            }
        });
    }
    ui.set_animate(animate);

    let pool = SlotPool::new((W * H * 4) as usize, &shm).expect("pool");

    let mut app = Spike {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        first_configure: true,
        frame_pending: false,
        layer,
        pool,
        msw,
        quit_requested,
        t0: Instant::now(),
        frames: 0u64,
        keys: 0u64,
    };

    // Custom-platform pump loop: wayland events + slint timers/animations.
    // (blocking_dispatch would sleep through slint's 16 ms Timer deadlines.)
    loop {
        if event_queue.dispatch_pending(&mut app).is_err() {
            eprintln!("compositor connection closed");
            break;
        }
        if conn.flush().is_err() {
            eprintln!("compositor connection closed (flush)");
            break;
        }
        slint::platform::update_timers_and_animations();
        if !app.frame_pending {
            app.draw(&qh);
        }
        if app.exit || app.quit_requested.get() {
            break;
        }
        if let Some(guard) = event_queue.prepare_read() {
            let backend = conn.backend();
            let fd = backend.poll_fd();
            let mut fds = [rustix::event::PollFd::new(
                &fd,
                rustix::event::PollFlags::IN,
            )];
            let timeout = slint::platform::duration_until_next_timer_update();
            let ts = timeout.map(|d| rustix::event::Timespec {
                tv_sec: d.as_secs() as rustix::event::Secs,
                tv_nsec: d.subsec_nanos() as rustix::event::Nsecs,
            });
            if rustix::event::poll(&mut fds, ts.as_ref()).is_err() {
                eprintln!("poll failed");
                break;
            }
            if fds[0].revents().contains(rustix::event::PollFlags::IN) {
                // WouldBlock is benign (socket drained between poll and read).
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        eprintln!("compositor connection closed (read)");
                        break;
                    }
                }
            }
        } else {
            // Another thread holds the read guard; brief pause.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    eprintln!(
        "spike-slint done: frames={} keys={} uptime={:?}",
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
    frame_pending: bool,
    layer: LayerSurface,
    pool: SlotPool,
    msw: Rc<MinimalSoftwareWindow>,
    quit_requested: Rc<Cell<bool>>,
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
        // A requested frame callback arrived: we may draw again now.
        self.frame_pending = false;
        slint::platform::update_timers_and_animations();
        self.draw(qh);
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
            let w = NonZeroU32::new(configure.new_size.0).unwrap_or(NonZeroU32::new(W).unwrap());
            let h = NonZeroU32::new(configure.new_size.1).unwrap_or(NonZeroU32::new(H).unwrap());
            // Call the WindowAdapter trait method (not the inherent one) so the
            // adapter size is updated and a Resized event is dispatched to slint.
            slint::platform::WindowAdapter::set_size(
                &*self.msw,
                slint::PhysicalSize::new(w.get(), h.get()).into(),
            );
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
        let text: SharedString = match event.keysym {
            Keysym::Escape => slint::platform::Key::Escape.into(),
            Keysym::Return | Keysym::KP_Enter => slint::platform::Key::Return.into(),
            Keysym::BackSpace => slint::platform::Key::Backspace.into(),
            Keysym::Left => slint::platform::Key::LeftArrow.into(),
            Keysym::Right => slint::platform::Key::RightArrow.into(),
            Keysym::Up => slint::platform::Key::UpArrow.into(),
            Keysym::Down => slint::platform::Key::DownArrow.into(),
            _ => match &event.utf8 {
                Some(s) => s.as_str().into(),
                None => return,
            },
        };
        eprintln!("key -> {:?}", text);
        self.msw
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        self.msw
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        // key presses may change slint state; advance timers/animations
        slint::platform::update_timers_and_animations();
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
        let width = W as i32;
        let height = H as i32;
        let stride = width * 4;
        let (buffer, canvas) = self
            .pool
            .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        let msw = self.msw.clone();
        if msw.draw_if_needed(|renderer| {
            self.frames += 1;
            // wl_shm Argb8888 is BGRA on little-endian; slint renders RGBA premultiplied.
            let pixels: &mut [PremultipliedRgbaColor] = unsafe {
                std::slice::from_raw_parts_mut(canvas.as_mut_ptr() as *mut _, canvas.len() / 4)
            };
            renderer.render(pixels, width as usize);
            // slint RGBA -> wl_shm BGRA (both premultiplied)
            for px in pixels.iter_mut() {
                core::mem::swap(&mut px.red, &mut px.blue);
            }
            self.frame_pending = true;
        }) {
            self.layer.wl_surface().damage_buffer(0, 0, width, height);
            self.layer
                .wl_surface()
                .frame(qh, FrameCallbackData(self.layer.wl_surface().clone()));
            buffer.attach_to(self.layer.wl_surface()).expect("attach");
            self.layer.commit();
        }
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
