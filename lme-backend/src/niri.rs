//! niri compositor state. Phase 1 M2 keeps the `niri msg --json`
//! subprocesses exactly like the Python backend; M3 adds the direct
//! `NIRI_SOCKET` path with the subprocess fallback.

use crate::runner::CommandRunner;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub fn unavailable() -> Value {
    json!({"available": false, "workspaces": [], "windows": []})
}

/// `niri_state()` port: subprocess probe, validation, workspace sorting.
pub fn niri_state_subprocess(runner: &CommandRunner) -> Value {
    let workspaces = runner
        .available("niri")
        .then(|| runner.run("niri", &["msg", "--json", "workspaces"]).ok())
        .flatten();
    let windows = runner
        .available("niri")
        .then(|| runner.run("niri", &["msg", "--json", "windows"]).ok())
        .flatten();
    let parse = |text: Option<String>| -> Option<Value> {
        let text = text?;
        let parsed: Value = serde_json::from_str(&text).ok()?;
        parsed
            .as_array()
            .cloned()
            .filter(|items| items.iter().all(|item| item.is_object()))
            .map(Value::Array)
    };
    match (parse(workspaces), parse(windows)) {
        (Some(mut workspaces), Some(windows)) => {
            workspaces
                .as_array_mut()
                .unwrap()
                .sort_by(workspace_comparator);
            json!({"available": true, "workspaces": workspaces, "windows": windows})
        }
        _ => unavailable(),
    }
}

/// One niri IPC request over `NIRI_SOCKET` (newline-delimited JSON).
pub fn niri_socket_request(request: &Value, timeout: Duration) -> Result<Value, String> {
    let path = std::env::var_os("NIRI_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "NIRI_SOCKET is unavailable. Run the shell inside the niri session.".to_string()
        })?;
    let mut stream = UnixStream::connect(&path).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    let payload = serde_json::to_string(request).map_err(|error| error.to_string())?;
    stream
        .write_all(payload.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.len() > 1024 * 1024 {
        return Err("The niri reply was too large".to_string());
    }
    let reply: Value =
        serde_json::from_str(line.trim_end()).map_err(|_| "Invalid niri reply".to_string())?;
    if reply.get("Ok").is_some() {
        Ok(reply["Ok"].clone())
    } else if let Some(error) = reply.get("Err") {
        Err(error
            .as_str()
            .unwrap_or("The niri action failed")
            .to_string())
    } else {
        Err("Invalid niri reply".to_string())
    }
}

/// M3: niri state over the socket, falling back to the subprocess probe.
pub fn niri_state(runner: &CommandRunner, use_socket: bool) -> Value {
    if !use_socket {
        return niri_state_subprocess(runner);
    }
    let read = |request: &str| niri_socket_request(&json!(request), Duration::from_secs(3));
    match (read("Workspaces"), read("Windows")) {
        (Ok(mut workspaces), Ok(windows)) => {
            if let (Some(items), Value::Array(windows)) = (workspaces.as_array_mut(), windows) {
                items.sort_by(workspace_comparator);
                let workspaces = Value::Array(items.to_vec());
                return json!({"available": true, "workspaces": workspaces, "windows": Value::Array(windows)});
            }
            unavailable()
        }
        _ => niri_state_subprocess(runner),
    }
}

/// Handle to the persistent niri event-stream (M3). The Python backend kept
/// a `niri msg --json event-stream` child purely as a change trigger; here
/// the socket stream plays that role. Only line arrival matters.
pub struct NiriEventStream {
    reader: Option<std::thread::JoinHandle<()>>,
    socket: Option<UnixStream>,
}

impl NiriEventStream {
    pub fn connect<F>(notify: F) -> Option<Self>
    where
        F: Fn() + Send + 'static,
    {
        let path = std::env::var_os("NIRI_SOCKET").map(PathBuf::from)?;
        // Connect once. The duplicated handle lets stop() wake a reader that
        // is blocked in read(): shutdown(2) is per-socket, so closing the
        // read side here makes the pending read return EOF immediately.
        let mut stream = UnixStream::connect(&path).ok()?;
        let wake = stream.try_clone().ok()?;
        let handle = std::thread::spawn(move || {
            let request = serde_json::json!("EventStream");
            let Ok(payload) = serde_json::to_string(&request) else {
                return;
            };
            if stream
                .write_all(payload.as_bytes())
                .and_then(|_| stream.write_all(b"\n"))
                .is_err()
            {
                return;
            }
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => notify(),
                }
            }
        });
        Some(Self {
            reader: Some(handle),
            socket: Some(wake),
        })
    }

    pub fn stop(&mut self) {
        // Wake the blocked read first (per-socket shutdown), then join,
        // bounded like the Python teardown (terminate, wait, kill).
        if let Some(socket) = self.socket.take() {
            let _ = socket.shutdown(std::net::Shutdown::Read);
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for NiriEventStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Build the `MoveWindowToWorkspace` action exactly like the Python code.
pub fn move_window_action(window_id: i64, workspace_id: i64) -> Value {
    json!({
        "Action": {
            "MoveWindowToWorkspace": {
                "window_id": window_id,
                "reference": {"Id": workspace_id},
                "focus": false,
            }
        }
    })
}

/// (output, idx, id) ordering used by both state readers.
fn workspace_comparator(left: &Value, right: &Value) -> std::cmp::Ordering {
    let key = workspace_sort_key;
    let a = key(left);
    let b = key(right);
    a.0.cmp(&b.0)
        .then_with(|| a.1.total_cmp(&b.1))
        .then_with(|| a.2.total_cmp(&b.2))
}

/// Sort key extraction shared by both state readers (exported for tests).
pub fn workspace_sort_key(item: &Value) -> (String, f64, f64) {
    let output = match item.get("output") {
        Some(Value::String(text)) if !text.is_empty() => text.clone(),
        _ => String::new(),
    };
    (
        output,
        crate::pyutil::number_or(item.get("idx"), 0.0),
        crate::pyutil::number_or(item.get("id"), 0.0),
    )
}

/// Convert the niri-ipc `Workspaces` reply into the pass-through list shape.
pub fn workspaces_reply_to_value(reply: Value) -> Option<Value> {
    let _ = Map::new();
    reply.as_array().cloned().map(Value::Array).or_else(|| {
        reply
            .get("Workspaces")
            .and_then(|items| items.as_array().cloned())
            .map(Value::Array)
    })
}
