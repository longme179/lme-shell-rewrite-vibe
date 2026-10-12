// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway Phase 2 stack-spike tool (step 2.0). See native/spike/README.md.
//! Headless mock Wayland compositor for stack measurements.
//!
//! Implements just enough wire protocol for the spike clients: wl_compositor,
//! wl_shm, wl_output, wl_seat (keyboard + pointer), zwlr_layer_shell_v1 (v5).
//!
//! Modes:
//!   idle     - frame callbacks fire immediately on commit (client draws once, sleeps)
//!   animate  - frame callbacks paced on a vsync grid at --fps (client redraw loop
//!              => CPU/RSS under load); SUMMARY reports delivered fps, late and
//!              dropped frames
//!   keys     - like idle plus a scripted key sequence (p o w e r BackSpace Down Up Esc)
//!
//! Input injection (Step 0a):
//!   --key-events "esc@2500,enter@3000"           named keys at ms offsets
//!   --pointer-events "motion:640,400@500;click@600;scroll:0,120@700;leave@800"
//!     (events separated by ';', x/y pairs by ',')
//!     motion:x,y   absolute-output motion (drives enter/leave by hit-test)
//!     click[:l|m|r]  press+release of a button at the current position
//!     press[:l|m|r] / release[:l|m|r]
//!     scroll:h,v   wheel notches in value120 units (120 = one notch). Like a
//!                  real compositor each notch is ALSO delivered as plain
//!                  axis pixels (53 px) — clients must count notches from
//!                  value120, not from absolute pixels.
//!     scrollpx:h,v touchpad-style pixel deltas: plain axis only (no value120)
//!     scrollstop   axis_stop on both axes (end of a continuous gesture)
//!     leave        move the pointer off-screen (forces leave)
//!
//! Pointer hit-testing models layer-shell geometry: anchors + margins +
//! committed buffer size place each layer surface on the output; stacking is
//! (layer rank, map order) — Overlay above Top, later above earlier.
//!
//! Also captures the Nth committed buffer to a PNG (benchmark evidence), or a
//! timed snapshot of the topmost surface with --capture-at MS --capture-out PATH.
//! Logs timestamped lines for the bench script to parse: BIND0, MAPPED, CAPTURED,
//! PTR, SUMMARY{...}

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Seek as _, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::time::{Duration, Instant};

use wayland_protocols_wlr::layer_shell::v1::server::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
// the generated types live inside the snake_case modules:
use std::sync::Arc;
use wayland_server::backend::{ClientData, ObjectId};
use wayland_server::ListeningSocket;
use wayland_server::{
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region,
        wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, Resource, WEnum,
};
use zwlr_layer_shell_v1::ZwlrLayerShellV1 as LayerShellIface;
use zwlr_layer_surface_v1::ZwlrLayerSurfaceV1 as LayerSurfaceIface;

const KEYMAP: &str = include_str!("../assets/minimal-keymap.xkb");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Animate,
    Keys,
}

impl Mode {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Mode::Idle),
            "animate" => Some(Mode::Animate),
            "keys" => Some(Mode::Keys),
            _ => None,
        }
    }
}

struct PoolRec {
    fd: fs::File,
    _size: i32,
}

#[derive(Clone)]
struct BufferRec {
    pool: ObjectId,
    offset: i32,
    w: i32,
    h: i32,
    stride: i32,
}

struct SurfaceRec {
    layer: Option<ObjectId>,
    pending_buffer: Option<ObjectId>,
    current_buffer: Option<ObjectId>,
    frame_callbacks: Vec<wl_callback::WlCallback>,
    configured: bool,
    mapped: bool,
    commits: u64,
    /// Monotonic map order (stacking within a layer).
    map_seq: u64,
}

struct LayerRec {
    surface: ObjectId,
    namespace: String,
    width: i32,
    height: i32,
    acked: bool,
    interactivity: Option<String>,
    anchors_top: bool,
    anchors_bottom: bool,
    anchors_left: bool,
    anchors_right: bool,
    /// (top, right, bottom, left) margins in px.
    margins: (i32, i32, i32, i32),
    /// Stacking rank: Background=0 Bottom=1 Top=2 Overlay=3.
    layer_rank: u8,
}

/// Scripted pointer event.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PtrEv {
    Motion(f64, f64),
    Press(u32),
    Release(u32),
    Click(u32),
    /// Wheel notches: (horizontal, vertical) in value120 units. Delivered as
    /// axis_value120(N*120) PLUS plain axis(N*53 px) so the pixel path stays
    /// realistic (real compositors send both; the px value is NOT 120/notch).
    Scroll(i32, i32),
    /// Touchpad-style (horizontal, vertical) pixel deltas: plain axis only.
    ScrollPx(f64, f64),
    /// axis_stop on both axes (finger lifted off the touchpad).
    ScrollStop,
    Leave,
}

/// Pixels a real compositor co-delivers per wheel notch alongside value120.
const WHEEL_PX_PER_NOTCH: f64 = 53.0;

/// Simple screen-space rectangle in output logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl ScreenRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

struct State {
    mode: Mode,
    start: Instant,
    duration: Duration,
    frame_interval: Duration,
    next_serial: u32,
    surfaces: HashMap<ObjectId, SurfaceRec>,
    layers: HashMap<ObjectId, LayerRec>,
    pools: HashMap<ObjectId, PoolRec>,
    buffers: HashMap<ObjectId, BufferRec>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    focus_surface: Option<ObjectId>,
    /// (callback, due Instant, surface) — pending paced frame callbacks.
    pending_frames: Vec<(wl_callback::WlCallback, Instant, ObjectId)>,
    key_script: VecDeque<(Duration, u32, wl_keyboard::KeyState)>,
    keys_done_at: Option<Instant>,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_script: VecDeque<(Duration, PtrEv)>,
    /// Pointer position on the output; None = off-screen (no focus).
    pointer_pos: Option<(f64, f64)>,
    pointer_focus: Option<ObjectId>,
    pointer_events_sent: u64,
    /// End the run this long after the last scripted pointer event.
    pointer_done_at: Option<Instant>,
    capture_commit: Option<u64>,
    capture_path: Option<String>,
    /// Timed capture: (due Instant, path) — snapshot the topmost layer surface.
    capture_at: Option<(Instant, String)>,
    keymap_file: Option<fs::File>,
    first_bind_at: Option<Instant>,
    mapped_at: Option<Instant>,
    frames_fired: u64,
    /// Frame callbacks delivered more than half an interval late.
    late_frames: u64,
    /// Commits that replaced a buffer while that surface still had an
    /// unfired frame callback (over-run; the previous frame never displayed).
    dropped_frames: u64,
    commits_total: u64,
    map_seq_next: u64,
    output_w: i32,
    output_h: i32,
}

