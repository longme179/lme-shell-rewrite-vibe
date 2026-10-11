// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway Phase 2 stack-spike tool (step 2.0). See native/spike/README.md.
//! Headless mock Wayland compositor for stack measurements.
//!
//! Implements just enough wire protocol for the spike clients: wl_compositor,
//! wl_shm, wl_output, wl_seat (keyboard), zwlr_layer_shell_v1 (v5).
//!
//! Modes:
//!   idle     - frame callbacks fire immediately on commit (client draws once, sleeps)
//!   animate  - frame callbacks paced at --fps (client redraw loop => CPU/RSS under load)
//!   keys     - like idle plus a scripted key sequence (p o w e r BackSpace Down Up Esc)
//!
//! Also captures the Nth committed buffer to a PNG (benchmark evidence).
//! Logs timestamped lines for the bench script to parse: BIND0, MAPPED, CAPTURED, SUMMARY{...}

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
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_region, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
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
}

struct LayerRec {
    surface: ObjectId,
    width: i32,
    height: i32,
    acked: bool,
    interactivity: Option<String>,
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
    pending_frames: Vec<(wl_callback::WlCallback, Instant)>,
    key_script: VecDeque<(Duration, u32, wl_keyboard::KeyState)>,
    keys_done_at: Option<Instant>,
    capture_commit: Option<u64>,
    capture_path: Option<String>,
    keymap_file: Option<fs::File>,
    first_bind_at: Option<Instant>,
    mapped_at: Option<Instant>,
    frames_fired: u64,
    commits_total: u64,
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

    fn tick(&mut self) -> bool {
        // returns false when the run should end
        let elapsed = self.start.elapsed();

        // fire due frame callbacks
        let time = elapsed.as_millis() as u32;
        let mut i = 0;
        while i < self.pending_frames.len() {
            if self.pending_frames[i].1 <= Instant::now() {
                let (cb, _) = self.pending_frames.remove(i);
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

        // keys-mode early end: script scheduled, done, grace period passed
        if let Some(done) = self.keys_done_at {
            if Instant::now() > done {
                return true;
            }
        }

        elapsed >= self.duration
    }

    /// Fire callbacks for a commit according to mode pacing.
    fn queue_frames(&mut self, surf_id: &ObjectId) {
        let Some(rec) = self.surfaces.get_mut(surf_id) else {
            return;
        };
        let cbs = std::mem::take(&mut rec.frame_callbacks);
        for cb in cbs {
            let ready = match self.mode {
                Mode::Animate => Instant::now() + self.frame_interval,
                _ => Instant::now(),
            };
            self.pending_frames.push((cb, ready));
        }
    }

    fn capture(&self, buf: &BufferRec) -> Option<String> {
        use std::os::unix::fs::FileExt;
        let pool = self.pools.get(&buf.pool)?;
        let path = self.capture_path.clone()?;
        let len = buf.h.checked_mul(buf.stride)? as usize;
        let mut raw = vec![0u8; len];
        pool.fd.read_exact_at(&mut raw, buf.offset as u64).ok()?;
        let rgba = bgra_rows_to_rgba(&raw, buf.w, buf.h, buf.stride);
        let file = fs::File::create(&path).ok()?;
        let mut enc = png::Encoder::new(file, buf.w as u32, buf.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&rgba).ok()?;
        Some(path)
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
                            if layer.width > 0 && layer.height > 0 {
                                (layer.width, layer.height)
                            } else {
                                (state.output_w, state.output_h)
                            }
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
                    // keyboard enter
                    let kb = state.keyboard.clone();
                    if let Some(kb) = kb {
                        let serial = state.serial();
                        kb.enter(serial, resource, Vec::new());
                        state.focus_surface = Some(resource.id().clone());
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

// ---- wl_seat / keyboard ----

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
        seat.capabilities(wl_seat::Capability::Keyboard);
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
            wl_seat::Request::Release => {}
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
            layer: _,
            namespace,
        } = request
        {
            let ls = data_init.init(id, ());
            state.layers.insert(
                ls.id().clone(),
                LayerRec {
                    surface: surface.id().clone(),
                    width: 0,
                    height: 0,
                    acked: false,
                    interactivity: None,
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
        _dh: &DisplayHandle,
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
                    if let Some(rec) = state.surfaces.get_mut(&layer.surface) {
                        rec.layer = None;
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
         \x20 [--capture-commit N] [--capture-out PATH] [--socket NAME] [--output WxH]"
    );
}

fn main() {
    let mut mode = Mode::Animate;
    let mut duration = 10.0f64;
    let mut fps = 120u32;
    let mut capture_commit: Option<u64> = None;
    let mut capture_path: Option<String> = None;
    let mut socket_name = "lme-mock-0".to_string();
    let mut output = (1920i32, 1080i32);

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
    dh.create_global::<State, wl_seat::WlSeat, ()>(7, ());
    dh.create_global::<State, LayerShellIface, ()>(5, ());

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
        key_script: VecDeque::new(),
        keys_done_at: None,
        capture_commit,
        capture_path,
        keymap_file: None,
        first_bind_at: None,
        mapped_at: None,
        frames_fired: 0,
        commits_total: 0,
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
        if state.tick() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let summary = format!(
        "{{\"mode\":\"{:?}\",\"frames_fired\":{},\"commits\":{},\"bind_to_map_ms\":{},\"duration_ms\":{}}}",
        state.mode,
        state.frames_fired,
        state.commits_total,
        state
            .mapped_at
            .zip(state.first_bind_at)
            .map(|(m, b)| m.duration_since(b).as_millis())
            .unwrap_or(0),
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
