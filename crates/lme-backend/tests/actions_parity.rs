//! M7: action parity. Drive the Python daemon and the Rust daemon with the
//! same action sequence in the same fixture environment; every lastResult,
//! every subprocess call and the niri socket action bytes must match.

mod common;

use common::{python3, repo_root, Fixture};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const RUST: &str = env!("CARGO_BIN_EXE_lme-backend");

struct Daemon {
    child: Child,
    receiver: std::sync::mpsc::Receiver<String>,
    last_sequence: i64,
}

impl Daemon {
    fn start(program: &str, args: &[String], fixture: &Fixture, socket: &std::path::Path) -> Self {
        let mut child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(fixture.env())
            .env("NIRI_SOCKET", socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if std::env::var("LME_TEST_SHOW_STDERR").is_ok() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .expect("spawn daemon");
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().take_while(Result::is_ok).flatten() {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        // -1 so the initial "ready" ack (sequence 0) passes the freshness check.
        let mut daemon = Self {
            child,
            receiver,
            last_sequence: -1,
        };
        daemon.read_snapshot(Duration::from_secs(15)); // initial snapshot
        daemon
    }

    fn read_snapshot(&mut self, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(line) = self.receiver.recv_timeout(remaining).ok() else {
                panic!("timed out waiting for a fresh lastResult");
            };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(sequence) = value["lastResult"]["sequence"].as_i64() else {
                continue;
            };
            if sequence > self.last_sequence {
                self.last_sequence = sequence;
                return value;
            }
        }
    }

    fn send(&mut self, request: &Value) -> Value {
        let mut stdin = self.child.stdin.take().unwrap();
        let write = stdin
            .write_all(format!("{}\n", request).as_bytes())
            .and_then(|_| stdin.flush());
        let _ = self.child.stdin.insert(stdin);
        write.expect("write action");
        self.read_snapshot(Duration::from_secs(20))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

/// Mock niri socket: state requests answer with the same canned data as
/// bench/stubs/niri (so the socket path and the subprocess path agree),
/// actions are acknowledged and their bytes recorded.
fn spawn_mock_niri() -> (PathBuf, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let directory = std::env::temp_dir().join(format!("lme-actions-mock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("niri.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let recorded: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let log = std::sync::Arc::clone(&recorded);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = std::sync::Arc::clone(&log);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|count| count > 0) {
                    let request = line.trim().to_string();
                    let reply = match serde_json::from_str::<Value>(&request) {
                        Ok(Value::String(request)) => match request.as_str() {
                            "Workspaces" => json!({"Ok": [
                                {"id": 1, "idx": 1, "name": null, "output": "eDP-1", "is_focused": true, "is_active": true},
                                {"id": 2, "idx": 2, "name": null, "output": "eDP-1", "is_focused": false, "is_active": false},
                            ]}),
                            "Windows" => json!({"Ok": [
                                {"id": 10, "title": "Firefox", "app_id": "firefox", "workspace_id": 1, "is_focused": true},
                                {"id": 11, "title": "kitty", "app_id": "kitty", "workspace_id": 2, "is_focused": false},
                            ]}),
                            _ => json!({"Ok": {"WorkspacesChanged": {}}}),
                        },
                        _ => {
                            log.lock().unwrap().push(request);
                            json!({"Ok": null})
                        }
                    };
                    let _ = reader.get_mut().write_all(format!("{reply}\n").as_bytes());
                    line.clear();
                }
            });
        }
    });
    (path, recorded)
}

/// True for probe traffic whose per-phase count depends on daemon speed:
/// niri state polls (subprocess in Python, socket in Rust), the Python
/// event-stream child, and the cached hardware probes behind the 1.5 s
/// window. Probe output itself is pinned by the snapshot parity suite.
fn is_probe(line: &str) -> bool {
    line.starts_with("niri msg --json ")
        || line == "nmcli radio wifi"
        || line.starts_with("nmcli -t ")
        || line.starts_with("busctl --system --json=short")
        || line.starts_with("busctl --system get-property")
        || line == "bluetoothctl show"
        || line.starts_with("wpctl get-volume ")
        || line == "brightnessctl --class=backlight -m"
        || line == "powerprofilesctl get"
}

