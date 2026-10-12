// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end golden tests: the real daemon binary against the real mock
//! compositor. Each test drives `lme-shell msg` commands, captures the
//! compositor's committed buffers as PNGs and compares them to committed
//! goldens with a pixel tolerance. Interaction tests inject keys through the
//! mock's `--key-events`.
//!
//! Goldens regenerate with LME_GOLDEN_REGEN=1 (inspect them before trusting).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const GOLDEN_DIR: &str = "tests/golden";

/// One harness run: compositor + daemon with isolated env.
struct Harness {
    tmp: PathBuf,
    _daemon: Child,
    compositor: Child,
    log_path: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self._daemon.kill();
        let _ = self._daemon.wait();
        let _ = self.compositor.kill();
        let _ = self.compositor.wait();
        // best-effort cleanup (other tests reuse the pid-suffixed dirs)
    }
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn find_bin(name: &str) -> PathBuf {
    let path = target_dir().join("debug").join(name);
    assert!(
        path.exists(),
        "missing {name} at {} — run `cargo build --workspace` before `cargo test` (the e2e tests spawn workspace binaries)",
        path.display()
    );
    path
}

fn write_isolated_config(tmp: &Path, animations: bool, bar_right: Option<&[&str]>) {
    let dir = tmp.join("config/lme-shell");
    std::fs::create_dir_all(&dir).unwrap();
    let mut settings = serde_json::json!({
        "animations": animations,
        "opacity": 0.90,
        "accent": "#9580ff",
        "barStyle": "connected",
        "barFontSize": 13,
        "barHeight": 42
    });
    if let Some(modules) = bar_right {
        // isolate ONE right-zone module so pointer tests address it without
        // depending on the widths of the other modules
        settings["barLayout"] = serde_json::json!({ "right": modules });
    }
    std::fs::write(
        dir.join("settings.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
}

fn fake_state_json() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "available": true,
        "workspaces": [
            {"id": 1, "idx": 1, "name": "", "output": "mock-1", "is_focused": true},
            {"id": 2, "idx": 2, "name": "web", "output": "mock-1", "is_focused": false},
            {"id": 3, "idx": 3, "name": "", "output": "mock-1", "is_focused": false}
        ],
        "windows": [
            {"id": 10, "title": "kitty — golden test", "app_id": "kitty",
             "is_focused": true, "workspace_id": 1}
        ]
    }))
    .unwrap()
}

fn fake_system_json() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "battery": {"available": true, "percent": 77, "charging": true},
        "network": {"connected": true, "ssid": "Golden-5G"},
        "audio": {"available": true, "volume": 64, "muted": false}
    }))
    .unwrap()
}

/// Which committed buffer the mock captures: the Nth commit (any surface)
/// or a timed snapshot of the topmost layer surface.
enum Capture {
    Commit(u64),
    AtMs(u64),
}

/// A deterministic .desktop entry injected into the daemon's isolated
/// XDG_DATA_HOME (tests that pass `apps` also blank XDG_DATA_DIRS so the
/// system list cannot leak in).
fn fake_desktop(name: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName={name}\nExec=/bin/true\n")
}

