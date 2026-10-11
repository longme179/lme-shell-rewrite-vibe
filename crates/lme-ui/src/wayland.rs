// SPDX-License-Identifier: GPL-3.0-or-later
//! Wayland application core: connection, outputs, seats, layer-shell windows,
//! shm buffers, frame-callback-driven rendering.
//!
//! Frame discipline (mission quality bar):
//! 1. Wayland dispatch only mutates flags/queues — never renders.
//! 2. A `calloop::ping` poked from handlers runs the render pass on the next
//!    loop iteration (outside dispatch): layout → collect → raster → attach →
//!    damage → (frame callback only while animating) → commit.
//! 3. Static content renders once; idle surfaces request no frame callbacks,
//!    so an idle shell performs zero Wayland traffic.

use crate::anim::Animator;
use crate::color::Color;
use crate::draw::DrawCmd;
use crate::render::{CpuRenderer, RenderBackend};
use crate::shadow::ShadowCache;
use crate::text::FontBook;
use crate::theme::Theme;
use crate::widgets::{Ctx, InputEvent, KeyInput, UiEvent, Widget};
use calloop::ping::Ping;
// Re-exported for crates building windows (WindowSpec fields).
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
pub use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use std::collections::HashMap;
use std::time::Instant;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

/// Info about one output.
#[derive(Clone)]
pub struct OutputRec {
    pub output: wl_output::WlOutput,
    pub name: String,
    /// Logical size (px).
    pub logical: (i32, i32),
    pub scale: i32,
}

/// Window creation spec (layer-shell parameters).
pub struct WindowSpec {
    pub namespace: String,
    pub layer: Layer,
    /// (top, right, bottom, left) anchors.
    pub anchor: (bool, bool, bool, bool),
    /// Exclusive zone; -1 = not set.
    pub exclusive_zone: i32,
    /// Fixed size request; None = compositor decides (anchored edges).
    pub size: Option<(u32, u32)>,
    pub keyboard: KeyboardInteractivity,
    /// Margins (top, right, bottom, left) — protocol order.
    pub margins: (i32, i32, i32, i32),
    pub output: Option<wl_output::WlOutput>,
    /// Painted under everything; transparent surfaces use a=0.
    pub background: Color,
}

impl WindowSpec {
    /// Anchored on all edges of the output (full-overlay surface).
    pub fn full_overlay(namespace: &str, keyboard: KeyboardInteractivity) -> Self {
        Self {
            namespace: namespace.to_string(),
            layer: Layer::Overlay,
            anchor: (true, true, true, true),
            exclusive_zone: -1,
            size: None,
            keyboard,
            margins: (0, 0, 0, 0),
            output: None,
            background: Color::rgba(0.0, 0.0, 0.0, 0.0),
        }
    }
}

pub struct Window {
    pub id: u64,
    pub layer: LayerSurface,
    pub output: Option<wl_output::WlOutput>,
    pool: SlotPool,
    renderer: CpuRenderer,
    pub size: (u32, u32),
    pub root: Box<dyn Widget>,
    background: Color,
    needs_layout: bool,
    needs_render: bool,
    frame_pending: bool,
    pub animating: bool,
    pub focus: Option<u64>,
    pub configured: bool,
}

impl Window {
    pub fn root(&mut self) -> &mut Box<dyn Widget> {
        &mut self.root
    }
}

