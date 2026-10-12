// SPDX-License-Identifier: GPL-3.0-or-later
//! niri integration for the daemon: cached workspaces/windows state, the
//! focused window title, and an event-driven refresh via the niri event
//! stream (`NIRI_SOCKET` or `niri msg --json event-stream`). In the sandbox
//! there is no niri: tests inject state via `LME_SHELL_FAKE_STATE`.

use lme_backend::niri;
use lme_backend::runner::CommandRunner;
use serde_json::Value;
use std::sync::mpsc as std_mpsc;

pub struct NiriService {
    runner: CommandRunner,
    use_socket: bool,
    state: Value,
    fake: Option<Value>,
    stream: Option<niri::NiriEventStream>,
}

impl NiriService {
    pub fn new() -> Self {
        let fake = std::env::var("LME_SHELL_FAKE_STATE")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok());
        let use_socket = std::env::var_os("NIRI_SOCKET").is_some();
        let runner = CommandRunner::new();
        let mut service = Self {
            runner,
            use_socket,
            state: Value::Null,
            fake,
            stream: None,
        };
        service.refresh();
        service
    }

    pub fn available(&self) -> bool {
        if self.fake.is_some() {
            return true;
        }
        self.state
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Full cached state (snapshot shape: {available, workspaces, windows}).
    pub fn state(&self) -> &Value {
        &self.state
    }

    pub fn refresh(&mut self) -> bool {
        let new = match &self.fake {
            Some(f) => f.clone(),
            None => niri::niri_state(&self.runner, self.use_socket),
        };
        let changed = serde_json::to_string(&new).ok() != serde_json::to_string(&self.state).ok();
        self.state = new;
        changed
    }

    /// Focused window title, "" when niri is unavailable.
    pub fn focused_title(&self) -> String {
        if !self.available() {
            return String::new();
        }
        self.state
            .get("windows")
            .and_then(Value::as_array)
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|w| {
                        w.get("is_focused")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    })
                    .and_then(|w| w.get("title").and_then(Value::as_str))
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }

    /// Workspaces for one output (legacy `ShellState.workspacesFor`): sorted
    /// by (output, idx, id), filtered by the output name when known.
    pub fn workspaces_for(&self, output_name: &str) -> Vec<Value> {
        let Some(list) = self.state.get("workspaces").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut items: Vec<Value> = list
            .iter()
            .filter(|ws| {
                let ws_output = ws.get("output").and_then(Value::as_str).unwrap_or("");
                output_name.is_empty() || ws_output == output_name
            })
            .cloned()
            .collect();
        items.sort_by(|a, b| {
            let ka = niri::workspace_sort_key(a);
            let kb = niri::workspace_sort_key(b);
            ka.0.cmp(&kb.0)
                .then(ka.1.partial_cmp(&kb.1).unwrap_or(std::cmp::Ordering::Equal))
                .then(ka.2.partial_cmp(&kb.2).unwrap_or(std::cmp::Ordering::Equal))
        });
        items
    }

    /// Start the event stream; each niri event pings the channel (the daemon
    /// then re-queries state — the stream is a change signal, like legacy).
    pub fn start_stream(&mut self, notify: std_mpsc::Sender<()>) -> bool {
        if self.fake.is_some() || self.stream.is_some() {
            return false;
        }
        let sender = notify;
        self.stream = niri::NiriEventStream::connect(move || {
            let _ = sender.send(());
        });
        self.stream.is_some()
    }

    /// Focus a workspace by id (legacy `workspace` action).
    pub fn focus_workspace(&self, id: i64) -> Result<String, String> {
        if self.fake.is_some() {
            return Ok(format!("[fake] focus workspace {id}"));
        }
        let _ = id;
        // The legacy action resolves the workspace against the cached state
        // and runs niri msg focus-workspace; reuse the backend action.
        let action = serde_json::json!({"action": "workspace", "id": id});
        let _ = action;
        Err("workspace switching needs the live backend loop (Tier 2 wiring)".into())
    }
}

impl Default for NiriService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_state() -> Value {
        serde_json::json!({
            "available": true,
            "workspaces": [
                {"id": 3, "idx": 2, "name": "two", "output": "DP-1", "is_focused": false},
                {"id": 1, "idx": 0, "name": "home", "output": "DP-1", "is_focused": true},
                {"id": 2, "idx": 1, "output": "HDMI-1", "is_focused": false}
            ],
            "windows": [
                {"id": 10, "title": "Terminal", "app_id": "kitty", "is_focused": true, "workspace_id": 1},
                {"id": 11, "title": "Browser", "app_id": "firefox", "is_focused": false, "workspace_id": 3}
            ]
        })
    }

    #[test]
    fn sorts_and_filters_workspaces() {
        let mut svc = NiriService::new();
        svc.fake = Some(fake_state());
        svc.refresh();
        assert!(svc.available());
        let dp1 = svc.workspaces_for("DP-1");
        assert_eq!(dp1.len(), 2);
        assert_eq!(dp1[0].get("name").and_then(Value::as_str), Some("home"));
        assert_eq!(svc.focused_title(), "Terminal");
        let all = svc.workspaces_for("");
        assert_eq!(all.len(), 3);
    }
}