/// Drop probe lines and mask the fixture's absolute root so logs from the
/// two per-backend fixture directories compare equal.
fn normalize_log(lines: Vec<String>, fixture: &Fixture) -> Vec<String> {
    let root = fixture.root.display().to_string();
    lines
        .into_iter()
        .filter(|line| !is_probe(line))
        .map(|line| line.replace(&root, "<FIXTURE>"))
        .collect()
}

#[test]
#[ignore = "upstream drift: lock is an async feature job in the current Python (see docs/UPSTREAM_SYNC.md and STATE.md); lme-backend implements the Phase 0 frozen contract"]
fn action_sequence_parity() {
    let Some(python) = python3() else {
        eprintln!("python3 unavailable; skipping action parity");
        return;
    };
    let (socket, recorded) = spawn_mock_niri();

    let actions: Vec<Value> = vec![
        json!({"action": "volume", "value": 45}),
        json!({"action": "mute"}),
        json!({"action": "micVolume", "value": 60}),
        json!({"action": "micMute"}),
        json!({"action": "brightness", "value": 50}),
        json!({"action": "wifiToggle"}),
        json!({"action": "powerProfile", "value": "balanced"}),
        json!({"action": "nativeOverview"}),
        json!({"action": "focusWindow", "id": 10}),
        json!({"action": "workspace", "id": 2}),
        json!({"action": "launch", "id": "alpha.desktop"}),
        json!({"action": "setting", "key": "barFontSize", "value": 15}),
        json!({"action": "setting", "key": "opacity", "value": 0.9}),
        json!({"action": "weatherSelect", "location": {"name": "Hue", "country": "VN", "latitude": 16.46, "longitude": 107.6}}),
        json!({"action": "openSettings", "target": "audio"}),
        json!({"action": "lock"}),
        json!({"action": "screenshot"}),
        json!({"action": "moveWindow", "id": 10, "workspaceId": 2}),
        json!({"action": "refresh"}),
        json!({"action": "chargeMode", "value": 85}),
        json!({"action": "volume", "value": 150}),
        json!({"action": "bogus"}),
    ];

    let mut results: Vec<(Value, Value)> = Vec::new();

    // A 60 s poll interval keeps timer-driven snapshots out of the phase:
    // every emitted line then corresponds to exactly one input, so ack
    // ordering and sequence numbers compare deterministically, and the
    // charge background job never folds its result mid-phase.
    let python_args = vec![
        repo_root()
            .join("legacy/scripts/desktop_backend.py")
            .to_string_lossy()
            .to_string(),
        "--interval".to_string(),
        "60".to_string(),
    ];
    let rust_args = vec!["--interval".to_string(), "60".to_string()];
    let (python_log, python_socket) = {
        let fixture = Fixture::new("actions-python");
        fixture.reset_log();
        recorded.lock().unwrap().clear();
        let mut daemon = Daemon::start(&python.to_string_lossy(), &python_args, &fixture, &socket);
        for (index, action) in actions.iter().enumerate() {
            eprintln!("[python] action {index}: {action}");
            let response = daemon.send(action);
            eprintln!("[python]   -> {}", response["lastResult"]["message"]);
            results.push((response["lastResult"].clone(), Value::Null));
        }
        let log = normalize_log(fixture.log_lines(), &fixture);
        let socket_actions = recorded.lock().unwrap().clone();
        eprintln!("[harness] python phase complete, dropping daemon");
        (log, socket_actions)
    };
    let (rust_log, rust_socket) = {
        let fixture = Fixture::new("actions-rust");
        fixture.reset_log();
        recorded.lock().unwrap().clear();
        let mut daemon = Daemon::start(RUST, &rust_args, &fixture, &socket);
        for (index, action) in actions.iter().enumerate() {
            eprintln!("[rust] action {index}: {action}");
            let response = daemon.send(action);
            eprintln!("[rust]   -> {}", response["lastResult"]["message"]);
            results[index].1 = response["lastResult"].clone();
        }
        let log = normalize_log(fixture.log_lines(), &fixture);
        let socket_actions = recorded.lock().unwrap().clone();
        eprintln!("[harness] rust phase complete, dropping daemon");
        (log, socket_actions)
    };

    eprintln!("[harness] comparing results");
    for (index, (python_result, rust_result)) in results.iter().enumerate() {
        assert_eq!(
            python_result, rust_result,
            "action {} ({}) lastResult differs\npython: {python_result}\nrust:   {rust_result}",
            index, actions[index]
        );
    }

    // The hand-shaped socket action must be byte-identical on both backends.
    assert_eq!(
        python_socket, rust_socket,
        "niri socket action bytes differ between backends"
    );

    // Subprocess call sequences must match exactly (tools + arguments)
    // after dropping timing-dependent probe traffic (see is_probe).
    assert_eq!(
        python_log, rust_log,
        "subprocess sequences differ after action replay"
    );
}