/// Shell-side logic hooks; implemented by the daemon.
pub trait AppHandler {
    fn output_added(&mut self, state: &mut UiState, output: &OutputRec);
    fn output_removed(&mut self, state: &mut UiState, name: &str);
    fn window_closed(&mut self, state: &mut UiState, window_id: u64);
    fn ui_event(&mut self, state: &mut UiState, window_id: u64, ev: UiEvent);
    /// Key not consumed by the widget tree (e.g. Escape).
    fn key(&mut self, state: &mut UiState, window_id: u64, key: &KeyInput);
    /// Called each frame pass while animations run.
    fn tick(&mut self, state: &mut UiState);
    /// The daemon's theme, exposed to the render pass.
    fn theme(&self) -> &Theme;
    /// Downcast support so the daemon can recover its concrete Shell from
    /// the handler box during with_shell calls.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Placeholder installed while the real handler is re-entrantly invoked
/// (handler methods receive `&mut UiState`, which contains the handler —
/// the swap makes that possible without aliasing).
struct NoopShell {
    theme: Theme,
}

impl AppHandler for NoopShell {
    fn output_added(&mut self, _s: &mut UiState, _o: &OutputRec) {}
    fn output_removed(&mut self, _s: &mut UiState, _n: &str) {}
    fn window_closed(&mut self, _s: &mut UiState, _w: u64) {}
    fn ui_event(&mut self, _s: &mut UiState, _w: u64, _e: UiEvent) {}
    fn key(&mut self, _s: &mut UiState, _w: u64, _k: &KeyInput) {}
    fn tick(&mut self, _s: &mut UiState) {}
    fn theme(&self) -> &Theme {
        &self.theme
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl UiState {
    /// Invoke a handler method with `self` as the state argument.
    pub fn with_shell<R>(
        &mut self,
        f: impl FnOnce(&mut Box<dyn AppHandler>, &mut UiState) -> R,
    ) -> R {
        let mut shell = std::mem::replace(
            &mut self.shell,
            Box::new(NoopShell {
                theme: Theme::default_with(&crate::theme::Palette::default()),
            }),
        );
        let r = f(&mut shell, self);
        self.shell = shell;
        r
    }
}

pub struct UiState {
    // sctk state
    registry_state: RegistryState,
    compositor: CompositorState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    layer_shell: LayerShell,

    // connection plumbing
    pub conn: Connection,
    qh: QueueHandle<Self>,
    pending_queue: Option<wayland_client::EventQueue<Self>>,

    // app data
    pub shell: Box<dyn AppHandler>,
    pub fonts: FontBook,
    pub shadows: ShadowCache,
    pub anims: Animator,
    outputs: Vec<OutputRec>,
    windows: HashMap<u64, Window>,
    next_window: u64,
    next_output_id: u64,
    keyboard_window: Option<u64>,
    pointer_window: Option<u64>,
    pointer_serial: u32,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: (bool, bool), // (shift, ctrl)
    render_queue: Vec<u64>,
    ping: Option<Ping>,
    cursor_pool: Option<SlotPool>,
    /// The daemon's loop handle (set by the daemon main) — lets shell code
    /// schedule one-shot timers (OSD expiry) from inside callbacks.
    pub handle: Option<calloop::LoopHandle<'static, UiState>>,
    pub exit: bool,
}

impl UiState {
    /// Connect via WAYLAND_DISPLAY and bind globals. The initial roundtrip is
    /// performed by `registry_queue_init`; output/seat events arrive on the
    /// first loop dispatch (event-driven startup).
    pub fn connect(shell: Box<dyn AppHandler>) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, event_queue) = registry_queue_init(&conn)?;
        let qh = event_queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)?;
        let layer_shell = LayerShell::bind(&globals, &qh)?;
        let shm = Shm::bind(&globals, &qh)?;
        let registry_state = RegistryState::new(&globals);
        let seat_state = SeatState::new(&globals, &qh);
        let output_state = OutputState::new(&globals, &qh);

        Ok(UiState {
            registry_state,
            compositor,
            seat_state,
            output_state,
            shm,
            layer_shell,
            conn: conn.clone(),
            qh,
            pending_queue: Some(event_queue),
            shell,
            fonts: FontBook::new(),
            shadows: ShadowCache::new(),
            anims: Animator::new(),
            outputs: Vec::new(),
            windows: HashMap::new(),
            next_window: 1,
            next_output_id: 1,
            keyboard_window: None,
            pointer_window: None,
            pointer_serial: 0,
            pointer: None,
            modifiers: (false, false),
            render_queue: Vec::new(),
            ping: None,
            cursor_pool: None,
            handle: None,
            exit: false,
        })
    }

    /// Insert the Wayland source + render ping into the daemon's loop.
    /// MUST be called once before running the loop.
    pub fn insert_sources(
        &mut self,
        handle: &calloop::LoopHandle<'static, Self>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let queue = self
            .pending_queue
            .take()
            .ok_or("insert_sources called twice")?;
        let source = calloop_wayland_source::WaylandSource::new(self.conn.clone(), queue);
        source.insert(handle.clone())?;
        let (ping, ping_source) = calloop::ping::make_ping()?;
        handle.insert_source(ping_source, |_, _, state| {
            state.render_pass();
        })?;
        self.ping = Some(ping);
        Ok(())
    }

