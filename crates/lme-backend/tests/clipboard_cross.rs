//! M6: clipboard history cross-compatibility. The Python service and the
//! Rust service share `history.sqlite3` on disk; both directions must work:
//! Python-captured entries readable by Rust and vice versa, plus daemon
//! protocol parity for search/pin/delete/clear/pause.

mod common;

use common::python3;
use lme_backend::clipboard::History;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const RUST: &str = env!("CARGO_BIN_EXE_lme-backend");

fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("lme-clip-cross-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn python_capture(dir: &Path, kind: &str, payload: &[u8]) {
    let python = python3().expect("python3 available");
    let mut child = Command::new(python)
        .arg(common::python_tree().join("scripts/clipboard.py"))
        .args(["--capture", kind, "--directory"])
        .arg(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn python capture");
    child.stdin.take().unwrap().write_all(payload).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
}

fn python_daemon_lines(dir: &Path, requests: &[String]) -> Vec<Value> {
    let python = python3().expect("python3 available");
    let mut child = Command::new(python)
        .arg(common::python_tree().join("scripts/clipboard.py"))
        .arg("--directory")
        .arg(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn python clipboard daemon");
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    drop(stdin);
    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let mut lines = Vec::new();
    for line in reader.lines().take_while(Result::is_ok).flatten() {
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            lines.push(value);
        }
    }
    let _ = child.wait();
    lines
}

fn rust_daemon_lines(dir: &Path, requests: &[String]) -> Vec<Value> {
    let mut child = Command::new(RUST)
        .arg("--clipboard")
        .arg("--directory")
        .arg(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rust clipboard daemon");
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    drop(stdin);
    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let mut lines = Vec::new();
    for line in reader.lines().take_while(Result::is_ok).flatten() {
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            lines.push(value);
        }
    }
    let _ = child.wait();
    lines
}

fn first_state(lines: &[Value]) -> &Value {
    lines
        .iter()
        .find(|line| line.get("entries").is_some())
        .expect("daemon emitted a state line")
}

#[test]
fn python_capture_is_readable_by_rust() {
    let dir = directory("py2rs");
    python_capture(&dir, "text", b"captured by python");
    let history = History::new(&dir, 100, lme_backend::clipboard::MAX_TOTAL).unwrap();
    let entries = history.entries("", "all").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["preview"], json!("captured by python"));
    assert_eq!(entries[0]["kind"], json!("text"));
    assert_eq!(history.total(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rust_store_is_readable_by_python() {
    let dir = directory("rs2py");
    let mut history = History::new(&dir, 100, lme_backend::clipboard::MAX_TOTAL).unwrap();
    history.store("text", b"stored by rust").unwrap().unwrap();
    history
        .store("image", b"\x89PNG\r\n\x1a\nfakepng")
        .unwrap()
        .unwrap();
    let lines = python_daemon_lines(
        &dir,
        &[json!({"action": "search", "query": "", "kind": "all"}).to_string()],
    );
    let state = first_state(&lines);
    let entries = state["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "python sees both rust entries: {state}");
    assert!(entries
        .iter()
        .any(|entry| entry["preview"] == json!("stored by rust")));
    assert!(entries.iter().any(|entry| entry["kind"] == json!("image")));
    assert!(entries.iter().any(|entry| entry["image"]
        .as_str()
        .is_some_and(|uri| uri.ends_with(".png"))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn daemon_protocol_parity() {
    let requests: Vec<String> = vec![
        json!({"action": "search", "query": "clip", "kind": "all"}).to_string(),
        json!({"action": "search", "query": "clip", "kind": "text"}).to_string(),
        json!({"action": "search", "query": "", "kind": "bogus"}).to_string(),
        json!({"action": "pin", "id": "short"}).to_string(),
    ];
    let python_lines = {
        let dir = directory("protocol-py");
        python_capture(&dir, "text", b"clip text one");
        let lines = python_daemon_lines(&dir, &requests);
        let _ = std::fs::remove_dir_all(&dir);
        lines
    };
    let rust_lines = {
        let dir = directory("protocol-rs");
        python_capture(&dir, "text", b"clip text one");
        let lines = rust_daemon_lines(&dir, &requests);
        let _ = std::fs::remove_dir_all(&dir);
        lines
    };
    // Both daemons emit an initial state before any request.
    first_state(&python_lines);
    first_state(&rust_lines);
    // Every request must produce the same (action, ok, message) sequence,
    // including the invalid-kind search reset and the unknown-id pin error.
    let signature = |lines: &[Value]| -> Vec<String> {
        lines
            .iter()
            .filter_map(|line| line.get("lastResult").filter(|result| !result.is_null()))
            .map(|result| {
                format!(
                    "{} ok={} msg={}",
                    result["action"].as_str().unwrap_or_default(),
                    result["ok"].as_bool().unwrap_or(false),
                    result["message"].as_str().unwrap_or_default()
                )
            })
            .collect()
    };
    assert_eq!(
        signature(&python_lines),
        signature(&rust_lines),
        "clipboard daemon results diverge between python and rust"
    );
}

#[test]
fn rust_daemon_full_cycle() {
    let dir = directory("rust-cycle");
    let requests: Vec<String> = vec![
        json!({"action": "search", "query": "", "kind": "all"}).to_string(),
        json!({"action": "pause", "value": true}).to_string(),
        json!({"action": "pause", "value": false}).to_string(),
        json!({"action": "clear", "includePinned": true}).to_string(),
        json!({"action": "copy", "id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}).to_string(),
        json!({"action": "unknown"}).to_string(),
    ];
    let mut child = Command::new(RUST)
        .args(["--clipboard", "--directory"])
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in &requests {
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        std::thread::sleep(Duration::from_millis(120));
    }
    drop(stdin);
    let stdout = child.stdout.take().unwrap();
    let states: Vec<Value> = BufReader::new(stdout)
        .lines()
        .take_while(Result::is_ok)
        .flatten()
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    let _ = child.wait();
    assert!(!states.is_empty(), "daemon emitted states");
    let results: Vec<&Value> = states
        .iter()
        .filter_map(|state| state.get("lastResult").filter(|r| !r.is_null()))
        .collect();
    let actions: Vec<&str> = results
        .iter()
        .map(|result| result["action"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        actions,
        vec!["search", "pause", "pause", "clear", "copy", "unknown"]
    );
    assert_eq!(results[3]["ok"], json!(true));
    assert_eq!(results[4]["ok"], json!(false), "unknown id copy must fail");
    assert_eq!(
        results[4]["message"],
        json!("This clipboard item is no longer available")
    );
    assert_eq!(results[5]["ok"], json!(false));
    assert_eq!(results[5]["message"], json!("Unknown clipboard action"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_mode_stores_once() {
    let dir = directory("capture");
    let payload = b"one-shot capture";
    let status = Command::new(RUST)
        .args(["--capture", "text", "--directory"])
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().unwrap().write_all(payload).unwrap();
            child.wait()
        })
        .unwrap();
    assert!(status.success());
    let history = History::new(&dir, 100, lme_backend::clipboard::MAX_TOTAL).unwrap();
    let entries = history.entries("", "all").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["preview"], json!("one-shot capture"));
    let _ = std::fs::remove_dir_all(&dir);
}