impl State {
    fn slog(&self, tag: &str, msg: &str) {
        println!(
            "[{:>9}ms] [{}] {}",
            self.start.elapsed().as_millis(),
            tag,
            msg
        );
        let _ = std::io::stdout().flush();
    }

    fn serial(&mut self) -> u32 {
        self.next_serial += 1;
        self.next_serial
    }

    fn tick(&mut self, dh: &DisplayHandle) -> bool {
        // returns false when the run should end
        let elapsed = self.start.elapsed();

        // fire due frame callbacks (late = past due + half interval)
        let time = elapsed.as_millis() as u32;
        let half = self.frame_interval / 2;
        let mut i = 0;
        while i < self.pending_frames.len() {
            if self.pending_frames[i].1 <= Instant::now() {
                let (cb, due, _) = self.pending_frames.remove(i);
                if Instant::now() > due + half {
                    self.late_frames += 1;
                }
                cb.done(time);
                self.frames_fired += 1;
            } else {
                i += 1;
            }
        }

        // scripted keys
        while let Some((t, key, state)) = self.key_script.front() {
            if elapsed < *t {
                break;
            }
            let (t, key, state) = (*t, *key, *state);
            self.key_script.pop_front();
            let kb = self.keyboard.clone();
            if let Some(kb) = kb {
                let serial = self.serial();
                kb.key(serial, t.as_millis() as u32, key, state);
                self.slog("KEY", &format!("key={} state={:?}", key, state));
            } else {
                self.slog("KEY", "no keyboard bound; dropping key");
            }
        }

        // scripted pointer events
        while let Some((t, ev)) = self.pointer_script.front() {
            if elapsed < *t {
                break;
            }
            let (t, ev) = (*t, *ev);
            self.pointer_script.pop_front();
            self.run_pointer_event(dh, ev, t.as_millis() as u32);
        }

        // timed capture (topmost layer surface snapshot)
        if let Some((due, path)) = self.capture_at.clone() {
            if Instant::now() >= due {
                let path = self.capture_topmost(&path);
                match path {
                    Some(p) => self.slog("CAPTURED", &p),
                    None => self.slog("CAPFAIL", "timed capture: no mapped layer surface"),
                }
                self.capture_at = None;
            }
        }

        // keys-mode early end: script scheduled, done, grace period passed
        if let Some(done) = self.keys_done_at {
            if Instant::now() > done {
                return true;
            }
        }
        // pointer-mode early end (same policy as keys)
        if let Some(done) = self.pointer_done_at {
            if Instant::now() > done {
                return true;
            }
        }

        elapsed >= self.duration
    }

    /// On-screen rect of a mapped layer surface (anchors + margins + size).
    /// Unanchored axes default to the top/left edge, per layer-shell.
    fn surface_rect(&self, surf_id: &ObjectId) -> Option<ScreenRect> {
        let rec = self.surfaces.get(surf_id)?;
        if !rec.mapped {
            return None;
        }
        let layer = self.layers.get(rec.layer.as_ref()?)?;
        let (bw, bh) = rec
            .current_buffer
            .as_ref()
            .and_then(|b| self.buffers.get(b))
            .map(|b| (b.w as f64, b.h as f64))
            .unwrap_or((layer.width.max(1) as f64, layer.height.max(1) as f64));
        let (mt, mr, mb, ml) = (
            layer.margins.0 as f64,
            layer.margins.1 as f64,
            layer.margins.2 as f64,
            layer.margins.3 as f64,
        );
        let out_w = self.output_w as f64;
        let out_h = self.output_h as f64;
        let w = if layer.anchors_left && layer.anchors_right {
            out_w - ml - mr
        } else {
            bw
        };
        let h = if layer.anchors_top && layer.anchors_bottom {
            out_h - mt - mb
        } else {
            bh
        };
        let x = if layer.anchors_left || !layer.anchors_right {
            ml
        } else {
            out_w - mr - w
        };
        let y = if layer.anchors_top || !layer.anchors_bottom {
            mt
        } else {
            out_h - mb - h
        };
        Some(ScreenRect { x, y, w, h })
    }

    /// Topmost mapped layer surface containing (x, y): higher layer rank
    /// first, then later map order.
    fn hit_test(&self, x: f64, y: f64) -> Option<ObjectId> {
        let mut best: Option<(u8, u64, ObjectId)> = None;
        for (id, rec) in &self.surfaces {
            if !rec.mapped {
                continue;
            }
            let Some(layer_id) = &rec.layer else {
                continue;
            };
            let Some(layer) = self.layers.get(layer_id) else {
                continue;
            };
            let Some(rect) = self.surface_rect(id) else {
                continue;
            };
            if !rect.contains(x, y) {
                continue;
            }
            let key = (layer.layer_rank, rec.map_seq);
            if best.as_ref().is_none_or(|b| key > (b.0, b.1)) {
                best = Some((layer.layer_rank, rec.map_seq, id.clone()));
            }
        }
        best.map(|(_, _, id)| id)
    }