    // ---- window API (used by the daemon) ----

    pub fn create_window(&mut self, spec: WindowSpec) -> u64 {
        let id = self.next_window;
        self.next_window += 1;
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            spec.layer,
            Some(&spec.namespace),
            spec.output.as_ref(),
        );
        layer.set_keyboard_interactivity(spec.keyboard);
        let mut anchor = Anchor::empty();
        if spec.anchor.0 {
            anchor |= Anchor::TOP;
        }
        if spec.anchor.1 {
            anchor |= Anchor::RIGHT;
        }
        if spec.anchor.2 {
            anchor |= Anchor::BOTTOM;
        }
        if spec.anchor.3 {
            anchor |= Anchor::LEFT;
        }
        layer.set_anchor(anchor);
        if spec.exclusive_zone != -1 {
            layer.set_exclusive_zone(spec.exclusive_zone);
        }
        layer.set_margin(
            spec.margins.0,
            spec.margins.1,
            spec.margins.2,
            spec.margins.3,
        );
        if let Some((w, h)) = spec.size {
            layer.set_size(w, h);
        }
        layer.commit();
        let (init_w, init_h) = spec.size.unwrap_or((4, 4));
        let pool = SlotPool::new((init_w.max(1) * init_h.max(1) * 4) as usize, &self.shm)
            .expect("slot pool");
        let window = Window {
            id,
            layer,
            output: spec.output,
            pool,
            renderer: CpuRenderer::new(init_w.max(1), init_h.max(1)),
            size: (init_w.max(1), init_h.max(1)),
            root: Box::new(crate::widgets::Spacer::fill()),
            background: spec.background,
            needs_layout: true,
            needs_render: true,
            frame_pending: false,
            animating: false,
            focus: None,
            configured: false,
        };
        self.windows.insert(id, window);
        id
    }

    pub fn close_window(&mut self, id: u64) {
        if self.windows.remove(&id).is_some() {
            // LayerSurface destruction happens on drop.
            self.render_queue.retain(|i| *i != id);
            if self.pointer_window == Some(id) {
                self.pointer_window = None;
            }
            if self.keyboard_window == Some(id) {
                self.keyboard_window = None;
            }
        }
    }

    pub fn window(&mut self, id: u64) -> Option<&mut Window> {
        self.windows.get_mut(&id)
    }

    /// Replace a window's widget root and request a fresh layout+render.
    pub fn set_root(&mut self, id: u64, root: Box<dyn Widget>) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.root = root;
            w.needs_layout = true;
            w.needs_render = true;
            self.request_render(id);
        }
    }

    /// Mutate a window's root via a typed closure (downcast at the call site).
    /// Sets the window animating flag if requested (for slide-in reveals).
    pub fn with_window<R>(
        &mut self,
        id: u64,
        f: impl FnOnce(&mut Window, &mut Ctx) -> R,
    ) -> Option<R> {
        let theme = self.shell.theme();
        let now = Instant::now();
        let mut queue = Vec::new();
        let focused = self.windows.get(&id).and_then(|w| w.focus);
        let (result, repaint) = {
            let mut ctx = Ctx {
                theme,
                fonts: &mut self.fonts,
                anims: &mut self.anims,
                shadows: &mut self.shadows,
                queue: &mut queue,
                now,
                focused,
                repaint: false,
            };
            let w = self.windows.get_mut(&id)?;
            let r = f(w, &mut ctx);
            (r, ctx.repaint)
        };
        let mut q = queue;
        for ev in q.drain(..) {
            self.with_shell(|shell, state| shell.ui_event(state, id, ev));
        }
        if repaint {
            self.request_render(id);
        }
        Some(result)
    }

    pub fn request_render(&mut self, id: u64) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.needs_render = true;
        }
        if !self.render_queue.contains(&id) {
            self.render_queue.push(id);
        }
        if let Some(p) = &self.ping {
            p.ping();
        }
    }

    pub fn outputs(&self) -> &[OutputRec] {
        &self.outputs
    }

    /// The output of the keyboard-focused window, else the first output.
    pub fn focused_output(&self) -> Option<OutputRec> {
        if let Some(wid) = self.keyboard_window {
            if let Some(w) = self.windows.get(&wid) {
                if let Some(o) = &w.output {
                    if let Some(rec) = self.outputs.iter().find(|r| r.output == *o) {
                        return Some(rec.clone());
                    }
                }
            }
        }
        self.outputs.first().cloned()
    }

    /// Request a frame callback for a window (keeps animation running).
    pub fn request_frame(&mut self, id: u64) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if !w.frame_pending && w.configured {
            w.layer
                .wl_surface()
                .frame(&self.qh, FrameCallbackData(w.layer.wl_surface().clone()));
            w.frame_pending = true;
        }
    }

    pub fn set_keyboard_focus(&mut self, window_id: u64, widget: Option<u64>) {
        if let Some(w) = self.windows.get_mut(&window_id) {
            w.focus = widget;
            self.request_render(window_id);
        }
    }

    pub fn window_ids(&self) -> Vec<u64> {
        self.windows.keys().copied().collect()
    }

    pub fn request_exit(&mut self) {
        self.exit = true;
    }

    fn find_window_by_surface(&self, surface: &wl_surface::WlSurface) -> Option<u64> {
        self.windows
            .iter()
            .find(|(_, w)| w.layer.wl_surface() == surface)
            .map(|(id, _)| *id)
    }

    // ---- render pass (runs OUTSIDE wayland dispatch, via ping) ----

    fn render_pass(&mut self) {
        let now = Instant::now();
        let animating = self.anims.tick(now);
        if animating {
            self.with_shell(|shell, state| shell.tick(state));
            let ids: Vec<u64> = self.windows.keys().copied().collect();
            for id in ids {
                if let Some(w) = self.windows.get_mut(&id) {
                    if w.animating {
                        w.needs_render = true;
                        if !self.render_queue.contains(&id) {
                            self.render_queue.push(id);
                        }
                    }
                }
            }
        }
        let queue: Vec<u64> = std::mem::take(&mut self.render_queue);
        for id in &queue {
            self.render_window(*id);
        }
        // Keep frame callbacks flowing only while animating.
        if animating {
            let ids: Vec<u64> = self
                .windows
                .iter()
                .filter(|(_, w)| w.animating && w.configured)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                self.request_frame(id);
            }
        }
    }

    #[allow(clippy::needless_return)]
    fn render_window(&mut self, id: u64) {
        let (width, height) = match self.windows.get(&id) {
            Some(w) if w.configured => (w.size.0 as i32, w.size.1 as i32),
            _ => return,
        };
        let needs_render = self
            .windows
            .get(&id)
            .map(|w| w.needs_render)
            .unwrap_or(false);
        if !needs_render || width == 0 || height == 0 {
            return;
        }
        let theme = self.shell.theme();
        let now = Instant::now();
        let mut cmds: Vec<DrawCmd> = Vec::new();
        let mut queue = Vec::new();
        {
            let focused = self.windows.get(&id).and_then(|w| w.focus);
            let mut ctx = Ctx {
                theme,
                fonts: &mut self.fonts,
                anims: &mut self.anims,
                shadows: &mut self.shadows,
                queue: &mut queue,
                now,
                focused,
                repaint: false,
            };
            let Some(w) = self.windows.get_mut(&id) else {
                return;
            };
            if w.needs_layout {
                let bounds = crate::geom::Rect::from_size(crate::geom::Size::new(
                    w.size.0 as f32,
                    w.size.1 as f32,
                ));
                w.root.layout(&mut ctx, bounds);
                w.needs_layout = false;
            }
            w.root.paint(&mut ctx, &mut cmds);
            let Some(w) = self.windows.get_mut(&id) else {
                return;
            };
            w.renderer.resize(w.size.0, w.size.1);
            w.renderer.render(&cmds, &mut self.fonts, w.background);
        }
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        let (buffer, canvas) =
            match w
                .pool
                .create_buffer(width, height, width * 4, wl_shm::Format::Argb8888)
            {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("lme-ui: buffer creation failed for window {id}: {e:?}");
                    return;
                }
            };
        // Copy BGRA frame into the wl_shm canvas.
        canvas.copy_from_slice(w.renderer.bgra());
        let surface = w.layer.wl_surface().clone();
        surface.damage_buffer(0, 0, width, height);
        if w.animating && !w.frame_pending {
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            w.frame_pending = true;
        }
        if let Err(e) = buffer.attach_to(&surface) {
            eprintln!("lme-ui: attach failed for window {id}: {e:?}");
            return;
        }
        w.layer.commit();
        w.needs_render = false;
        // Widget-queued events from paint (rare) go to the shell.
        let mut q = queue;
        for ev in q.drain(..) {
            self.with_shell(|shell, state| shell.ui_event(state, id, ev));
        }
    }
}

