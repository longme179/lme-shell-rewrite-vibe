//! M7: daemon event-loop behaviour — ack-per-input ordering, malformed and
//! oversized input handling, blank-line silence, polling cadence, clean
//! exit on stdin EOF, and the niri event-stream child.

mod common;

use common::Fixture;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RUST: &str = env!("CARGO_BIN_EXE_lme-backend");

struct Daemon {
    child: std::process::Child,
    receiver: std::sync::mpsc::Receiver<String>,
}

impl Daemon {
    fn start(fixture: &Fixture, interval: &str) -> Self {
        let mut child = Command::new(RUST)
            .args(["--interval", interval])
            .env_clear()
            .envs(fixture.env())
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
        Self { child, receiver }
    }

    fn read_line(&mut self, timeout: Duration) -> Option<String> {
        self.receiver.recv_timeout(timeout).ok()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
fn loop_acknowledges_every_input_line() {
    crate::require_legacy_reference!();
    let fixture = Fixture::new("loop-ack");
    let mut daemon = Daemon::start(&fixture, "60");
    let first = daemon
        .read_line(Duration::from_secs(15))
        .expect("initial snapshot");
    let snapshot: Value = serde_json::from_str(first.trim()).unwrap();
    assert_eq!(snapshot["lastResult"]["action"], "ready");

    let mut stdin = daemon.child.stdin.take().unwrap();
    // Two actions in one write must each produce their own snapshot ack.
    let burst = format!(
        "{}\n{}\n",
        serde_json::json!({"action": "volume", "value": 40}),
        serde_json::json!({"action": "mute"}),
    );
    stdin.write_all(burst.as_bytes()).unwrap();
    stdin.flush().unwrap();
    let stdin = daemon.child.stdin.insert(stdin);
    let _ = stdin;

    let mut daemon = daemon;
    let first_ack = daemon.read_line(Duration::from_secs(10)).unwrap();
    let second_ack = daemon.read_line(Duration::from_secs(10)).unwrap();
    let first_ack: Value = serde_json::from_str(first_ack.trim()).unwrap();
    let second_ack: Value = serde_json::from_str(second_ack.trim()).unwrap();
    assert_eq!(first_ack["lastResult"]["action"], "volume");
    assert_eq!(first_ack["lastResult"]["message"], "Volume 40%");
    assert_eq!(second_ack["lastResult"]["action"], "mute");
    assert_eq!(
        second_ack["lastResult"]["sequence"],
        first_ack["lastResult"]["sequence"].as_i64().unwrap() + 1
    );

    // Malformed JSON reports the contract error and keeps running.
    let mut stdin = daemon.child.stdin.take().unwrap();
    stdin.write_all(b"this is not json\n").unwrap();
    stdin.flush().unwrap();
    let _ = daemon.child.stdin.insert(stdin);
    let bad = daemon.read_line(Duration::from_secs(10)).unwrap();
    let bad: Value = serde_json::from_str(bad.trim()).unwrap();
    assert_eq!(bad["lastResult"]["ok"], false);
    assert_eq!(bad["lastResult"]["message"], "Invalid JSON input");
}

#[test]
fn loop_rejects_oversized_input() {
    crate::require_legacy_reference!();
    let fixture = Fixture::new("loop-oversize");
    let mut daemon = Daemon::start(&fixture, "60");
    let _ = daemon.read_line(Duration::from_secs(15));
    let mut stdin = daemon.child.stdin.take().unwrap();
    let oversize = "x".repeat(70 * 1024);
    stdin.write_all(oversize.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
    let _ = daemon.child.stdin.insert(stdin);
    let response = daemon
        .read_line(Duration::from_secs(10))
        .expect("oversize ack");
    let response: Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(
        response["lastResult"]["message"],
        "Input exceeds 64 KiB limit"
    );
}

#[test]
fn loop_polls_on_interval_and_exits_cleanly() {
    crate::require_legacy_reference!();
    let fixture = Fixture::new("loop-poll");
    let mut daemon = Daemon::start(&fixture, "0.5");
    let _ = daemon.read_line(Duration::from_secs(15));
    // Idle for ~1.7s: expect roughly 3 poll snapshots (0.5s cadence).
    let start = Instant::now();
    let mut polls = 0;
    while start.elapsed() < Duration::from_millis(1700) {
        if daemon.read_line(Duration::from_millis(300)).is_some() {
            polls += 1;
        }
    }
    assert!(polls >= 2, "expected periodic polls, saw {polls}");
    // The niri event-stream child is running (subprocess fallback mode).
    assert!(fixture
        .log_lines()
        .iter()
        .any(|call| call.contains("niri msg --json event-stream")));
    // Blank lines produce no output: sync just after a poll, then stay
    // silent for a window shorter than the poll interval.
    let _ = daemon.read_line(Duration::from_secs(2));
    let mut stdin = daemon.child.stdin.take().unwrap();
    stdin.write_all(b"\n\n").unwrap();
    stdin.flush().unwrap();
    let _ = daemon.child.stdin.insert(stdin);
    let silent = daemon.read_line(Duration::from_millis(250));
    assert!(silent.is_none(), "blank lines must not emit: {silent:?}");
    // EOF: clean exit.
    drop(daemon.child.stdin.take());
    let status = daemon.child.wait().unwrap();
    assert!(status.success(), "daemon must exit cleanly on stdin EOF");
}
