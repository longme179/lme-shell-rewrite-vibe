// SPDX-License-Identifier: GPL-3.0-or-later
//! Power actions behind a trait: mock/dry-run for tests, logind D-Bus for
//! the real machine (Phase 2 approved deviation: logind instead of
//! `systemctl`). Lock spawns `swaylock --color 22212c`, logout runs
//! `niri msg action quit --skip-confirmation` — the exact legacy semantics
//! (REBUILD_SPEC §3.4). Tests never call the real implementations.

use std::process::Stdio;

pub trait PowerExecutor {
    fn lock(&mut self) -> Result<String, String>;
    fn logout(&mut self) -> Result<String, String>;
    fn suspend(&mut self) -> Result<String, String>;
    fn reboot(&mut self) -> Result<String, String>;
    fn poweroff(&mut self) -> Result<String, String>;
}

/// Dry-run: logs intent, touches nothing (tests, `--dry-run`).
#[derive(Default)]
pub struct DryRunPower {
    pub log: Vec<String>,
}

impl PowerExecutor for DryRunPower {
    fn lock(&mut self) -> Result<String, String> {
        self.log.push("lock".into());
        Ok("[dry-run] lock".into())
    }
    fn logout(&mut self) -> Result<String, String> {
        self.log.push("logout".into());
        Ok("[dry-run] logout".into())
    }
    fn suspend(&mut self) -> Result<String, String> {
        self.log.push("suspend".into());
        Ok("[dry-run] suspend".into())
    }
    fn reboot(&mut self) -> Result<String, String> {
        self.log.push("reboot".into());
        Ok("[dry-run] reboot".into())
    }
    fn poweroff(&mut self) -> Result<String, String> {
        self.log.push("poweroff".into());
        Ok("[dry-run] poweroff".into())
    }
}

/// Real executor: logind on the system bus (zbus blocking API runs on its
/// own executor thread), swaylock / niri spawned detached.
pub struct LogindPower;

impl LogindPower {
    fn call(&self, method: &str) -> Result<String, String> {
        let conn = zbus::blocking::Connection::system().map_err(|e| format!("system bus: {e}"))?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .map_err(|e| format!("login1 proxy: {e}"))?;
        proxy
            .call_method(method, &false)
            .map_err(|e| format!("{method}: {e}"))?;
        Ok(format!("{method} requested"))
    }
}

/// Spawn a detached process (never through a shell; argv only).
/// Spawn a child we HOLD (idle inhibitor, wlsunset) — like spawn_detached
/// but returns the Child so the caller can kill it to release/stop.
/// (Tier 2.5; dry-run callers check their own flag before calling this.)
pub fn spawn_detached_hold(argv: &[&str]) -> Result<std::process::Child, String> {
    use std::process::{Command, Stdio};
    let (prog, rest) = argv.split_first().ok_or("empty argv")?;
    Command::new(prog)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {prog}: {e}"))
}

pub fn spawn_detached(argv: &[&str]) -> Result<String, String> {
    use std::process::Command;
    Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", argv[0]))?;
    Ok(format!("spawned {}", argv[0]))
}

impl PowerExecutor for LogindPower {
    fn lock(&mut self) -> Result<String, String> {
        spawn_detached(&["swaylock", "--color", "22212c"])
    }
    fn logout(&mut self) -> Result<String, String> {
        spawn_detached(&["niri", "msg", "action", "quit", "--skip-confirmation"])
    }
    fn suspend(&mut self) -> Result<String, String> {
        self.call("Suspend")
    }
    fn reboot(&mut self) -> Result<String, String> {
        self.call("Reboot")
    }
    fn poweroff(&mut self) -> Result<String, String> {
        self.call("PowerOff")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_logs_without_touching_anything() {
        let mut p = DryRunPower::default();
        assert!(p.suspend().is_ok());
        assert!(p.poweroff().is_ok());
        assert_eq!(p.log, vec!["suspend".to_string(), "poweroff".to_string()]);
    }
}