// ---- sctk handler impls ----

impl CompositorHandler for UiState {
    fn scale_factor_changed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _s: &wl_surface::WlSurface,
        _f: i32,
    ) {
        // Real-compositor scale change reconfigures the surface; the mock
        // never changes scale. Needs real-machine validation.
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
        _q: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if let Some(id) = self.find_window_by_surface(surface) {
            if let Some(w) = self.windows.get_mut(&id) {
                w.frame_pending = false;
                if w.animating {
                    w.needs_render = true;
                    if !self.render_queue.contains(&id) {
                        self.render_queue.push(id);
                    }
                }
            }
            if let Some(p) = &self.ping {
                p.ping();
            }
        }
    }
}

impl OutputHandler for UiState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, o: wl_output::WlOutput) {
        let info = self.output_state.info(&o);
        let name = info
            .as_ref()
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| format!("output-{}", self.next_output_id));
        let logical = info
            .as_ref()
            .and_then(|i| i.logical_size)
            .map(|s| (s.0, s.1))
            .unwrap_or((1920, 1080));
        let scale = info.as_ref().map(|i| i.scale_factor).unwrap_or(1);
        if self.outputs.iter().any(|r| r.output == o) {
            return;
        }
        self.next_output_id += 1;
        let rec = OutputRec {
            output: o,
            name,
            logical,
            scale,
        };
        self.outputs.push(rec.clone());
        let rec_clone = rec;
        self.with_shell(|shell, state| shell.output_added(state, &rec_clone));
    }
    fn update_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, o: wl_output::WlOutput) {
        let info = self.output_state.info(&o);
        if let Some(rec) = self.outputs.iter_mut().find(|r| r.output == o) {
            if let Some(i) = info {
                if let Some(s) = i.logical_size {
                    rec.logical = (s.0, s.1);
                }
                rec.scale = i.scale_factor;
            }
        }
    }
    fn output_destroyed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        o: wl_output::WlOutput,
    ) {
        if let Some(pos) = self.outputs.iter().position(|r| r.output == o) {
            let rec = self.outputs.remove(pos);
            let name = rec.name;
            self.with_shell(|shell, state| shell.output_removed(state, &name));
        }
    }
}