    /// Topmost mapped layer surface (for timed captures).
    fn topmost_layer_surface(&self) -> Option<ObjectId> {
        let mut best: Option<(u8, u64, ObjectId)> = None;
        for (id, rec) in &self.surfaces {
            if !rec.mapped {
                continue;
            }
            let Some(layer_id) = &rec.layer else {
                continue;
            };
            let Some(layer) = self.layers.get(layer_id) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|b| (layer.layer_rank, rec.map_seq) > (b.0, b.1))
            {
                best = Some((layer.layer_rank, rec.map_seq, id.clone()));
            }
        }
        best.map(|(_, _, id)| id)
    }

    fn capture_topmost(&self, path: &str) -> Option<String> {
        let id = self.topmost_layer_surface()?;
        let buf = self
            .surfaces
            .get(&id)
            .and_then(|r| r.current_buffer.clone())
            .and_then(|b| self.buffers.get(&b).cloned())?;
        self.capture_to(&buf, path)
    }

    /// Update pointer focus from pointer_pos; enter/leave as needed, then
    /// deliver a motion event when the pointer is over a surface.
    fn pointer_update(&mut self, dh: &DisplayHandle, time: u32, send_motion: bool) {
        let new_focus = self.pointer_pos.and_then(|(x, y)| self.hit_test(x, y));
        if new_focus != self.pointer_focus {
            let ptr = self.pointer.clone();
            if let Some(ptr) = ptr {
                if let Some(old) = self.pointer_focus.take() {
                    if let Ok(sf) = wl_surface::WlSurface::from_id(dh, old) {
                        ptr.leave(self.serial(), &sf);
                    }
                }
                if let Some(new) = &new_focus {
                    if let Ok(sf) = wl_surface::WlSurface::from_id(dh, new.clone()) {
                        let (sx, sy) = self
                            .surface_rect(new)
                            .and_then(|r| self.pointer_pos.map(|(x, y)| (x - r.x, y - r.y)))
                            .unwrap_or((0.0, 0.0));
                        ptr.enter(self.serial(), &sf, sx, sy);
                    }
                }
                ptr.frame();
                self.pointer_events_sent += 1;
            }
            self.pointer_focus = new_focus;
            self.slog("PTR", &format!("focus -> {}", self.focus_desc()));
        }
        if send_motion {
            if let (Some((x, y)), Some(focus), Some(ptr)) = (
                self.pointer_pos,
                self.pointer_focus.clone(),
                self.pointer.clone(),
            ) {
                if let Some(rect) = self.surface_rect(&focus) {
                    ptr.motion(time, x - rect.x, y - rect.y);
                    ptr.frame();
                    self.pointer_events_sent += 1;
                    self.slog("PTR", &format!("motion {x:.0},{y:.0}"));
                }
            }
        }
    }

    fn focus_desc(&self) -> String {
        self.pointer_focus
            .as_ref()
            .and_then(|id| self.surfaces.get(id))
            .and_then(|r| r.layer.as_ref())
            .and_then(|lid| self.layers.get(lid))
            .map(|l| format!("layer surface {} (rank {})", l.namespace, l.layer_rank))
            .unwrap_or_else(|| "none".to_string())
    }

    fn run_pointer_event(&mut self, dh: &DisplayHandle, ev: PtrEv, time: u32) {
        match ev {
            PtrEv::Motion(x, y) => {
                self.pointer_pos = Some((x, y));
                self.pointer_update(dh, time, true);
            }
            PtrEv::Leave => {
                self.pointer_pos = None;
                self.pointer_update(dh, time, false);
            }
            PtrEv::Click(btn) => {
                self.run_pointer_event(dh, PtrEv::Press(btn), time);
                self.run_pointer_event(dh, PtrEv::Release(btn), time + 40);
            }
            PtrEv::Press(btn) => {
                if let (Some(focus), Some(ptr)) = (self.pointer_focus.clone(), self.pointer.clone())
                {
                    if let Some(rect) = self.surface_rect(&focus) {
                        if let Some((x, y)) = self.pointer_pos {
                            ptr.motion(time, x - rect.x, y - rect.y);
                        }
                    }
                    ptr.button(self.serial(), time, btn, wl_pointer::ButtonState::Pressed);
                    ptr.frame();
                    self.pointer_events_sent += 1;
                    self.slog("PTR", &format!("press btn={btn:#x}"));
                } else {
                    self.slog("PTR", "press with no focus; dropped");
                }
            }
            PtrEv::Release(btn) => {
                if let (Some(focus), Some(ptr)) = (self.pointer_focus.clone(), self.pointer.clone())
                {
                    if let Some(rect) = self.surface_rect(&focus) {
                        if let Some((x, y)) = self.pointer_pos {
                            ptr.motion(time, x - rect.x, y - rect.y);
                        }
                    }
                    ptr.button(self.serial(), time, btn, wl_pointer::ButtonState::Released);
                    ptr.frame();
                    self.pointer_events_sent += 1;
                    self.slog("PTR", &format!("release btn={btn:#x}"));
                } else {
                    self.slog("PTR", "release with no focus; dropped");
                }
            }
            PtrEv::Scroll(h, v) => {
                if let (Some(_focus), Some(ptr)) =
                    (self.pointer_focus.clone(), self.pointer.clone())
                {
                    if h != 0 {
                        ptr.axis(
                            time,
                            wl_pointer::Axis::HorizontalScroll,
                            h as f64 * WHEEL_PX_PER_NOTCH,
                        );
                        ptr.axis_value120(wl_pointer::Axis::HorizontalScroll, h * 120);
                    }
                    if v != 0 {
                        ptr.axis(
                            time,
                            wl_pointer::Axis::VerticalScroll,
                            v as f64 * WHEEL_PX_PER_NOTCH,
                        );
                        ptr.axis_value120(wl_pointer::Axis::VerticalScroll, v * 120);
                    }
                    ptr.axis_source(wl_pointer::AxisSource::Wheel);
                    ptr.frame();
                    self.pointer_events_sent += 1;
                    self.slog("PTR", &format!("scroll(wheel) h={h} v={v}"));
                } else {
                    self.slog("PTR", "scroll with no focus; dropped");
                }
            }
            PtrEv::ScrollPx(h, v) => {
                if let (Some(_focus), Some(ptr)) =
                    (self.pointer_focus.clone(), self.pointer.clone())
                {
                    if h != 0.0 {
                        ptr.axis(time, wl_pointer::Axis::HorizontalScroll, h);
                    }
                    if v != 0.0 {
                        ptr.axis(time, wl_pointer::Axis::VerticalScroll, v);
                    }
                    ptr.axis_source(wl_pointer::AxisSource::Finger);
                    ptr.frame();
                    self.pointer_events_sent += 1;
                    self.slog("PTR", &format!("scroll(px) h={h:.1} v={v:.1}"));
                } else {
                    self.slog("PTR", "scrollpx with no focus; dropped");
                }
            }
            PtrEv::ScrollStop => {
                if let (Some(_focus), Some(ptr)) =
                    (self.pointer_focus.clone(), self.pointer.clone())
                {
                    ptr.axis_stop(time, wl_pointer::Axis::HorizontalScroll);
                    ptr.axis_stop(time, wl_pointer::Axis::VerticalScroll);
                    ptr.axis_source(wl_pointer::AxisSource::Finger);
                    ptr.frame();
                    self.pointer_events_sent += 1;
                    self.slog("PTR", "scroll stop");
                } else {
                    self.slog("PTR", "scrollstop with no focus; dropped");
                }
            }
        }
    }

    /// Fire callbacks for a commit according to mode pacing. Animate paces on
    /// a vsync grid: every callback is due at the NEXT grid tick after the
    /// commit — the scanout clock, not commit time.
    fn queue_frames(&mut self, surf_id: &ObjectId) {
        let Some(rec) = self.surfaces.get_mut(surf_id) else {
            return;
        };
        // dropped-frame accounting: this surface still has an unfired callback
        let overran = self.pending_frames.iter().any(|(_, _, s)| s == surf_id);
        if overran {
            self.dropped_frames += 1;
        }
        let cbs = std::mem::take(&mut rec.frame_callbacks);
        for cb in cbs {
            let ready = match self.mode {
                Mode::Animate => {
                    let elapsed = self.start.elapsed().as_nanos() as f64;
                    let interval = self.frame_interval.as_nanos().max(1) as f64;
                    let k = (elapsed / interval).ceil().max(1.0) as u32;
                    self.start + self.frame_interval * k
                }
                _ => Instant::now(),
            };
            self.pending_frames.push((cb, ready, surf_id.clone()));
        }
    }

    fn capture(&self, buf: &BufferRec) -> Option<String> {
        let path = self.capture_path.clone()?;
        self.capture_to(buf, &path)
    }

    /// Encode a buffer's current contents to a PNG at `path`.
    fn capture_to(&self, buf: &BufferRec, path: &str) -> Option<String> {
        use std::os::unix::fs::FileExt;
        let pool = self.pools.get(&buf.pool)?;
        let len = buf.h.checked_mul(buf.stride)? as usize;
        let mut raw = vec![0u8; len];
        pool.fd.read_exact_at(&mut raw, buf.offset as u64).ok()?;
        let rgba = bgra_rows_to_rgba(&raw, buf.w, buf.h, buf.stride);
        let file = fs::File::create(path).ok()?;
        let mut enc = png::Encoder::new(file, buf.w as u32, buf.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&rgba).ok()?;
        Some(path.to_string())
    }

    fn schedule_keys(&mut self) {
        if self.mode != Mode::Keys || !self.key_script.is_empty() {
            return;
        }
        // wire codes: p=25 o=24 w=17 e=18 r=19 BackSpace=14 Down=108 Up=103 Esc=1
        let seq: &[u32] = &[25, 24, 17, 18, 19, 14, 108, 103, 1];
        let mut t = 300u64;
        for k in seq {
            self.key_script.push_back((
                Duration::from_millis(t),
                *k,
                wl_keyboard::KeyState::Pressed,
            ));
            self.key_script.push_back((
                Duration::from_millis(t + 45),
                *k,
                wl_keyboard::KeyState::Released,
            ));
            t += 90;
        }
        // after the last key (+ release) allow a grace period, then end the run
        self.keys_done_at = Some(Instant::now() + Duration::from_millis(t + 45 + 3_000));
    }
}

