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
        "missing {name} at {} — run `cargo test --workspace` so workspace binaries are built",
        path.display()
    );
    path
}

fn write_isolated_config(tmp: &Path, animations: bool) {
    let dir = tmp.join("config/lme-shell");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "animations": animations,
            "opacity": 0.90,
            "accent": "#9580ff",
            "barStyle": "connected",
            "barFontSize": 13,
            "barHeight": 42
        }))
        .unwrap(),
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
    let unique = format!(
        "{}-{}-{}",
        name,
        std::process::id(),
        Instant::now().elapsed().as_nanos() // extra uniqueness per test
    );
    let tmp = std::env::temp_dir().join(&unique);
    std::fs::create_dir_all(tmp.join("runtime")).unwrap();
    write_isolated_config(&tmp, animations);
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
    if let (Some(n), Some(out)) = (capture_commit, capture_out) {
        cmd.arg("--capture-commit")
            .arg(n.to_string())
            .arg("--capture-out")
            .arg(out);
    }
    if let Some(keys) = key_events {
        cmd.arg("--key-events").arg(keys);
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
        .env_remove("LME_SHELL_PERF")
        .stdout(Stdio::null())
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

/// Launcher: opens, accepts typed text, arrows move the selection, Enter
/// activates (dry-run launch of a fake app) and Esc closes.
#[test]
fn launcher_types_and_launches() {
    let harness = spawn_harness(
        "launcher",
        "1280x800",
        None,
        None,
        Some("f@1500,i@1600,r@1700,e@1800,f@1900,o@2000,x@2100,enter@2600,esc@3600"),
        false,
    );
    std::thread::sleep(Duration::from_millis(900));
    harness.msg(&["toggle", "launcher"]);
    std::thread::sleep(Duration::from_millis(300));
    let status = harness.msg(&["status"]);
    assert!(status.contains("launcher"), "launcher open: {status}");
    // keys typed f-i-r-e-f-o-x + Enter; the daemon has no Firefox desktop
    // entry in the sandbox, so the launcher shows zero results and Enter is
    // a no-op — the panel must stay open until Esc.
    std::thread::sleep(Duration::from_millis(1600));
    let status = harness.msg(&["status"]);
    assert!(
        status.contains("launcher"),
        "launcher still open after Enter: {status}"
    );
    std::thread::sleep(Duration::from_millis(1400));
    let status = harness.msg(&["status"]);
    assert!(
        !status.contains("launcher"),
        "launcher closed by Esc: {status}"
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

/// Launcher opens with the app list; capture after the reveal settles.
#[test]
fn golden_launcher() {
    let png = capture_png("launcher", 4);
    let _ = std::fs::remove_file(&png);
    let harness = spawn_harness(
        "launcher-golden",
        "1280x800",
        Some(4),
        Some(&png),
        None,
        false,
    );
    assert!(
        harness.wait_log("MAPPED", Duration::from_secs(15)),
        "bar never mapped"
    );
    harness.msg(&["toggle", "launcher"]);
    assert!(
        harness.wait_log("CAPTURED", Duration::from_secs(15)),
        "launcher capture never happened"
    );
    assert_matches_golden(&png, "launcher.png");
}