impl LayerShellHandler for UiState {
    fn closed(&mut self, _c: &Connection, _q: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(id) = self
            .windows
            .iter()
            .find(|(_, w)| w.layer.wl_surface() == layer.wl_surface())
            .map(|(id, _)| *id)
        {
            self.windows.remove(&id);
            self.render_queue.retain(|i| *i != id);
            if self.pointer_window == Some(id) {
                self.pointer_window = None;
            }
            if self.keyboard_window == Some(id) {
                self.keyboard_window = None;
            }
            self.with_shell(|shell, state| shell.window_closed(state, id));
        }
    }
    fn configure(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if let Some(id) = self
            .windows
            .iter()
            .find(|(_, w)| w.layer.wl_surface() == layer.wl_surface())
            .map(|(id, _)| *id)
        {
            if let Some(w) = self.windows.get_mut(&id) {
                let (nw, nh) = (configure.new_size.0.max(1), configure.new_size.1.max(1));
                let size_changed = w.size != (nw, nh);
                w.size = (nw, nh);
                if size_changed || !w.configured {
                    w.needs_layout = true;
                }
                w.configured = true;
                w.needs_render = true;
                if !self.render_queue.contains(&id) {
                    self.render_queue.push(id);
                }
                if let Some(p) = &self.ping {
                    p.ping();
                }
            }
        }
    }
}