/// Convert wl_shm Argb8888 (BGRA byte order on LE) rows into tight RGBA.
/// Pure function, unit-tested below.
fn bgra_rows_to_rgba(raw: &[u8], w: i32, h: i32, stride: i32) -> Vec<u8> {
    let (w, h, stride) = (w as usize, h as usize, stride as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in raw.chunks_exact(stride).take(h) {
        for px in row.as_chunks::<4>().0.iter().take(w) {
            out.push(px[2]); // R
            out.push(px[1]); // G
            out.push(px[0]); // B
            out.push(px[3]); // A
        }
    }
    out
}

// ---- wl_compositor ----

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for State {
    fn bind(
        state: &mut State,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: wayland_server::New<wl_compositor::WlCompositor>,
        _data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        data_init.init(resource, ());
        if state.first_bind_at.is_none() {
            state.first_bind_at = Some(Instant::now());
            state.slog("BIND0", "client bound first global");
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        if let wl_compositor::Request::CreateSurface { id } = request {
            let surface = data_init.init(id, ());
            state.surfaces.insert(
                surface.id().clone(),
                SurfaceRec {
                    layer: None,
                    pending_buffer: None,
                    current_buffer: None,
                    frame_callbacks: Vec::new(),
                    configured: false,
                    mapped: false,
                    commits: 0,
                    map_seq: 0,
                },
            );
        }
    }
}

// ---- wl_surface ----

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        resource: &wl_surface::WlSurface,
        request: wl_surface::Request,
        _data: &(),
        dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            wl_surface::Request::Attach { buffer, .. } => {
                if let Some(rec) = state.surfaces.get_mut(&resource.id()) {
                    rec.pending_buffer = buffer.map(|b| b.id().clone());
                }
            }
            wl_surface::Request::Frame { callback } => {
                let cb = _data_init.init(callback, ());
                if let Some(rec) = state.surfaces.get_mut(&resource.id()) {
                    rec.frame_callbacks.push(cb);
                }
            }
            wl_surface::Request::Commit => {
                let Some(rec) = state.surfaces.get_mut(&resource.id()) else {
                    return;
                };
                rec.commits += 1;
                state.commits_total += 1;
                let commit_no = rec.commits;
                let layer_id = rec.layer.clone();
                let configured = rec.configured;
                let mapped = rec.mapped;
                let buffer_id = rec.pending_buffer.clone();

                if !configured {
                    if let Some(lid) = &layer_id {
                        let (w, h) = {
                            let Some(layer) = state.layers.get_mut(lid) else {
                                return;
                            };
                            layer.acked = false;
                            // Per-axis: a 0 dimension derives from the
                            // output when that axis is anchored on both
                            // edges (layer-shell convention), else the
                            // requested size or full output as fallback.
                            let w = if layer.width > 0 {
                                layer.width
                            } else {
                                state.output_w
                            };
                            let h = if layer.height > 0 {
                                layer.height
                            } else {
                                state.output_h
                            };
                            (w, h)
                        };
                        if let Ok(ls) = LayerSurfaceIface::from_id(dh, lid.clone()) {
                            // NOTE: event arg order per XML is (serial, width, height)
                            let serial = state.serial();
                            ls.configure(serial, w as u32, h as u32);
                        }
                        if let Some(rec2) = state.surfaces.get_mut(&resource.id()) {
                            rec2.configured = true;
                        }
                        state.slog("CFG", &format!("configure {}x{} sent", w, h));
                    }
                    return;
                }

                // apply pending buffer
                if let Some(rec2) = state.surfaces.get_mut(&resource.id()) {
                    rec2.current_buffer = buffer_id.clone();
                }
                let buf_rec = buffer_id
                    .as_ref()
                    .and_then(|b| state.buffers.get(b).cloned());

                if !mapped {
                    if let Some(rec2) = state.surfaces.get_mut(&resource.id()) {
                        rec2.mapped = true;
                        state.map_seq_next += 1;
                        rec2.map_seq = state.map_seq_next;
                    }
                    state.mapped_at = Some(Instant::now());
                    let since_bind = state
                        .first_bind_at
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);
                    state.slog(
                        "MAPPED",
                        &format!(
                            "first frame commit #{commit_no} (bind->map {}ms)",
                            since_bind
                        ),
                    );
                    state.schedule_keys();
                    // Keyboard focus policy: the first mapped surface gets
                    // focus; afterwards, any surface whose layer requested
                    // "exclusive" interactivity takes focus when it maps,
                    // and releases it when destroyed (layer-shell semantics
                    // approximated for shell testing).
                    let layer_id = state
                        .surfaces
                        .get(&resource.id())
                        .and_then(|r| r.layer.clone());
                    let wants_exclusive = layer_id
                        .as_ref()
                        .and_then(|lid| state.layers.get(lid))
                        .map(|l| l.interactivity.as_deref() == Some("exclusive"))
                        .unwrap_or(false);
                    let take_focus = state.focus_surface.is_none() || wants_exclusive;
                    if take_focus {
                        let kb = state.keyboard.clone();
                        if let Some(kb) = kb {
                            if let Some(old) = state.focus_surface.clone() {
                                if old != resource.id() {
                                    if let Ok(sf) = wl_surface::WlSurface::from_id(dh, old) {
                                        let serial = state.serial();
                                        kb.leave(serial, &sf);
                                    }
                                }
                            }
                            let serial = state.serial();
                            kb.enter(serial, resource, Vec::new());
                            state.focus_surface = Some(resource.id().clone());
                        }
                    }
                }

                // release the buffer back to the client
                if let Some(bid) = &buffer_id {
                    if let Ok(buf) = wl_buffer::WlBuffer::from_id(dh, bid.clone()) {
                        buf.release();
                    }
                }

                // capture?
                if let (Some(n), Some(_)) = (state.capture_commit, &buf_rec) {
                    if commit_no == n {
                        let path = state.capture(buf_rec.as_ref().expect("capture buf"));
                        if let Some(p) = path {
                            state.slog("CAPTURED", &p);
                        } else {
                            state.slog("CAPFAIL", "capture failed");
                        }
                    }
                }

                // fire frame callbacks (paced per mode)
                state.queue_frames(&resource.id());
            }
            wl_surface::Request::Destroy => {
                if state.pointer_focus.as_ref() == Some(&resource.id()) {
                    // surface going away without layer destroy: drop focus
                    if let Some(ptr) = state.pointer.clone() {
                        if let Ok(sf) = wl_surface::WlSurface::from_id(dh, resource.id().clone()) {
                            ptr.leave(state.serial(), &sf);
                            ptr.frame();
                        }
                    }
                    state.pointer_focus = None;
                }
                state.surfaces.remove(&resource.id());
            }
            _ => {}
        }
    }
}