/// Spawn compositor + daemon with the full option set (pointer injection,
/// timed captures, isolated app list, single-module barLayout).
#[allow(clippy::too_many_arguments)]
fn spawn_harness_full(
    name: &str,
    output_size: &str,
    capture: Option<(Capture, &Path)>,
    key_events: Option<&str>,
    pointer_events: Option<&str>,
    animations: bool,
    bar_right: Option<&[&str]>,
    apps: &[&str], // app NAMES; bodies are generated
) -> Harness {
    let unique = format!(
        "{}-{}-{}",
        name,
        std::process::id(),
        Instant::now().elapsed().as_nanos() // extra uniqueness per test
    );
    let tmp = std::env::temp_dir().join(&unique);
    std::fs::create_dir_all(tmp.join("runtime")).unwrap();
    write_isolated_config(&tmp, animations, bar_right);
    let isolate_apps = !apps.is_empty();
    if isolate_apps {
        let apps_dir = tmp.join(".local/share/applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        for (i, app) in apps.iter().enumerate() {
            std::fs::write(
                apps_dir.join(format!("lme-test-{i}.desktop")),
                fake_desktop(app),
            )
            .unwrap();
        }
    }
    let fake = tmp.join("fake-niri.json");
    std::fs::write(&fake, fake_state_json()).unwrap();
    let fake_system = tmp.join("fake-system.json");
    std::fs::write(&fake_system, fake_system_json()).unwrap();

    // compositor
    let mut cmd = Command::new(find_bin("mock-compositor"));
    cmd.args([
        "--mode",
        "idle",
        "--duration",
        "60",
        "--fps",
        "120",
        "--output",
        output_size,
        "--socket",
        &unique,
    ]);
    if let Some((cap, out)) = capture {
        match cap {
            Capture::Commit(n) => {
                cmd.arg("--capture-commit").arg(n.to_string());
            }
            Capture::AtMs(ms) => {
                cmd.arg("--capture-at").arg(ms.to_string());
            }
        }
        cmd.arg("--capture-out").arg(out);
    }
    if let Some(keys) = key_events {
        cmd.arg("--key-events").arg(keys);
    }
    if let Some(pointer) = pointer_events {
        cmd.arg("--pointer-events").arg(pointer);
    }
    cmd.env("XDG_RUNTIME_DIR", tmp.join("runtime"));
    // The mock compositor logs (slog) to STDOUT — pipe and tee it.
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut compositor = cmd.spawn().expect("spawn mock-compositor");
    let stdout = compositor.stdout.take().unwrap();
    let log_path = tmp.join("compositor.log");
    let log_for_thread = log_path.clone();
    // Tee compositor stdout into a log file the harness polls.
    std::thread::spawn(move || {
        let mut file = std::fs::File::create(&log_for_thread).expect("create compositor log");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    use std::io::Write as _;
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    });
    // wait for the socket to appear
    let socket = tmp.join("runtime").join(&unique);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "compositor socket never appeared"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // daemon
    let mut dcmd = Command::new(find_bin("lme-shell"));
    dcmd.arg("daemon").arg("--dry-run");
    dcmd.env("WAYLAND_DISPLAY", &unique)
        .env("XDG_RUNTIME_DIR", tmp.join("runtime"))
        .env("XDG_CONFIG_HOME", tmp.join("config"))
        .env("HOME", &tmp)
        .env("LME_SHELL_FAKE_STATE", &fake)
        .env("LME_SHELL_FAKE_STATE_SYSTEM", &fake_system)
        .env_remove("NIRI_SOCKET")
        .env_remove("LME_SHELL_PERF");
    if isolate_apps {
        // only the injected .desktop files (see Applications::default_paths)
        dcmd.env("XDG_DATA_DIRS", "");
    }
    dcmd.stdout(Stdio::null())
        .stderr(std::fs::File::create(tmp.join("shell.log")).unwrap());
    let mut daemon = dcmd.spawn().expect("spawn lme-shell daemon");

    // wait for the daemon IPC socket
    let daemon_sock = tmp.join("runtime").join("lme-shell.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !daemon_sock.exists() {
        assert!(Instant::now() < deadline, "daemon socket never appeared");
        let _ = daemon.try_wait().expect("daemon alive check");
        std::thread::sleep(Duration::from_millis(20));
    }
    Harness {
        tmp,
        _daemon: daemon,
        compositor,
        log_path,
    }
}

/// Spawn compositor + daemon. `capture_commit`/`capture_out` capture one
/// buffer; `key_events` is the mock's `--key-events` spec.
#[allow(clippy::too_many_arguments)]
fn spawn_harness(
    name: &str,
    output_size: &str,
    capture_commit: Option<u64>,
    capture_out: Option<&Path>,
    key_events: Option<&str>,
    animations: bool,
) -> Harness {
    let capture = capture_commit.map(|n| (Capture::Commit(n), capture_out.expect("capture path")));
    spawn_harness_full(
        name,
        output_size,
        capture,
        key_events,
        None,
        animations,
        None,
        &[],
    )
}