impl SeatHandler for UiState {
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
        if capability == Capability::Pointer {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => eprintln!("lme-ui: pointer unavailable: {e:?}"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _c: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        _capability: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for UiState {
    fn enter(
        &mut self,
        _c: &Connection,
        _qh: &QueueHandle<Self>,
        _k: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _keys: &[u32],
        _keysyms: &[Keysym],
    ) {
        self.keyboard_window = self.find_window_by_surface(surface);
    }
    fn leave(
        &mut self,
        _c: &Connection,
        _qh: &QueueHandle<Self>,
        _k: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        self.keyboard_window = None;
    }
    fn press_key(
        &mut self,
        _c: &Connection,
        _qh: &QueueHandle<Self>,
        _k: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let Some(win) = self.keyboard_window else {
            return;
        };
        let (shift, ctrl) = self.modifiers;
        let input = KeyInput {
            sym: u32::from(event.keysym),
            utf8: event.utf8.clone().unwrap_or_default(),
            shift,
            ctrl,
        };
        self.dispatch_key(win, input);
    }
    fn repeat_key(
        &mut self,
        c: &Connection,
        qh: &QueueHandle<Self>,
        k: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.press_key(c, qh, k, serial, event);
    }
    fn release_key(
        &mut self,
        _c: &Connection,
        _qh: &QueueHandle<Self>,
        _k: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _e: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _c: &Connection,
        _qh: &QueueHandle<Self>,
        _k: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _r: RawModifiers,
        _l: u32,
    ) {
        self.modifiers = (modifiers.shift, modifiers.ctrl);
    }
}

impl UiState {
    fn dispatch_key(&mut self, win: u64, input: KeyInput) {
        let ev = InputEvent::Key(input.clone());
        let mut queue = Vec::new();
        let mut consumed = false;
        let mut repaint = false;
        let theme = self.shell.theme();
        let now = Instant::now();
        {
            let focused = self.windows.get(&win).and_then(|w| w.focus);
            let mut ctx = Ctx {
                theme,
                fonts: &mut self.fonts,
                anims: &mut self.anims,
                shadows: &mut self.shadows,
                queue: &mut queue,
                now,
                focused,
                repaint: false,
            };
            if let Some(w) = self.windows.get_mut(&win) {
                if let Some(f) = w.focus {
                    consumed = w.root.deliver(&mut ctx, &ev, Some(f));
                }
                if !consumed {
                    consumed = w.root.deliver(&mut ctx, &ev, None);
                }
                if ctx.repaint {
                    w.needs_render = true;
                }
                repaint = ctx.repaint;
            }
        }
        let mut q = queue;
        for ev in q.drain(..) {
            self.with_shell(|shell, state| shell.ui_event(state, win, ev));
        }
        if repaint {
            self.request_render(win);
        }
        if !consumed {
            self.with_shell(|shell, state| shell.key(state, win, &input));
        }
    }

    /// Pointer dispatch. Simplification (documented): no cross-widget drag
    /// grab — a slider drag stops at its own rect; the sandbox has no pointer
    /// input to test against anyway (mock compositor: keyboard only).
    fn dispatch_pointer(&mut self, win: u64, ev: InputEvent) {
        let mut queue = Vec::new();
        let theme = self.shell.theme();
        let now = Instant::now();
        {
            let focused = self.windows.get(&win).and_then(|w| w.focus);
            let mut ctx = Ctx {
                theme,
                fonts: &mut self.fonts,
                anims: &mut self.anims,
                shadows: &mut self.shadows,
                queue: &mut queue,
                now,
                focused,
                repaint: false,
            };
            if let Some(w) = self.windows.get_mut(&win) {
                w.root.deliver(&mut ctx, &ev, None);
                if ctx.repaint {
                    w.needs_render = true;
                }
            }
            if ctx.repaint {
                self.request_render(win);
            }
        }
        let mut q = queue;
        for e in q.drain(..) {
            self.with_shell(|shell, state| shell.ui_event(state, win, e));
        }
    }

    /// Draw and attach a simple arrow cursor on pointer enter (no
    /// cursor-theme dependency; any compositor).
    fn set_cursor(&mut self) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        const CS: i32 = 24;
        if self.cursor_pool.is_none() {
            match SlotPool::new((CS * CS * 4) as usize, &self.shm) {
                Ok(p) => self.cursor_pool = Some(p),
                Err(e) => {
                    eprintln!("lme-ui: cursor pool failed: {e:?}");
                    return;
                }
            }
        }
        let Some(pool) = self.cursor_pool.as_mut() else {
            return;
        };
        let Ok((buffer, canvas)) = pool.create_buffer(CS, CS, CS * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // Classic arrow: black-bordered white triangle pointing up-left.
        let mut rgba = vec![0u8; (CS * CS * 4) as usize];
        let inside = |x: i32, y: i32, pad: f32| -> bool {
            // triangle (3,3) (3,16) (12,10), padded
            let (ax, ay) = (3.0 - pad, 3.0 - pad);
            let (bx, by) = (3.0 - pad, 16.0 + pad);
            let (cx, cy) = (12.0 + pad, 10.0);
            let d1 = (x as f32 - bx) * (ay - by) - (ax - bx) * (y as f32 - by);
            let d2 = (x as f32 - cx) * (by - cy) - (bx - cx) * (y as f32 - cy);
            let d3 = (x as f32 - ax) * (cy - ay) - (cx - ax) * (y as f32 - ay);
            let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            !(has_neg && has_pos)
        };
        for i in 0..(CS * CS) {
            let (x, y) = (i % CS, i / CS);
            let (r, g, b, a) = if inside(x, y, -1.2) {
                (255, 255, 255, 255)
            } else if inside(x, y, 0.6) {
                (0, 0, 0, 255)
            } else {
                (0, 0, 0, 0)
            };
            let base = (i * 4) as usize;
            rgba[base] = r;
            rgba[base + 1] = g;
            rgba[base + 2] = b;
            rgba[base + 3] = a;
        }
        for (d, s) in canvas
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(rgba.as_chunks::<4>().0.iter())
        {
            *d = [s[2], s[1], s[0], s[3]];
        }
        let surface = self.compositor.create_surface(&self.qh);
        pointer.set_cursor(self.pointer_serial, Some(&surface), 3, 3);
        if let Err(e) = buffer.attach_to(&surface) {
            eprintln!("lme-ui: cursor attach failed: {e:?}");
            return;
        }
        surface.commit();
    }
}

impl PointerHandler for UiState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let win = self.find_window_by_surface(&event.surface);
            match &event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer_serial = *serial;
                    self.pointer_window = win;
                    if let Some(w) = win {
                        self.set_cursor();
                        self.request_render(w);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    let prev = self.pointer_window.take();
                    if let Some(w) = prev {
                        self.dispatch_pointer(w, InputEvent::Leave);
                    }
                }
                PointerEventKind::Motion { .. } => {
                    if let Some(w) = self.pointer_window {
                        self.dispatch_pointer(
                            w,
                            InputEvent::Move(crate::geom::Point::new(
                                event.position.0 as f32,
                                event.position.1 as f32,
                            )),
                        );
                    }
                }
                PointerEventKind::Press { .. } => {
                    if let Some(w) = self.pointer_window {
                        self.dispatch_pointer(
                            w,
                            InputEvent::Down(crate::geom::Point::new(
                                event.position.0 as f32,
                                event.position.1 as f32,
                            )),
                        );
                    }
                }
                PointerEventKind::Release { .. } => {
                    if let Some(w) = self.pointer_window {
                        self.dispatch_pointer(
                            w,
                            InputEvent::Up(crate::geom::Point::new(
                                event.position.0 as f32,
                                event.position.1 as f32,
                            )),
                        );
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    if let Some(w) = self.pointer_window {
                        self.dispatch_pointer(
                            w,
                            InputEvent::Scroll(
                                crate::geom::Point::new(
                                    event.position.0 as f32,
                                    event.position.1 as f32,
                                ),
                                vertical.absolute as f32 / 120.0,
                            ),
                        );
                    }
                }
            }
        }
    }
}

impl ShmHandler for UiState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(UiState);

impl ProvidesRegistryState for UiState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(UiState);
