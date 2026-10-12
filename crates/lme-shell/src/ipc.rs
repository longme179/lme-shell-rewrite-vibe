// SPDX-License-Identifier: GPL-3.0-or-later
//! IPC: `$XDG_RUNTIME_DIR/lme-shell.sock`, one JSON object per line per
//! connection, one JSON reply line (ADR 0002). `lme-shell msg` is the client.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

pub fn socket_path() -> std::path::PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return std::path::PathBuf::from(runtime).join("lme-shell.sock");
    }
    let uid = rustix::process::getuid().as_raw();
    std::path::PathBuf::from(format!("/tmp/lme-shell-{uid}.sock"))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "cmd", content = "args", rename_all = "camelCase")]
pub enum Command {
    /// toggle <panel>: launcher|power|control|notifications|clipboard|overview|
    /// activity|tray|wifi|settings (legacy Quickshell IPC surface).
    Toggle {
        panel: String,
    },
    Close,
    Refresh,
    Status,
    /// osd <label> <value 0-100> — media-key style popup.
    Osd {
        label: String,
        value: f32,
    },
    /// notify <summary> [body] — local notification toast (Tier 1.9).
    Notify {
        summary: String,
        body: String,
    },
    /// dnd <minutes> — -1 until off, 0 off, >0 timed (legacy setDnd).
    Dnd {
        minutes: i64,
    },
    /// notifications — live + history + unread (center payload / testing).
    Notifications,
    /// status <message> — status toast (like QML lastResult messages).
    StatusMessage {
        message: String,
    },
    /// run a backend action (dry-run safe subset; power actions).
    Power {
        action: String,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl Reply {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            detail: None,
        }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            detail: None,
        }
    }
}

/// Client used by `lme-shell msg` (and tests).
pub fn send(cmd: &Command) -> Result<Reply, String> {
    let path = socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("connect {}: {e}", path.display()))?;
    let payload = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    stream
        .write_all(payload.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read reply: {e}"))?;
    if line.trim().is_empty() {
        return Err("daemon closed without a reply".into());
    }
    serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))
}

/// Handle one accepted connection on a worker thread: parse, execute via the
/// main-loop channel, relay the reply. Connections are tiny and one-shot.
pub fn serve_connection(
    mut stream: std::os::unix::net::UnixStream,
    to_main: calloop::channel::Sender<(Command, std_mpsc::Sender<Reply>)>,
) {
    std::thread::spawn(move || {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            return;
        }
        let Ok(cmd) = serde_json::from_str::<Command>(&line) else {
            let reply = Reply::err("Invalid JSON input");
            let _ = stream.write_all(serde_json::to_string(&reply).unwrap_or_default().as_bytes());
            let _ = stream.write_all(b"\n");
            return;
        };
        let (tx, rx) = std_mpsc::channel();
        if to_main.send((cmd, tx)).is_err() {
            return;
        }
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(reply) => {
                let _ =
                    stream.write_all(serde_json::to_string(&reply).unwrap_or_default().as_bytes());
                let _ = stream.write_all(b"\n");
            }
            Err(_) => {
                let reply = Reply::err("daemon timeout");
                let _ =
                    stream.write_all(serde_json::to_string(&reply).unwrap_or_default().as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }
    });
}
