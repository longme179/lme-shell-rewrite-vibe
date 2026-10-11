//! The daemon main loop: stdin JSON lines (64 KiB cap), niri event
//! subscription with the 100 ms debounce, and the polling interval —
//! behaviour-identical to the Python selector loop.

use crate::backend::DesktopBackend;
use crate::niri::NiriEventStream;
use serde_json::Value;
use std::io::{BufRead, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const INPUT_LIMIT: usize = 64 * 1024;
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);

enum Event {
    Line(String),
    Oversize,
    Eof,
    Niri,
}

fn stdin_reader(sender: Sender<Event>) {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut buffer = [0u8; 65536];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match handle.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(Event::Eof);
                return;
            }
            Ok(count) => {
                pending.extend_from_slice(&buffer[..count]);
                if pending.len() > INPUT_LIMIT {
                    // Same as the Python reset: drop the buffer, report once.
                    pending.clear();
                    if sender.send(Event::Oversize).is_err() {
                        return;
                    }
                    continue;
                }
                while let Some(position) = pending.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = pending.drain(..=position).collect();
                    let line = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
                    if sender.send(Event::Line(line)).is_err() {
                        return;
                    }
                }
            }
            Err(_) => {
                let _ = sender.send(Event::Eof);
                return;
            }
        }
    }
}

fn niri_subprocess_reader(stdout: std::process::ChildStdout, sender: Sender<Event>) {
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if sender.send(Event::Niri).is_err() {
                    return;
                }
            }
        }
    }
}

pub fn run(backend: &mut DesktopBackend, interval: f64) {
    let interval = Duration::from_secs_f64(interval.max(0.5));
    let (sender, receiver) = std::sync::mpsc::channel::<Event>();
    std::thread::Builder::new()
        .name("lme-stdin".to_string())
        .spawn({
            let sender = sender.clone();
            move || stdin_reader(sender)
        })
        .expect("stdin reader thread");

    let mut niri_child: Option<Child> = None;
    let mut niri_stream: Option<NiriEventStream> = None;
    if !backend.demo && backend.niri["available"].as_bool().unwrap_or(false) {
        if std::env::var_os("NIRI_SOCKET").is_some() {
            let event_sender = sender.clone();
            niri_stream = NiriEventStream::connect(move || {
                let _ = event_sender.send(Event::Niri);
            });
        } else if let Ok(mut child) = Command::new(
            backend
                .probes
                .runner
                .executable("niri")
                .cloned()
                .unwrap_or_else(|_| "niri".into()),
        )
        .args(["msg", "--json", "event-stream"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        {
            if let Some(stdout) = child.stdout.take() {
                let stream_sender = sender.clone();
                std::thread::Builder::new()
                    .name("lme-niri-events".to_string())
                    .spawn(move || niri_subprocess_reader(stdout, stream_sender))
                    .expect("niri event reader thread");
                niri_child = Some(child);
            }
        }
    }

    let mut next_poll = Instant::now() + interval;
    let mut event_refresh_due: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let deadline = event_refresh_due.unwrap_or(next_poll).min(next_poll);
        let timeout = deadline.saturating_duration_since(now);
        match receiver.recv_timeout(timeout) {
            Ok(Event::Eof) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Event::Oversize) => {
                backend.invalid_input("Input exceeds 64 KiB limit");
                if crate::emit(&backend.snapshot()).is_err() {
                    break;
                }
                next_poll = Instant::now() + interval;
                event_refresh_due = None;
            }
            Ok(Event::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(request) => {
                        backend.handle(request);
                    }
                    Err(_) => backend.invalid_input("Invalid JSON input"),
                }
                if crate::emit(&backend.snapshot()).is_err() {
                    break;
                }
                next_poll = Instant::now() + interval;
                event_refresh_due = None;
            }
            Ok(Event::Niri) => {
                if event_refresh_due.is_none() {
                    event_refresh_due = Some(Instant::now() + EVENT_DEBOUNCE);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if now >= next_poll || event_refresh_due.is_some_and(|due| now >= due) {
            if crate::emit(&backend.snapshot()).is_err() {
                break;
            }
            next_poll = Instant::now() + interval;
            event_refresh_due = None;
        }
    }
    // Teardown mirrors the Python `finally` block.
    if let Some(mut stream) = niri_stream.take() {
        stream.stop();
    }
    if let Some(mut child) = niri_child.take() {
        terminate_child(&mut child);
    }
    backend.weather.close();
}

fn terminate_child(child: &mut Child) {
    crate::clipboard::terminate_process(child);
}