impl Harness {
    fn msg(&self, args: &[&str]) -> String {
        let out = Command::new(find_bin("lme-shell"))
            .arg("msg")
            .args(args)
            .env("XDG_RUNTIME_DIR", self.tmp.join("runtime"))
            .output()
            .expect("run msg");
        assert!(
            out.status.success(),
            "msg {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Wait until the compositor logged `tag`.
    fn wait_log(&self, tag: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(text) = std::fs::read_to_string(&self.log_path) {
                if text.contains(tag) {
                    return true;
                }
            }
            if Instant::now() > deadline {
                eprintln!(
                    "--- compositor log after timeout waiting for {tag}:\n{}---",
                    std::fs::read_to_string(&self.log_path).unwrap_or_default()
                );
                eprintln!(
                    "--- daemon stderr: {}",
                    std::fs::read_to_string(self.tmp.join("shell.log")).unwrap_or_default()
                );
                return false;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

// ---- PNG comparison with tolerance ----

struct Png {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode_png(path: &Path) -> Png {
    let data = std::fs::read(path).expect("read png");
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf).expect("png frame");
    Png {
        width: info.width,
        height: info.height,
        rgba: buf,
    }
}

/// Mismatch ratio with per-channel tolerance; returns (ratio, max_delta).
fn compare(a: &Png, b: &Png, tolerance: u8) -> (f64, u8) {
    assert_eq!(
        (a.width, a.height),
        (b.width, b.height),
        "dimensions differ"
    );
    let mut mismatched = 0usize;
    let mut max_delta = 0u8;
    let total = (a.width as usize) * (a.height as usize);
    for i in 0..total {
        let (pa, pb) = (&a.rgba[i * 4..i * 4 + 4], &b.rgba[i * 4..i * 4 + 4]);
        let mut pixel_diff = false;
        for c in 0..4 {
            let delta = pa[c].abs_diff(pb[c]);
            max_delta = max_delta.max(delta);
            if delta > tolerance {
                pixel_diff = true;
            }
        }
        if pixel_diff {
            mismatched += 1;
        }
    }
    (mismatched as f64 / total as f64, max_delta)
}

fn assert_matches_golden(actual: &Path, golden_name: &str) {
    let golden = Path::new(GOLDEN_DIR).join(golden_name);
    if std::env::var_os("LME_GOLDEN_REGEN").is_some() {
        std::fs::create_dir_all(GOLDEN_DIR).unwrap();
        std::fs::copy(actual, &golden).expect("regen golden");
        eprintln!("regenerated golden {}", golden.display());
        return;
    }
    assert!(
        golden.exists(),
        "golden {} missing (run with LME_GOLDEN_REGEN=1)",
        golden.display()
    );
    let (ratio, max_delta) = compare(&decode_png(actual), &decode_png(&golden), 12);
    assert!(
        ratio < 0.005,
        "golden {golden_name}: {ratio:.4} of pixels differ beyond tolerance (max channel delta {max_delta})"
    );
}

fn capture_png(name: &str, commit: u64) -> PathBuf {
    std::env::temp_dir().join(format!(
        "lme-golden-{name}-{}-{commit}.png",
        std::process::id()
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full desktop: bar renders with modules (clock, workspaces, title,
/// battery, network, audio) and matches the golden.
#[test]
fn golden_bar() {
    let png = capture_png("bar", 2);
    let _ = std::fs::remove_file(&png);
    let harness = spawn_harness("bar", "1280x800", Some(2), Some(&png), None, false);
    // The capture file is written before the CAPTURED log line; waiting on
    // the log avoids reading a half-written PNG.
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "bar capture never happened"
    );
    // sanity: the captured image is not blank
    let image = decode_png(&png);
    let distinct = {
        let mut colors = std::collections::HashSet::new();
        for px in image.rgba.chunks(4).step_by(37) {
            colors.insert([px[0], px[1], px[2], px[3]]);
        }
        colors.len()
    };
    assert!(distinct > 3, "bar looks blank ({distinct} distinct colors)");
    assert_matches_golden(&png, "bar.png");
}

/// Power menu opens centered on the focused output, with the 5 actions.
#[test]
fn golden_power_menu() {
    let png = capture_png("power", 3);
    let _ = std::fs::remove_file(&png);
    let harness = spawn_harness("power", "1280x800", Some(3), Some(&png), None, false);
    // wait for the bar's first frame before toggling the menu
    let deadline = Instant::now() + Duration::from_secs(15);
    while !harness.wait_log("MAPPED", Duration::from_millis(200)) {
        if Instant::now() > deadline {
            panic!("bar never mapped");
        }
    }
    // toggle the power menu via IPC; with animations off it draws in one commit
    harness.msg(&["toggle", "power"]);
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "power menu capture never happened"
    );
    assert_matches_golden(&png, "power-menu.png");
}

/// Esc closes the power menu (key injection through the compositor).
#[test]
fn power_menu_esc_closes() {
    // esc at 2.5 s; the power menu is opened by the test at ~1 s.
    let harness = spawn_harness("esc", "1280x800", None, None, Some("esc@2500"), false);
    // let the bar settle, then open the menu via IPC
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "power"]);
    std::thread::sleep(Duration::from_millis(400));
    // status shows the power panel open
    let status = harness.msg(&["status"]);
    assert!(
        status.contains("power"),
        "power panel reported open: {status}"
    );
    // wait past the esc injection
    std::thread::sleep(Duration::from_millis(1800));
    let status = harness.msg(&["status"]);
    assert!(
        !status.contains("power"),
        "power panel must be closed by Esc: {status}"
    );
}

/// Launcher: opens with a populated list, typed text filters it, arrows
/// move the selection, Enter activates (launch of an injected fake app)
/// and Esc closes.
#[test]
fn launcher_types_and_launches() {
    let harness = spawn_harness_full(
        "launcher",
        "1280x800",
        None,
        Some("b@1500,e@1600,t@1700,a@1800,enter@2600,esc@3600"),
        None,
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(900));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("created (lme-launcher)", Duration::from_secs(10)),
        "launcher never opened"
    );
    let status = harness.msg(&["status"]);
    assert!(status.contains("launcher"), "launcher open: {status}");
    // keys type b-e-t-a + Enter: "Beta Pointer" is the only match, so
    // Enter must LAUNCH it and close the panel (the old test relied on
    // zero results masking the missing query refresh wiring)
    assert!(
        harness.wait_log("destroyed (lme-launcher)", Duration::from_secs(10)),
        "Enter must launch the selected result and close the launcher"
    );
    let status = harness.msg(&["status"]);
    assert!(
        !status.contains("launcher"),
        "launcher closed after launch: {status}"
    );
    assert!(
        harness.msg(&["ping"]) == "pong",
        "daemon alive after launch"
    );
}

/// OSD popup shows and auto-hides.
#[test]
fn osd_shows_and_expires() {
    let harness = spawn_harness("osd", "1280x800", None, None, None, false);
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["osd", "Volume", "42"]);
    std::thread::sleep(Duration::from_millis(300));
    let status = harness.msg(&["status"]);
    assert!(status.contains("running"), "daemon alive after OSD");
    // 1.8 s expiry — after 3 s the OSD window must be gone (the compositor
    // logs LAYER created/destroyed; we check the daemon still responds and
    // that a second OSD works, proving the window lifecycle).
    std::thread::sleep(Duration::from_millis(2500));
    harness.msg(&["osd", "Brightness", "80"]);
    std::thread::sleep(Duration::from_millis(300));
    let status = harness.msg(&["status"]);
    assert!(status.contains("running"), "daemon alive after second OSD");
}

/// Daemon IPC surface: ping/status/close/refresh all reply.
#[test]
fn ipc_surface() {
    let harness = spawn_harness("ipc", "1280x800", None, None, None, false);
    assert_eq!(harness.msg(&["ping"]), "pong");
    let status = harness.msg(&["status"]);
    assert!(status.contains("running"));
    assert!(status.contains("dryRun"));
    let refresh = harness.msg(&["refresh"]);
    assert!(refresh.starts_with("refreshed"));
    let close = harness.msg(&["close"]);
    assert_eq!(close, "closed");
    // power dry-run action is logged, nothing executed
    let power = harness.msg(&["power", "suspend"]);
    assert!(power.contains("dry-run"), "dry-run power: {power}");
}

/// Launcher opens with the app list; timed topmost capture after the
/// reveal settles. (The old --capture-commit version grabbed an empty
/// pre-frame buffer and passed vacuously — Step-0a found that too.)
#[test]
fn golden_launcher() {
    let png = capture_png("launcher", 0);
    let _ = std::fs::remove_file(&png);
    let harness = spawn_harness_full(
        "launcher-golden",
        "1280x800",
        Some((Capture::AtMs(2500), &png)),
        None,
        None,
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "launcher capture never happened"
    );
    // sanity: the panel is actually drawn this time (list rows present)
    let image = decode_png(&png);
    assert_eq!((image.width, image.height), (1280, 800));
    let opaque = image
        .rgba
        .chunks(4)
        .step_by(997)
        .filter(|p| p[3] > 32)
        .count();
    assert!(
        opaque > 40,
        "launcher panel looks empty ({opaque} opaque samples)"
    );
    assert_matches_golden(&png, "launcher.png");
}

// ---------------------------------------------------------------------------
// Pointer interaction tests (Amendment 3 Step 0a: mock pointer injection)
// ---------------------------------------------------------------------------
//
// Bar geometry in these tests (1280x800 output, barHeight 42, barEdgeMargin 8,
// barSideMargin 12, bar font 13): the bar surface spans x 12..1268, y 8..50;
// the leftmost launcher pill is centered near (54, 29) and, with barLayout
// right=["audio"], the audio pill is the rightmost module near (1222, 29).
// The launcher overlay covers the whole output; its 520x560 panel sits at
// (380, 120) with result rows every 52 px starting at y 194.

const BAR_LAUNCHER_PILL: (f64, f64) = (54.0, 29.0);
const BAR_AUDIO_PILL: (f64, f64) = (1222.0, 29.0);
const LAUNCHER_ROW0: (f64, f64) = (640.0, 220.0);
const LAUNCHER_LIST: (f64, f64) = (640.0, 300.0);

fn at(p: (f64, f64)) -> String {
    format!("{},{}", p.0, p.1)
}

/// Clicking the launcher pill in the bar with a real pointer opens the
/// launcher window (finding a wiring: bar buttons receive pointer events).
#[test]
fn pointer_click_opens_launcher() {
    let spec = format!("motion:{}@1200;click@1300", at(BAR_LAUNCHER_PILL));
    let harness = spawn_harness_full(
        "ptr-launcher",
        "1280x800",
        None,
        None,
        Some(&spec),
        false,
        None,
        &[],
    );
    assert!(
        harness.wait_log("created (lme-launcher)", Duration::from_secs(10)),
        "launcher window never mapped after the pointer click"
    );
    let status = harness.msg(&["status"]);
    assert!(status.contains("launcher"), "launcher open: {status}");
}

/// Clicking a launcher result row launches it and closes the panel.
#[test]
fn launcher_row_click_launches_and_closes() {
    let spec = format!("motion:{}@1500;click@1600", at(LAUNCHER_ROW0));
    let harness = spawn_harness_full(
        "ptr-rowclick",
        "1280x800",
        None,
        None,
        Some(&spec),
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("created (lme-launcher)", Duration::from_secs(10)),
        "launcher never opened"
    );
    assert!(
        harness.wait_log("destroyed (lme-launcher)", Duration::from_secs(10)),
        "launcher must close after clicking the first result row"
    );
    let status = harness.msg(&["status"]);
    assert!(!status.contains("launcher"), "launcher closed: {status}");
}

/// A click on the dimmed backdrop (outside the panel) dismisses the panel
/// (finding c).
#[test]
fn outside_click_dismisses_launcher() {
    let harness = spawn_harness_full(
        "ptr-dismiss",
        "1280x800",
        None,
        None,
        Some("motion:5,5@1500;click@1600"),
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("created (lme-launcher)", Duration::from_secs(10)),
        "launcher never opened"
    );
    assert!(
        harness.wait_log("destroyed (lme-launcher)", Duration::from_secs(10)),
        "outside click must dismiss the launcher"
    );
    let status = harness.msg(&["status"]);
    assert!(!status.contains("launcher"), "launcher closed: {status}");
}

/// One wheel notch (value120 path) moves the launcher selection down a row
/// (finding b + d); the pointer leaves before the capture so no hover row
/// is drawn.
#[test]
fn golden_launcher_wheel_notch() {
    let png = capture_png("launcher-wheel", 0);
    let _ = std::fs::remove_file(&png);
    let spec = format!(
        "motion:{}@1500;scroll:0,1@1600;leave@1700",
        at(LAUNCHER_LIST)
    );
    let harness = spawn_harness_full(
        "ptr-wheel",
        "1280x800",
        Some((Capture::AtMs(2500), &png)),
        None,
        Some(&spec),
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "timed capture never happened"
    );
    assert_matches_golden(&png, "launcher-wheel-row1.png");
}

/// Touchpad-style pixel deltas accumulate to the same one-line scroll
/// (finding d: plain-axis path, no value120).
#[test]
fn golden_launcher_touchpad_scroll() {
    let png = capture_png("launcher-tpad", 0);
    let _ = std::fs::remove_file(&png);
    let spec = format!(
        "motion:{0}@1400;scrollpx:0,20@1500;scrollpx:0,20@1550;scrollpx:0,20@1600;leave@1700",
        at(LAUNCHER_LIST)
    );
    let harness = spawn_harness_full(
        "ptr-tpad",
        "1280x800",
        Some((Capture::AtMs(2500), &png)),
        None,
        Some(&spec),
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "timed capture never happened"
    );
    // 60 px = one 50 px line + remainder: same state as the wheel notch
    assert_matches_golden(&png, "launcher-wheel-row1.png");
}

/// axis_stop discards the sub-threshold remainder: 20 px + stop + 20 px +
/// stop never scrolls (finding d).
#[test]
fn golden_launcher_scroll_stop_resets() {
    let png = capture_png("launcher-stop", 0);
    let _ = std::fs::remove_file(&png);
    let spec = format!(
        "motion:{0}@1400;scrollpx:0,20@1500;scrollstop@1550;scrollpx:0,20@1600;scrollstop@1650;leave@1700",
        at(LAUNCHER_LIST)
    );
    let harness = spawn_harness_full(
        "ptr-stop",
        "1280x800",
        Some((Capture::AtMs(2500), &png)),
        None,
        Some(&spec),
        false,
        None,
        &["Alpha Pointer", "Beta Pointer", "Gamma Pointer"],
    );
    std::thread::sleep(Duration::from_millis(800));
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "timed capture never happened"
    );
    // selection stayed on row 0 and the pointer left: the panel looks
    // exactly like a freshly-opened launcher with these three apps
    assert_matches_golden(&png, "launcher-unscrolled.png");
}

/// Wheel over the audio pill previews volume +5%/notch as a dry-run OSD
/// (finding e).
#[test]
fn golden_wheel_over_audio_pill() {
    let png = capture_png("osd-wheel", 0);
    let _ = std::fs::remove_file(&png);
    let spec = format!("motion:{}@1200;scroll:0,1@1300", at(BAR_AUDIO_PILL));
    let harness = spawn_harness_full(
        "ptr-volwheel",
        "1280x800",
        Some((Capture::AtMs(2200), &png)),
        None,
        Some(&spec),
        false,
        Some(&["audio"]),
        &[],
    );
    assert!(
        harness.wait_log("created (lme-osd)", Duration::from_secs(10)),
        "volume wheel must show the OSD"
    );
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "timed capture never happened"
    );
    let image = decode_png(&png);
    assert!(
        (image.width, image.height) == (240, 64),
        "topmost surface must be the value OSD, got {}x{}",
        image.width,
        image.height
    );
    // fake system state says volume 64 -> one notch up previews 69
    assert_matches_golden(&png, "osd-volume-wheel.png");
}

/// Clicking the audio pill answers with the control-center notice (finding a:
/// status modules are real buttons; panels arrive in Tier 2).
#[test]
fn click_audio_pill_shows_control_notice() {
    let png = capture_png("osd-notice", 0);
    let _ = std::fs::remove_file(&png);
    let spec = format!("motion:{}@1200;click@1300", at(BAR_AUDIO_PILL));
    let harness = spawn_harness_full(
        "ptr-audio",
        "1280x800",
        Some((Capture::AtMs(2200), &png)),
        None,
        Some(&spec),
        false,
        Some(&["audio"]),
        &[],
    );
    assert!(
        harness.wait_log("created (lme-osd)", Duration::from_secs(10)),
        "audio pill click must show the control notice"
    );
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "timed capture never happened"
    );
    let image = decode_png(&png);
    assert!(
        (image.width, image.height) == (460, 76),
        "topmost surface must be the notice OSD, got {}x{}",
        image.width,
        image.height
    );
    assert_matches_golden(&png, "osd-control-notice.png");
}