#[test]
fn charge_mode_pending_then_completed_parity() {
    let Some(python) = python3() else {
        eprintln!("python3 unavailable; skipping charge parity");
        return;
    };

    // A writable threshold-style battery: available, writable, no pkexec —
    // the background job writes the threshold and verifies it.
    let make_sysfs = |name: &str| -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("lme-charge-parity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let battery = root.join("class/power_supply/BAT0");
        std::fs::create_dir_all(&battery).unwrap();
        std::fs::write(battery.join("type"), "Battery\n").unwrap();
        std::fs::write(battery.join("charge_control_end_threshold"), "100\n").unwrap();
        std::fs::write(battery.join("charge_control_start_threshold"), "0\n").unwrap();
        root
    };

    // Python phase: drive desktop_backend.DesktopBackend directly against a
    // fake sysfs and print both results as one JSON line.
    let python_sysfs = make_sysfs("python");
    let fixture = Fixture::new("charge-python");
    let tree = common::python_tree();
    let driver = format!(
        r#"
import json, sys, time
from pathlib import Path
sys.path.insert(0, {scripts:?})
from desktop_backend import DesktopBackend
backend = DesktopBackend(proc=Path("/proc"), sysfs=Path({sysfs:?}), weather_fetch=False)
pending = backend.handle({{"action": "chargeMode", "value": 85}})
for _ in range(500):
    state = backend.snapshot()
    result = state["lastResult"]
    if result.get("action") == "chargeMode" and result.get("pending") is not True:
        break
    time.sleep(0.02)
else:
    raise SystemExit("charge result never folded")
print(json.dumps({{"pending": pending, "completed": result}}))
"#,
        scripts = tree.join("scripts").display().to_string(),
        sysfs = python_sysfs.display().to_string(),
    );
    let output = Command::new(&python)
        .arg("-c")
        .arg(&driver)
        .env_clear()
        .envs(fixture.env())
        .output()
        .expect("run python driver");
    assert!(
        output.status.success(),
        "python driver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let python_results: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let _ = std::fs::remove_dir_all(&python_sysfs);

    // Rust phase: the same flow through the library API.
    let rust_sysfs = make_sysfs("rust");
    let fixture = Fixture::new("charge-rust");
    let mut backend = lme_backend::backend::DesktopBackend::new(
        false,
        false,
        Some(fixture.root.join("home/.config/lme-shell/settings.json")),
        Some(Vec::new()),
        PathBuf::from("/proc"),
        rust_sysfs.clone(),
        false,
        true,
        None,
    );
    let pending = backend.handle(json!({"action": "chargeMode", "value": 85}));
    let deadline = Instant::now() + Duration::from_secs(10);
    let completed = loop {
        let snapshot = backend.snapshot();
        let result = snapshot["lastResult"].clone();
        if result["action"] == json!("chargeMode") && result["pending"] != json!(true) {
            break result;
        }
        assert!(
            Instant::now() < deadline,
            "charge result never folded; last: {result}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = std::fs::remove_dir_all(&rust_sysfs);

    assert_eq!(
        pending["pending"],
        json!(true),
        "must acknowledge as pending"
    );
    assert_eq!(pending, python_results["pending"], "pending ack differs");
    assert_eq!(
        completed, python_results["completed"],
        "completed charge result differs"
    );
    assert_eq!(
        completed["ok"],
        json!(true),
        "the writable fake sysfs must succeed"
    );
    assert_eq!(
        completed["message"],
        json!("Charge mode updated · 85%"),
        "the threshold write must be verified and reported"
    );
}