// ---- wl_region (accepted, ignored) ----

impl Dispatch<wl_region::WlRegion, ()> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        _resource: &wl_region::WlRegion,
        _request: wl_region::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
    }
}

// ---- wl_shm / pool / buffer ----

impl GlobalDispatch<wl_shm::WlShm, ()> for State {
    fn bind(
        _state: &mut State,
        dh: &DisplayHandle,
        _client: &Client,
        resource: wayland_server::New<wl_shm::WlShm>,
        _data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        let shm = data_init.init(resource, ());
        shm.format(wl_shm::Format::Argb8888);
        shm.format(wl_shm::Format::Xrgb8888);
        let _ = dh;
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &wl_shm::WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        if let wl_shm::Request::CreatePool { id, fd, size } = request {
            let pool = data_init.init(id, ());
            state.pools.insert(
                pool.id().clone(),
                PoolRec {
                    fd: fd_into_file(fd),
                    _size: size,
                },
            );
        }
    }
}

fn fd_into_file(fd: OwnedFd) -> fs::File {
    fd.into()
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        resource: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                let buf = data_init.init(id, ());
                let _fmt = match format {
                    WEnum::Value(f) => f as u32,
                    WEnum::Unknown(u) => u,
                };
                state.buffers.insert(
                    buf.id().clone(),
                    BufferRec {
                        pool: resource.id().clone(),
                        offset,
                        w: width,
                        h: height,
                        stride,
                    },
                );
            }
            wl_shm_pool::Request::Destroy => {
                state.pools.remove(&resource.id());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        resource: &wl_buffer::WlBuffer,
        _request: wl_buffer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        state.buffers.remove(&resource.id());
    }
}

// ---- wl_output ----

impl GlobalDispatch<wl_output::WlOutput, ()> for State {
    fn bind(
        state: &mut State,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: wayland_server::New<wl_output::WlOutput>,
        _data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        let out = data_init.init(resource, ());
        out.geometry(
            0,
            0,
            344,
            194,
            wl_output::Subpixel::Unknown,
            "mock".to_string(),
            "lme-mock".to_string(),
            wl_output::Transform::Normal,
        );
        out.mode(
            wl_output::Mode::Current,
            state.output_w,
            state.output_h,
            119_988,
        );
        out.scale(1);
        out.done();
    }
}

// ---- wl_seat / keyboard / pointer ----

impl GlobalDispatch<wl_seat::WlSeat, ()> for State {
    fn bind(
        _state: &mut State,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: wayland_server::New<wl_seat::WlSeat>,
        _data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        let seat = data_init.init(resource, ());
        seat.capabilities(wl_seat::Capability::Keyboard | wl_seat::Capability::Pointer);
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &wl_seat::WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            wl_seat::Request::GetKeyboard { id } => {
                let kb = data_init.init(id, ());
                // send keymap immediately (before enter)
                let mut tmp = tempfile::tempfile().expect("tempfile");
                let keymap = KEYMAP.as_bytes().to_vec();
                tmp.write_all(&keymap).expect("write keymap");
                tmp.seek(std::io::SeekFrom::Start(0)).ok();
                let fd = tmp.as_fd();
                kb.keymap(wl_keyboard::KeymapFormat::XkbV1, fd, keymap.len() as u32);
                // keep the file alive for the lifetime of the display
                state.keymap_file = Some(tmp);
                state.keyboard = Some(kb.clone());
                // if the surface is already mapped, enter now
                if let Some(focus) = state.focus_surface.clone() {
                    if let Ok(sf) = wl_surface::WlSurface::from_id(_dh, focus) {
                        let serial = state.serial();
                        kb.enter(serial, &sf, Vec::new());
                    }
                }
            }
            wl_seat::Request::GetPointer { id } => {
                let ptr = data_init.init(id, ());
                state.pointer = Some(ptr);
                state.slog("PTR", "client bound wl_pointer");
            }
            wl_seat::Request::Release => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &wl_pointer::WlPointer,
        request: wl_pointer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            wl_pointer::Request::SetCursor {
                surface,
                hotspot_x,
                hotspot_y,
                serial: _,
            } => {
                // cursor surfaces are plain wl_surfaces (no layer) — excluded
                // from hit-testing; nothing to do beyond liveness logging.
                state.slog(
                    "PTR-CURSOR",
                    &format!(
                        "set_cursor surface={} hotspot {hotspot_x},{hotspot_y}",
                        surface.is_some()
                    ),
                );
            }
            wl_pointer::Request::Release => {
                state.pointer = None;
                state.pointer_focus = None;
            }
            _ => {}
        }
    }
}

// ---- zwlr_layer_shell_v1 ----

