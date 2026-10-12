//! M3: niri IPC over the unix socket — mock-server test. The socket path
//! must produce the same `niri` snapshot shape as the subprocess probe, and
//! a dead socket must fall back to the subprocess (stub) probe.

mod common;

use common::Fixture;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RUST: &str = env!("CARGO_BIN_EXE_lme-backend");

struct MockNiri {
    path: PathBuf,
    actions: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockNiri {
    /// Serve the niri newline-JSON protocol: one request line in, one
    /// reply line out. Workspaces/Windows replies use canned data; any
    /// other request is recorded as an action and acknowledged.
    fn spawn(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("lme-niri-mock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("niri.sock");
        let listener = UnixListener::bind(&path).expect("bind mock niri socket");
        let actions: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let recorded = std::sync::Arc::clone(&actions);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = std::sync::Arc::clone(&recorded);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|count| count > 0) {
                        let request = line.trim().to_string();
                        let reply = mock_reply(&request, &recorded);
                        let _ = reader.get_mut().write_all(format!("{reply}\n").as_bytes());
                        line.clear();
                    }
                });
            }
        });
        Self { path, actions }
    }

    fn workspaces() -> Value {
        json!([
            {"id": 5, "idx": 2, "name": null, "output": "eDP-1", "is_focused": false, "is_active": false},
            {"id": 4, "idx": 1, "name": null, "output": "eDP-1", "is_focused": true, "is_active": true},
            {"id": 9, "idx": 1, "name": null, "output": "HDMI-1", "is_focused": false, "is_active": false},
        ])
    }

    fn windows() -> Value {
        json!([
            {"id": 30, "title": "Socket Window", "app_id": "app", "workspace_id": 4, "is_focused": true},
        ])
    }
}

fn mock_reply(request: &str, recorded: &std::sync::Mutex<Vec<String>>) -> String {
    let parsed: Value = serde_json::from_str(request).unwrap_or(Value::Null);
    match parsed.as_str() {
        Some("Workspaces") => json!({"Ok": MockNiri::workspaces()}).to_string(),
        Some("Windows") => json!({"Ok": MockNiri::windows()}).to_string(),
        Some("EventStream") => {
            // Emit one event immediately, then keep the stream open.
            json!({"Ok": {"WorkspacesChanged": {}}}).to_string()
        }
        _ => {
            recorded.lock().unwrap().push(request.to_string());
            json!({"Ok": null}).to_string()
        }
    }
}

fn snapshot_with_socket(fixture: &Fixture, socket: Option<&std::path::Path>) -> Value {
    let mut command = std::process::Command::new(RUST);
    command.arg("--once");
    command.env_clear().envs(fixture.env());
    if let Some(socket) = socket {
        // Must come after env_clear()/envs(): fixture.env() strips NIRI_SOCKET.
        command.env("NIRI_SOCKET", socket);
    }
    let output = command.output().expect("spawn backend");
    assert!(output.status.success());
    let line = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn socket_state_matches_subprocess_shape() {
    crate::require_legacy_reference!();
    let mock = MockNiri::spawn("state");
    let fixture = Fixture::new("niri-socket");
    fixture.reset_log();
    let snapshot = snapshot_with_socket(&fixture, Some(&mock.path));
    let niri = &snapshot["niri"];
    assert_eq!(niri["available"], json!(true));
    // Sorted by (output, idx, id) with byte-wise string ordering, exactly
    // like Python's tuple sort: "HDMI-1" < "eDP-1" because 'H' < 'e'.
    let ids: Vec<Value> = niri["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!(9), json!(4), json!(5)]);
    assert_eq!(niri["windows"][0]["title"], json!("Socket Window"));
    // The socket path must not shell out to `niri msg` at all.
    assert!(
        !fixture
            .log_lines()
            .iter()
            .any(|call| call.starts_with("niri")),
        "socket mode must not spawn niri subprocesses: {:?}",
        fixture.log_lines()
    );
    // The subprocess path (no socket) sees the stub's data instead.
    let subprocess = snapshot_with_socket(&fixture, None);
    assert_eq!(
        subprocess["niri"]["workspaces"].as_array().unwrap().len(),
        2
    );
    assert!(fixture
        .log_lines()
        .iter()
        .any(|call| call.starts_with("niri msg --json workspaces")));
}

#[test]
fn dead_socket_falls_back_to_subprocess() {
    crate::require_legacy_reference!();
    let fixture = Fixture::new("niri-fallback");
    let dead = fixture.root.join("no-such.sock");
    fixture.reset_log();
    let snapshot = snapshot_with_socket(&fixture, Some(&dead));
    assert_eq!(snapshot["niri"]["available"], json!(true));
    assert!(
        fixture
            .log_lines()
            .iter()
            .any(|call| call.starts_with("niri msg --json workspaces")),
        "dead socket must fall back to the subprocess probe"
    );
}

#[test]
fn socket_action_round_trip() {
    let mock = MockNiri::spawn("action");
    let fixture = Fixture::new("niri-action");
    fixture.reset_log();
    let mut child = Command::new(RUST)
        .args(["--interval", "60"])
        .env_clear()
        .envs(fixture.env())
        .env("NIRI_SOCKET", &mock.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
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
    let first = receiver
        .recv_timeout(Duration::from_secs(15))
        .expect("initial snapshot");
    let first: Value = serde_json::from_str(first.trim()).unwrap();
    assert_eq!(first["lastResult"]["action"], "ready");

    // moveWindow must reach the socket as the exact hand-shaped action,
    // without shelling out to `niri msg`.
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(br#"{"action":"moveWindow","id":30,"workspaceId":4}"#)
        .and_then(|_| stdin.write_all(b"\n"))
        .unwrap();
    stdin.flush().unwrap();
    let _ = child.stdin.insert(stdin);

    // The event-stream trigger may emit extra snapshots; wait for the ack.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ack = None;
    while Instant::now() < deadline {
        let Ok(line) = receiver.recv_timeout(Duration::from_millis(500)) else {
            break;
        };
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        if value["lastResult"]["action"] == json!("moveWindow") {
            ack = Some(value);
            break;
        }
    }
    let ack = ack.expect("moveWindow ack");
    assert_eq!(ack["lastResult"]["ok"], json!(true));
    assert_eq!(
        ack["lastResult"]["message"],
        json!("Window moved to workspace")
    );
    assert!(
        !fixture
            .log_lines()
            .iter()
            .any(|call| call.starts_with("niri")),
        "socket mode must not spawn niri subprocesses: {:?}",
        fixture.log_lines()
    );
    let recorded = mock.actions.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        1,
        "unexpected socket requests: {recorded:?}"
    );
    let sent: Value = serde_json::from_str(&recorded[0]).unwrap();
    assert_eq!(
        sent,
        json!({"Action": {"MoveWindowToWorkspace": {"window_id": 30, "reference": {"Id": 4}, "focus": false}}})
    );

    drop(child.stdin.take());
    let _ = child.wait();
}