impl GlobalDispatch<LayerShellIface, ()> for State {
    fn bind(
        _state: &mut State,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: wayland_server::New<LayerShellIface>,
        _data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<LayerShellIface, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &LayerShellIface,
        request: zwlr_layer_shell_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        if let zwlr_layer_shell_v1::Request::GetLayerSurface {
            id,
            surface,
            output: _,
            layer,
            namespace,
        } = request
        {
            let ls = data_init.init(id, ());
            let layer_rank = match layer {
                WEnum::Value(zwlr_layer_shell_v1::Layer::Background) => 0,
                WEnum::Value(zwlr_layer_shell_v1::Layer::Bottom) => 1,
                WEnum::Value(zwlr_layer_shell_v1::Layer::Top) => 2,
                WEnum::Value(zwlr_layer_shell_v1::Layer::Overlay) => 3,
                WEnum::Unknown(u) => {
                    state.slog("WARN", &format!("unknown layer enum {u}"));
                    2
                }
                _ => 2,
            };
            state.layers.insert(
                ls.id().clone(),
                LayerRec {
                    surface: surface.id().clone(),
                    namespace: namespace.to_string(),
                    width: 0,
                    height: 0,
                    acked: false,
                    interactivity: None,
                    anchors_top: false,
                    anchors_bottom: false,
                    anchors_left: false,
                    anchors_right: false,
                    margins: (0, 0, 0, 0),
                    layer_rank,
                },
            );
            if let Some(rec) = state.surfaces.get_mut(&surface.id()) {
                rec.layer = Some(ls.id().clone());
            }
            state.slog("LAYER", &format!("layer surface created ({})", namespace));
        }
    }
}

impl Dispatch<LayerSurfaceIface, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        resource: &LayerSurfaceIface,
        request: zwlr_layer_surface_v1::Request,
        _data: &(),
        dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwlr_layer_surface_v1::Request::SetSize { width, height } => {
                state.slog("SETSIZE", &format!("{}x{}", width, height));
                if let Some(layer) = state.layers.get_mut(&resource.id()) {
                    layer.width = width as i32;
                    layer.height = height as i32;
                }
            }
            zwlr_layer_surface_v1::Request::SetAnchor { anchor } => {
                if let Some(layer) = state.layers.get_mut(&resource.id()) {
                    use zwlr_layer_surface_v1::Anchor as A;
                    if let wayland_server::WEnum::Value(bits) = anchor {
                        layer.anchors_top = bits.contains(A::Top);
                        layer.anchors_bottom = bits.contains(A::Bottom);
                        layer.anchors_left = bits.contains(A::Left);
                        layer.anchors_right = bits.contains(A::Right);
                    }
                }
            }
            zwlr_layer_surface_v1::Request::SetMargin {
                top,
                right,
                bottom,
                left,
            } => {
                if let Some(layer) = state.layers.get_mut(&resource.id()) {
                    layer.margins = (top, right, bottom, left);
                }
            }
            zwlr_layer_surface_v1::Request::SetKeyboardInteractivity {
                keyboard_interactivity: interactivity,
            } => {
                let desc = match interactivity {
                    WEnum::Value(zwlr_layer_surface_v1::KeyboardInteractivity::None) => "none",
                    WEnum::Value(zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive) => {
                        "exclusive"
                    }
                    WEnum::Value(zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand) => {
                        "on_demand"
                    }
                    WEnum::Unknown(u) => {
                        state.slog("WARN", &format!("unknown interactivity {}", u));
                        "unknown"
                    }
                    _ => "other",
                };
                if let Some(layer) = state.layers.get_mut(&resource.id()) {
                    layer.interactivity = Some(desc.to_string());
                }
                state.slog("KBD-INT", desc);
            }
            zwlr_layer_surface_v1::Request::AckConfigure { serial } => {
                if let Some(layer) = state.layers.get_mut(&resource.id()) {
                    layer.acked = true;
                }
                let _ = serial;
            }
            zwlr_layer_surface_v1::Request::Destroy => {
                if let Some(layer) = state.layers.remove(&resource.id()) {
                    state.slog("LAYER", &format!("destroyed ({})", layer.namespace));
                    if let Some(rec) = state.surfaces.get_mut(&layer.surface) {
                        rec.layer = None;
                    }
                    // focused surface going away: release keyboard focus
                    if state.focus_surface.as_ref() == Some(&layer.surface) {
                        if let (Some(kb), Ok(sf)) = (
                            state.keyboard.clone(),
                            wl_surface::WlSurface::from_id(dh, layer.surface.clone()),
                        ) {
                            let serial = state.serial();
                            kb.leave(serial, &sf);
                        }
                        state.focus_surface = None;
                    }
                    // pointer focus going away: send leave (best effort; the
                    // surface may already be destroyed on the client side)
                    if state.pointer_focus.as_ref() == Some(&layer.surface) {
                        if let (Some(ptr), Ok(sf)) = (
                            state.pointer.clone(),
                            wl_surface::WlSurface::from_id(dh, layer.surface.clone()),
                        ) {
                            ptr.leave(state.serial(), &sf);
                            ptr.frame();
                        }
                        state.pointer_focus = None;
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- wl_callback (no client requests) ----

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        _resource: &wl_callback::WlCallback,
        _request: wl_callback::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
    }
}

// ---- wl_output / wl_keyboard: no client requests handled ----

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        _resource: &wl_output::WlOutput,
        _request: wl_output::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        _resource: &wl_keyboard::WlKeyboard,
        _request: wl_keyboard::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
    }
}

fn usage() {
    eprintln!(
        "usage: mock-compositor [--mode idle|animate|keys] [--duration SECS] [--fps N]\n\
         \x20 [--capture-commit N | --capture-at MS] [--capture-out PATH] [--socket NAME]\n\
         \x20 [--output WxH] [--key-events name@ms,...]\n\
         \x20 [--pointer-events motion:x,y@ms;click[:l|m|r]@ms;press/release[:l|m|r]@ms;\
scroll:h,v@ms;scrollpx:h,v@ms;scrollstop@ms;leave@ms]"
    );
}

/// Parse a --pointer-events script into (ms, event) pairs. Events are
/// separated by ';'; coordinate pairs inside one event use ','.
fn parse_pointer_spec(spec: &str) -> Result<Vec<(u64, PtrEv)>, String> {
    let button = |spec: &str| -> Result<u32, String> {
        match spec {
            "" | "l" => Ok(0x110), // BTN_LEFT
            "r" => Ok(0x111),      // BTN_RIGHT
            "m" => Ok(0x112),      // BTN_MIDDLE
            other => Err(format!("unknown button {other:?}")),
        }
    };
    let mut out = Vec::new();
    for pair in spec.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (body, ms) = pair
            .split_once('@')
            .ok_or_else(|| format!("missing @ms in {pair:?}"))?;
        let ms: u64 = ms.parse().map_err(|_| format!("bad ms in {pair:?}"))?;
        let (verb, arg) = body.split_once(':').unwrap_or((body, ""));
        let ev = match verb {
            "motion" | "move" => {
                let (x, y) = arg
                    .split_once(',')
                    .ok_or_else(|| format!("motion wants x,y in {pair:?}"))?;
                PtrEv::Motion(
                    x.trim().parse().map_err(|_| format!("bad x in {pair:?}"))?,
                    y.trim().parse().map_err(|_| format!("bad y in {pair:?}"))?,
                )
            }
            "click" => PtrEv::Click(button(arg)?),
            "press" => PtrEv::Press(button(arg)?),
            "release" => PtrEv::Release(button(arg)?),
            "scroll" => {
                let (h, v) = arg
                    .split_once(',')
                    .ok_or_else(|| format!("scroll wants h,v in {pair:?}"))?;
                PtrEv::Scroll(
                    h.trim().parse().map_err(|_| format!("bad h in {pair:?}"))?,
                    v.trim().parse().map_err(|_| format!("bad v in {pair:?}"))?,
                )
            }
            "scrollpx" => {
                let (h, v) = arg
                    .split_once(',')
                    .ok_or_else(|| format!("scrollpx wants h,v in {pair:?}"))?;
                PtrEv::ScrollPx(
                    h.trim().parse().map_err(|_| format!("bad h in {pair:?}"))?,
                    v.trim().parse().map_err(|_| format!("bad v in {pair:?}"))?,
                )
            }
            "scrollstop" | "stop" => PtrEv::ScrollStop,
            "leave" => PtrEv::Leave,
            other => return Err(format!("unknown pointer event {other:?}")),
        };
        out.push((ms, ev));
    }
    Ok(out)
}

/// Key names → keycodes (matching the vendored minimal keymap and the
/// existing scripted sequence).
fn key_code_for(name: &str) -> Option<u32> {
    Some(match name {
        "esc" => 1,
        "enter" | "return" => 28,
        "backspace" => 14,
        "tab" => 15,
        "up" => 103,
        "down" => 108,
        "left" => 105,
        "right" => 106,
        "space" => 57,
        one if one.len() == 1 && one.as_bytes()[0].is_ascii_lowercase() => {
            // evdev KEY_<letter>
            KEY_LETTERS[(one.as_bytes()[0] - b'a') as usize]
        }
        _ => return None,
    })
}

/// evdev codes for a..z (KEY_A=30 … KEY_Z=44, in evdev's alphabetical order).
const KEY_LETTERS: [u32; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45,
    21, 44,
];

fn main() {
    let mut mode = Mode::Animate;
    let mut duration = 10.0f64;
    let mut fps = 120u32;
    let mut capture_commit: Option<u64> = None;
    let mut capture_path: Option<String> = None;
    let mut socket_name = "lme-mock-0".to_string();
    let mut output = (1920i32, 1080i32);

    let mut key_events: Vec<(u64, u32)> = Vec::new();
    let mut pointer_events: Vec<(u64, PtrEv)> = Vec::new();
    let mut capture_at_ms: Option<u64> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => mode = Mode::from_str(&args.next().unwrap_or_default()).unwrap_or(mode),
            "--duration" => duration = args.next().and_then(|v| v.parse().ok()).unwrap_or(duration),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).unwrap_or(fps),
            "--capture-commit" => {
                capture_commit = args.next().and_then(|v| v.parse().ok()).or(capture_commit)
            }
            "--capture-out" => capture_path = args.next().or(capture_path),
            "--socket" => socket_name = args.next().unwrap_or(socket_name),
            "--key-events" => {
                // "esc@2500,down@3000,enter@3500" — inject named keys at
                // millisecond offsets from start (name@ms pairs).
                let spec = args.next().unwrap_or_default();
                for pair in spec.split(',') {
                    let mut it = pair.trim().split('@');
                    let name = it.next().unwrap_or("").trim();
                    let ms: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(500);
                    if let Some(code) = key_code_for(name) {
                        key_events.push((ms, code));
                    } else {
                        eprintln!("unknown key name: {name}");
                    }
                }
            }
            "--pointer-events" => {
                // "motion:640,400@500;click@600;scroll:0,120@700;leave@800"
                // (events separated by ';', x/y pairs by ',')
                let spec = args.next().unwrap_or_default();
                match parse_pointer_spec(&spec) {
                    Ok(mut evs) => pointer_events.append(&mut evs),
                    Err(e) => {
                        eprintln!("bad --pointer-events: {e}");
                        std::process::exit(2);
                    }
                }
            }
            "--capture-at" => {
                capture_at_ms = args.next().and_then(|v| v.parse().ok()).or(capture_at_ms)
            }
            "--output" => {
                if let Some(v) = args.next() {
                    if let Some((w, h)) = v.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                            output = (w, h);
                        }
                    }
                }
            }
            _ => {
                usage();
                std::process::exit(2);
            }
        }
    }

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        let dir = "/tmp/lme-mock-run".to_string();
        fs::create_dir_all(&dir).ok();
        dir
    });
    fs::create_dir_all(&runtime_dir).expect("runtime dir");

    let mut display = Display::<State>::new().expect("display");
    let dh = display.handle();

    dh.create_global::<State, wl_compositor::WlCompositor, ()>(6, ());
    dh.create_global::<State, wl_shm::WlShm, ()>(1, ());
    dh.create_global::<State, wl_output::WlOutput, ()>(4, ());
    // seat v8: axis_value120 requires a wl_pointer of version >= 8
    dh.create_global::<State, wl_seat::WlSeat, ()>(8, ());
    dh.create_global::<State, LayerShellIface, ()>(5, ());

    // Scripted input: key events become press+release pairs 45 ms apart;
    // pointer events run verbatim; both end the run after a grace period.
    let mut scheduled: VecDeque<(Duration, u32, wl_keyboard::KeyState)> = VecDeque::new();
    let mut pointer_script: VecDeque<(Duration, PtrEv)> = VecDeque::new();
    let mut pointer_done_at: Option<Instant> = None;
    if !pointer_events.is_empty() {
        let mut last = 0u64;
        for (ms, ev) in pointer_events {
            last = last.max(ms);
            pointer_script.push_back((Duration::from_millis(ms), ev));
        }
        // end the run a grace period after the last scripted pointer event
        pointer_done_at = Some(Instant::now() + Duration::from_millis(last + 3_000));
    }
    for (ms, code) in key_events {
        scheduled.push_back((
            Duration::from_millis(ms),
            code,
            wl_keyboard::KeyState::Pressed,
        ));
        scheduled.push_back((
            Duration::from_millis(ms + 45),
            code,
            wl_keyboard::KeyState::Released,
        ));
    }

    // Bind relative to XDG_RUNTIME_DIR when set (the normal case); otherwise bind an
    // absolute path in the fallback dir so the mock works in bare environments.
    let listen_socket = if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        ListeningSocket::bind(&socket_name).expect("bind socket")
    } else {
        let path = std::path::Path::new(&runtime_dir).join(&socket_name);
        let _ = fs::remove_file(&path);
        ListeningSocket::bind_absolute(path).expect("bind socket (absolute)")
    };
    let socket_path = listen_socket
        .socket_name()
        .map(|n| std::path::Path::new(&runtime_dir).join(n))
        .unwrap_or_else(|| std::path::PathBuf::from(socket_name.clone()));
    eprintln!("socket path: {}", socket_path.display());

    let capture_at = capture_at_ms.map(|ms| {
        (
            Instant::now() + Duration::from_millis(ms),
            capture_path
                .clone()
                .unwrap_or_else(|| "mock-capture.png".to_string()),
        )
    });

    let mut state = State {
        mode,
        start: Instant::now(),
        duration: Duration::from_secs_f64(duration),
        frame_interval: Duration::from_secs_f64(1.0 / fps as f64),
        next_serial: 100,
        surfaces: HashMap::new(),
        layers: HashMap::new(),
        pools: HashMap::new(),
        buffers: HashMap::new(),
        keyboard: None,
        focus_surface: None,
        pending_frames: Vec::new(),
        key_script: scheduled,
        keys_done_at: None,
        pointer: None,
        pointer_script,
        pointer_pos: None,
        pointer_focus: None,
        pointer_events_sent: 0,
        pointer_done_at,
        capture_commit,
        capture_path,
        capture_at,
        keymap_file: None,
        first_bind_at: None,
        mapped_at: None,
        frames_fired: 0,
        late_frames: 0,
        dropped_frames: 0,
        commits_total: 0,
        map_seq_next: 0,
        output_w: output.0,
        output_h: output.1,
    };

    state.slog(
        "START",
        &format!(
            "socket={} mode={:?} fps={} duration={}s output={}x{}",
            socket_name, state.mode, fps, duration, output.0, output.1
        ),
    );

    // client data kept minimal: the mock tracks liveness via dispatch/flush errors
    struct MockClientData;
    impl ClientData for MockClientData {}

    loop {
        // accept new clients (non-blocking)
        while let Some(stream) = listen_socket.accept().expect("accept") {
            display
                .handle()
                .insert_client(stream, Arc::new(MockClientData))
                .expect("insert client");
            state.slog("CONNECT", "client connected");
        }

        // dispatch is non-blocking on the rust backend (epoll timeout 0)
        match display.dispatch_clients(&mut state) {
            Ok(_) => {}
            Err(e) => {
                state.slog("DISPATCH-ERR", &format!("{}", e));
                break;
            }
        }
        if display.flush_clients().is_err() {
            state.slog("CLIENT-GONE", "flush failed (client exited)");
            break;
        }
        if state.tick(&dh) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let elapsed_s = state.start.elapsed().as_secs_f64();
    let summary = format!(
        "{{\"mode\":\"{:?}\",\"frames_fired\":{},\"delivered_fps\":{:.1},\"late_frames\":{},\"dropped_frames\":{},\"commits\":{},\"bind_to_map_ms\":{},\"pointer_events\":{},\"duration_ms\":{}}}",
        state.mode,
        state.frames_fired,
        if elapsed_s > 0.0 {
            state.frames_fired as f64 / elapsed_s
        } else {
            0.0
        },
        state.late_frames,
        state.dropped_frames,
        state.commits_total,
        state
            .mapped_at
            .zip(state.first_bind_at)
            .map(|(m, b)| m.duration_since(b).as_millis())
            .unwrap_or(0),
        state.pointer_events_sent,
        state.start.elapsed().as_millis(),
    );
    state.slog("SUMMARY", &summary);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_rows_to_rgba_swizzles_and_strips_stride() {
        // 2x1 image, stride 12 (one padding dword per row)
        // pixels: B=10 G=20 R=30 A=255 | B=40 G=50 R=60 A=128, then padding
        let raw = [10u8, 20, 30, 255, 40, 50, 60, 128, 0, 0, 0, 0];
        let out = bgra_rows_to_rgba(&raw, 2, 1, 12);
        assert_eq!(out, vec![30, 20, 10, 255, 60, 50, 40, 128]);
    }

    #[test]
    fn bgra_rows_handles_multiple_rows() {
        let mut raw = Vec::new();
        for row in 0..3u8 {
            raw.extend_from_slice(&[row, row + 1, row + 2, 255]); // BGRA
            raw.extend_from_slice(&[0, 0, 0, 0]); // stride pad to 8
        }
        let out = bgra_rows_to_rgba(&raw, 1, 3, 8);
        assert_eq!(out.len(), 12);
        assert_eq!(out[0..4], [2, 1, 0, 255]); // first row R,G,B,A
        assert_eq!(out[8..12], [4, 3, 2, 255]); // third row
    }

    #[test]
    fn keymap_contains_required_keysyms() {
        for sym in [
            "Escape",
            "BackSpace",
            "Return",
            "Left",
            "Right",
            "Up",
            "Down",
        ] {
            assert!(KEYMAP.contains(sym), "keymap missing {}", sym);
        }
        assert!(KEYMAP.contains("[ p, P ]"), "keymap missing p/P level pair");
    }
}

#[cfg(test)]
mod pointer_spec_tests {
    use super::*;

    #[test]
    fn parses_full_script() {
        let evs = parse_pointer_spec(
            "motion:640,15@100;click@200;click:r@250;press:m@300;release:m@310;\
             scroll:0,1@400;scroll:-2,0@450;scrollpx:0,25.5@500;scrollstop@600;leave@700",
        )
        .expect("valid script");
        assert_eq!(evs[0], (100, PtrEv::Motion(640.0, 15.0)));
        assert_eq!(evs[1], (200, PtrEv::Click(0x110)));
        assert_eq!(evs[2], (250, PtrEv::Click(0x111))); // r = BTN_RIGHT
        assert_eq!(evs[3], (300, PtrEv::Press(0x112))); // m = BTN_MIDDLE
        assert_eq!(evs[4], (310, PtrEv::Release(0x112)));
        assert_eq!(evs[5], (400, PtrEv::Scroll(0, 1)));
        assert_eq!(evs[6], (450, PtrEv::Scroll(-2, 0)));
        assert_eq!(evs[7], (500, PtrEv::ScrollPx(0.0, 25.5)));
        assert_eq!(evs[8], (600, PtrEv::ScrollStop));
        assert_eq!(evs[9], (700, PtrEv::Leave));
        assert_eq!(evs.len(), 10);
    }

    #[test]
    fn rejects_bad_scripts() {
        assert!(parse_pointer_spec("motion:640").is_err()); // missing @ms
        assert!(parse_pointer_spec("click:x@10").is_err()); // unknown button
        assert!(parse_pointer_spec("teleport:1,2@10").is_err()); // unknown verb
        assert!(parse_pointer_spec("scroll:5@10").is_err()); // wants h,v
        assert!(parse_pointer_spec("motion:abc,2@10").is_err()); // bad coords
    }
}
