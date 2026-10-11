//! `perform()` port: every input action, same messages and side effects.

use crate::backend::DesktopBackend;
use crate::charging::ChargeControl;
use crate::prefs::validate_location;
use crate::runner::{CommandError, CommandRunner};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const POWER_PROFILES: &[&str] = &["power-saver", "balanced", "performance"];

/// What `perform` produced for `handle` to fold into lastResult.
pub enum PerformOutcome {
    Message(String),
    ChargeSubmit(i64),
    Demo,
}

/// Minimal backend view handed to the charge worker thread.
pub struct ChargeJobBackend {
    pub runner: CommandRunner,
    pub charger: ChargeControl,
}

/// Charge job slot shared with the worker thread.
pub type ChargeSlot = std::sync::Arc<std::sync::Mutex<Option<Result<String, String>>>>;

pub fn percentage(value: &Value, minimum: i64) -> Result<i64, String> {
    let number = match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    };
    match number {
        Some(value) if (minimum as f64..=100.0).contains(&value) => {
            Ok(crate::pyutil::py_round_int(value))
        }
        _ => Err(format!("Value must be between {minimum} and 100")),
    }
}

pub fn identifier(value: &Value) -> Result<String, String> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            let id = n.as_i64().unwrap_or(0);
            if id >= 0 {
                Ok(id.to_string())
            } else {
                Err("A non-negative numeric ID is required".to_string())
            }
        }
        _ => Err("A non-negative numeric ID is required".to_string()),
    }
}

/// `set_charge` port: sysfs write or the pkexec helper, then confirmation.
/// Runs on the charge worker thread against a minimal backend view.
pub fn set_charge_static(
    runner: &CommandRunner,
    charger: &ChargeControl,
    limit: i64,
) -> Result<String, String> {
    let state = charger.state();
    if state["writable"].as_bool().unwrap_or(false) {
        charger.set_mode(limit)?;
    } else {
        runner.run_with_timeout(
            "pkexec",
            &[
                "--disable-internal-agent",
                "/usr/bin/python3",
                "-I",
                crate::charging::HELPER_PATH,
                &limit.to_string(),
            ],
            Duration::from_secs(90),
        )?;
    }
    let state = charger.state();
    let desired = if limit == 85 { "conservation" } else { "max" };
    if !state["available"].as_bool().unwrap_or(false)
        || state["mode"].as_str() != Some(desired)
        || (state["supports85"].as_bool().unwrap_or(false)
            && state["limitPercent"].as_i64() != Some(limit))
    {
        return Err("The kernel did not confirm the requested charge mode.".to_string());
    }
    let suffix = if state["supports85"].as_bool().unwrap_or(false) {
        format!(" · {limit}%")
    } else {
        " · firmware limit".to_string()
    };
    Ok(format!("Charge mode updated{suffix}"))
}

/// `set_bluetooth` port, including rfkill recovery and confirmation polls.
pub fn set_bluetooth(backend: &mut DesktopBackend, enabled: bool) -> Result<String, String> {
    let state = backend.probes.bluetooth(
        backend
            .preferences
            .values
            .get("bluetoothAdapter")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if !state["available"].as_bool().unwrap_or(false) {
        return Err(
            "No Bluetooth adapter is available. Check bluetooth.service and the adapter firmware."
                .to_string(),
        );
    }
    if enabled && state["hardBlocked"].as_bool().unwrap_or(false) {
        return Err(
            "Bluetooth is hardware-blocked. Disable airplane mode or use the laptop's radio switch."
                .to_string(),
        );
    }
    let adapter = state["adapter"].as_str().unwrap_or_default().to_string();

    let write_power = || -> Result<(), CommandError> {
        if !adapter.is_empty() {
            backend.probes.runner.run_with_timeout(
                "busctl",
                &[
                    "--system",
                    "set-property",
                    "org.bluez",
                    &adapter,
                    "org.bluez.Adapter1",
                    "Powered",
                    "b",
                    if enabled { "true" } else { "false" },
                ],
                Duration::from_secs(12),
            )?;
        } else {
            // bluetoothctl can return exit code zero while BlueZ reports a failure.
            let output = backend.probes.runner.run_with_timeout(
                "bluetoothctl",
                &[
                    "--timeout",
                    "10",
                    "power",
                    if enabled { "on" } else { "off" },
                ],
                Duration::from_secs(12),
            )?;
            let lowered = output.to_lowercase();
            if [
                "failed",
                "not available",
                "not authorized",
                "blocked",
                "error",
            ]
            .iter()
            .any(|needle| lowered.contains(needle))
            {
                return Err(CommandError(output.chars().take(240).collect::<String>()));
            }
        }
        Ok(())
    };

    if let Err(error) = write_power() {
        let soft_blocked = state["softBlocked"].as_bool().unwrap_or(false);
        if enabled && soft_blocked {
            if !backend.probes.runner.available("rfkill") {
                return Err(
                    "Bluetooth is software-blocked. Install rfkill and unblock Bluetooth."
                        .to_string(),
                );
            }
            let unblock = || -> Result<(), CommandError> {
                for block in backend.probes.rfkill_bluetooth_blocks() {
                    if block.soft && !block.hard && block.id.bytes().all(|b| b.is_ascii_digit()) {
                        backend
                            .probes
                            .runner
                            .run("rfkill", &["unblock", &block.id])?;
                    }
                }
                write_power()
            };
            if let Err(unblock_error) = unblock() {
                return Err(format!(
                    "Bluetooth is software-blocked; unblock it in Fedora's radio settings or run rfkill unblock bluetooth. {unblock_error}"
                ));
            }
        } else {
            return Err(format!(
                "BlueZ could not change Bluetooth power. Check the Bluetooth service, firmware and session authorization. {error}"
            ));
        }
    }
    for attempt in 0..3 {
        let current = backend.probes.bluetooth(
            backend
                .preferences
                .values
                .get("bluetoothAdapter")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        if current["enabled"].as_bool() == Some(enabled) {
            return Ok(format!(
                "Bluetooth turned {}",
                if enabled { "on" } else { "off" }
            ));
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    Err("BlueZ did not confirm the requested power state. Check journalctl -b -u bluetooth and rfkill list bluetooth.".to_string())
}

/// `apply_wallpaper` port (also serves `--restore-wallpaper`).
pub fn apply_wallpaper(
    backend: &mut DesktopBackend,
    raw: &Value,
    save: bool,
    animate: bool,
) -> Result<String, String> {
    if backend.demo {
        return Err("Wallpaper commands are disabled in demo mode".to_string());
    }
    let Some(text) = raw.as_str() else {
        return Err("A wallpaper file path is required".to_string());
    };
    let expanded = shellexpand_home(text);
    let path = match std::fs::canonicalize(&expanded) {
        Ok(resolved) => resolved,
        Err(_) => absolutize(&expanded),
    };
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !path.is_file()
        || !matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
        )
    {
        return Err("Select an existing PNG, JPEG, WebP, GIF, or BMP wallpaper".to_string());
    }
    backend.probes.runner.executable("awww")?;
    let query = || {
        backend
            .probes
            .runner
            .run_with_timeout("awww", &["query"], Duration::from_secs(1))
    };
    if query().is_err() {
        let daemon = backend.probes.runner.executable("awww-daemon")?.clone();
        backend
            .probes
            .runner
            .spawn(&[daemon.to_string_lossy().as_ref()])?;
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            match query() {
                Ok(_) => break,
                Err(_) if std::time::Instant::now() >= deadline => {
                    return Err(
                        "awww-daemon did not become ready. Check your Wayland session.".to_string(),
                    )
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
    let animations = backend
        .preferences
        .values
        .get("animations")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let transition = if animate && animations {
        "fade"
    } else {
        "none"
    };
    backend.probes.runner.run_with_timeout(
        "awww",
        &[
            "img",
            &path.to_string_lossy(),
            "--transition-type",
            transition,
        ],
        Duration::from_secs(15),
    )?;
    if save {
        backend.preferences.set(
            "wallpaper",
            Value::String(path.to_string_lossy().to_string()),
        )?;
    }
    Ok("Wallpaper updated".to_string())
}

fn shellexpand_home(text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(text)
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// `perform()` port. Charge submission is signalled back to `handle`.
pub fn perform(
    backend: &mut DesktopBackend,
    action: &str,
    request: &Map<String, Value>,
) -> Result<PerformOutcome, String> {
    match action {
        "chargeMode" => {
            let value = match request.get("value") {
                Some(Value::Number(n)) if n.is_i64() => n.as_i64(),
                _ => None,
            };
            if !matches!(value, Some(85 | 100)) {
                return Err("Choose conservation (85) or maximum charge (100).".to_string());
            }
            let value = value.unwrap_or(85);
            let state = backend.charging(backend.charge_job_running());
            if !state["canSet"].as_bool().unwrap_or(false) {
                return Err(state["reason"].as_str().unwrap_or_default().to_string());
            }
            if backend.charge_job_running() {
                return Err("A charge-mode change is already running.".to_string());
            }
            Ok(PerformOutcome::ChargeSubmit(value))
        }
        "weatherSearch" => {
            backend
                .weather
                .search(request.get("query").unwrap_or(&Value::Null))?;
            Ok(PerformOutcome::Message("Searching locations".to_string()))
        }
        "weatherDetect" => {
            backend.weather.detect();
            Ok(PerformOutcome::Message(
                "Detecting approximate location".to_string(),
            ))
        }
        "weatherRefresh" => {
            let automatic = backend
                .preferences
                .values
                .get("weatherAutoLocation")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            backend.weather.refresh(automatic);
            Ok(PerformOutcome::Message("Refreshing weather".to_string()))
        }
        "weatherSelect" => {
            let place = validate_location(request.get("location").unwrap_or(&Value::Null))?;
            if place.as_object().is_some_and(|map| map.is_empty()) {
                return Err("Select a city first.".to_string());
            }
            let mut updates = Map::new();
            updates.insert("weatherLocation".into(), place);
            updates.insert("weatherAutoLocation".into(), Value::Bool(false));
            backend.preferences.update(updates)?;
            Ok(PerformOutcome::Message(
                "Weather location updated".to_string(),
            ))
        }
        "refresh" => {
            if backend.settings_only {
                backend.preferences.reload(true);
                return Ok(PerformOutcome::Message("Preferences refreshed".to_string()));
            }
            backend.apps = backend.applications.scan();
            backend.last_app_scan = std::time::Instant::now();
            Ok(PerformOutcome::Message("State refreshed".to_string()))
        }
        "launch" => {
            let app_id = request
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(app) = backend.applications.entries.get(app_id) else {
                return Err("Application is no longer available; refresh the launcher".to_string());
            };
            let name = app["name"].as_str().unwrap_or_default().to_string();
            let argv: Vec<String> = app["argv"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            if app["terminal"].as_bool().unwrap_or(false) {
                // gio chooses the desktop's terminal independently of our
                // setting; terminal applications must honor the user's choice.
                let terminal = backend
                    .preferences
                    .values
                    .get("terminal")
                    .and_then(Value::as_str)
                    .unwrap_or("kitty");
                let Some(terminal) = crate::pyutil::which(terminal) else {
                    return Err("The configured terminal is unavailable".to_string());
                };
                let mut command = vec![terminal.to_string_lossy().to_string(), "-e".to_string()];
                command.extend(argv);
                let refs: Vec<&str> = command.iter().map(String::as_str).collect();
                backend.probes.runner.spawn(&refs)?;
            } else if backend.probes.runner.available("gio") {
                backend
                    .probes
                    .runner
                    .run("gio", &["launch", app["path"].as_str().unwrap_or_default()])?;
            } else {
                let command = argv;
                let refs: Vec<&str> = command.iter().map(String::as_str).collect();
                backend.probes.runner.spawn(&refs)?;
            }
            Ok(PerformOutcome::Message(format!("Launched {name}")))
        }
        "focusWindow" => {
            let id = identifier(request.get("id").unwrap_or(&Value::Null))?;
            backend
                .probes
                .runner
                .run("niri", &["msg", "action", "focus-window", "--id", &id])?;
            Ok(PerformOutcome::Message("Window focused".to_string()))
        }
        "moveWindow" => {
            let window_id: i64 = identifier(request.get("id").unwrap_or(&Value::Null))?
                .parse()
                .unwrap_or(0);
            let workspace_id: i64 = identifier(request.get("workspaceId").unwrap_or(&Value::Null))?
                .parse()
                .unwrap_or(0);
            let state = crate::niri::niri_state(&backend.probes.runner, backend.uses_socket());
            let window_exists = state["windows"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["id"].as_i64() == Some(window_id))
            });
            let workspace_exists = state["workspaces"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["id"].as_i64() == Some(workspace_id))
            });
            if !window_exists || !workspace_exists {
                return Err("Window or workspace is no longer available".to_string());
            }
            backend.set_niri_state(state);
            crate::niri::niri_socket_request(
                &crate::niri::move_window_action(window_id, workspace_id),
                Duration::from_secs(3),
            )?;
            Ok(PerformOutcome::Message(
                "Window moved to workspace".to_string(),
            ))
        }
        "nativeOverview" => {
            backend
                .probes
                .runner
                .run("niri", &["msg", "action", "toggle-overview"])?;
            Ok(PerformOutcome::Message(
                "Native overview toggled".to_string(),
            ))
        }
        "workspace" => {
            let workspace_id = identifier(request.get("id").unwrap_or(&Value::Null))?;
            let mut workspace = find_workspace(&backend.niri, &workspace_id);
            if workspace.is_none() {
                let state = crate::niri::niri_state(&backend.probes.runner, backend.uses_socket());
                backend.set_niri_state(state);
                workspace = find_workspace(&backend.niri, &workspace_id);
            }
            let Some(workspace) = workspace else {
                return Err("Workspace is no longer available".to_string());
            };
            if let Some(output) = workspace.get("output").and_then(Value::as_str) {
                if !output.is_empty() {
                    backend
                        .probes
                        .runner
                        .run("niri", &["msg", "action", "focus-monitor", output])?;
                }
            }
            let idx = match workspace.get("idx") {
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                _ => 0,
            };
            backend.probes.runner.run(
                "niri",
                &["msg", "action", "focus-workspace", &idx.to_string()],
            )?;
            Ok(PerformOutcome::Message("Workspace focused".to_string()))
        }
        "volume" | "micVolume" => {
            let value = percentage(request.get("value").unwrap_or(&Value::Null), 0)?;
            let source = if action == "volume" {
                "@DEFAULT_AUDIO_SINK@"
            } else {
                "@DEFAULT_AUDIO_SOURCE@"
            };
            backend.probes.runner.run(
                "wpctl",
                &["set-volume", "--limit", "1.0", source, &format!("{value}%")],
            )?;
            Ok(PerformOutcome::Message(format!(
                "{} {value}%",
                if action == "volume" {
                    "Volume"
                } else {
                    "Microphone"
                }
            )))
        }
        "mute" | "micMute" => {
            let source = if action == "mute" {
                "@DEFAULT_AUDIO_SINK@"
            } else {
                "@DEFAULT_AUDIO_SOURCE@"
            };
            backend
                .probes
                .runner
                .run("wpctl", &["set-mute", source, "toggle"])?;
            Ok(PerformOutcome::Message(if action == "mute" {
                "Mute toggled".to_string()
            } else {
                "Microphone mute toggled".to_string()
            }))
        }
        "brightness" => {
            let value = percentage(request.get("value").unwrap_or(&Value::Null), 1)?;
            backend.probes.runner.run(
                "brightnessctl",
                &["--class=backlight", "set", &format!("{value}%")],
            )?;
            Ok(PerformOutcome::Message(format!("Brightness {value}%")))
        }
        "wifiToggle" => {
            let status = backend.probes.runner.run("nmcli", &["radio", "wifi"])?;
            let target = if status == "enabled" { "off" } else { "on" };
            backend
                .probes
                .runner
                .run("nmcli", &["radio", "wifi", target])?;
            Ok(PerformOutcome::Message("Wi-Fi toggled".to_string()))
        }
        "bluetoothToggle" => {
            let state = backend.probes.bluetooth(
                backend
                    .preferences
                    .values
                    .get("bluetoothAdapter")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            let enabled = state["enabled"].as_bool().unwrap_or(false);
            set_bluetooth(backend, !enabled).map(PerformOutcome::Message)
        }
        "bluetoothDevice" => {
            let path = request
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let connected = request.get("connected").and_then(Value::as_bool);
            let state = backend.probes.bluetooth(
                backend
                    .preferences
                    .values
                    .get("bluetoothAdapter")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            let known = state["devices"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["path"].as_str() == Some(path)));
            if !valid_device_path(path) || connected.is_none() || !known {
                return Err("Unknown Bluetooth device or state".to_string());
            }
            backend.probes.runner.run_with_timeout(
                "busctl",
                &[
                    "--system",
                    "call",
                    "org.bluez",
                    path,
                    "org.bluez.Device1",
                    if connected == Some(true) {
                        "Connect"
                    } else {
                        "Disconnect"
                    },
                ],
                Duration::from_secs(15),
            )?;
            Ok(PerformOutcome::Message(format!(
                "Bluetooth device {}",
                if connected == Some(true) {
                    "connected"
                } else {
                    "disconnected"
                }
            )))
        }
        "powerProfile" => {
            let Some(value) = request.get("value").and_then(Value::as_str) else {
                return Err("Invalid power profile".to_string());
            };
            if !POWER_PROFILES.contains(&value) {
                return Err("Invalid power profile".to_string());
            }
            let profile = backend.probes.power_profile();
            let provider = backend.probes.provider();
            if profile.is_none() || provider.is_none() {
                return Err("No compatible power-profile service is available".to_string());
            }
            let provider = provider.unwrap();
            if provider.first().map(String::as_str) == Some("dbus") {
                // An active local session is normally authorized by polkit;
                // allow a bounded interaction if the administrator changed it.
                backend.probes.runner.run_with_timeout(
                    "busctl",
                    &[
                        "--system",
                        "set-property",
                        &provider[1],
                        &provider[2],
                        &provider[3],
                        "ActiveProfile",
                        "s",
                        value,
                    ],
                    Duration::from_secs(30),
                )?;
            } else {
                backend.probes.runner.run_with_timeout(
                    "powerprofilesctl",
                    &["set", value],
                    Duration::from_secs(30),
                )?;
            }
            Ok(PerformOutcome::Message(format!("Power profile: {value}")))
        }
        "lock" => {
            let swaylock = backend.probes.runner.executable("swaylock")?.clone();
            backend.probes.runner.spawn(&[
                swaylock.to_string_lossy().as_ref(),
                "--color",
                "22212c",
            ])?;
            Ok(PerformOutcome::Message("Lock requested".to_string()))
        }
        "screenshot" | "screenshotArea" => {
            // niri owns capture and clipboard integration. Let a just-dismissed
            // control panel finish its surface teardown.
            std::thread::sleep(Duration::from_millis(200));
            let command = if action == "screenshot" {
                "screenshot-screen"
            } else {
                "screenshot"
            };
            backend
                .probes
                .runner
                .run("niri", &["msg", "action", command])?;
            Ok(PerformOutcome::Message(
                "Screenshot requested; output follows niri's screenshot-path setting".to_string(),
            ))
        }
        "logout" => {
            backend
                .probes
                .runner
                .run("niri", &["msg", "action", "quit", "--skip-confirmation"])?;
            Ok(PerformOutcome::Message("Logout requested".to_string()))
        }
        "suspend" | "reboot" | "poweroff" => {
            backend.probes.runner.run("systemctl", &[action])?;
            let mut capitalized = action.to_string();
            if let Some(first) = capitalized.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            Ok(PerformOutcome::Message(format!("{capitalized} requested")))
        }
        "wallpaper" => apply_wallpaper(
            backend,
            request.get("path").unwrap_or(&Value::Null),
            true,
            true,
        )
        .map(PerformOutcome::Message),
        "setting" => {
            let Some(key) = request.get("key").and_then(Value::as_str) else {
                return Err("Use the wallpaper action to change wallpaper".to_string());
            };
            if key == "wallpaper" {
                return Err("Use the wallpaper action to change wallpaper".to_string());
            }
            backend
                .preferences
                .set(key, request.get("value").cloned().unwrap_or(Value::Null))?;
            Ok(PerformOutcome::Message("Preferences saved".to_string()))
        }
        "openSettings" => {
            let commands = [
                ("network", "nm-connection-editor"),
                ("bluetooth", "blueman-manager"),
                ("audio", "pavucontrol"),
            ];
            let Some(target) = request.get("target").and_then(Value::as_str) else {
                return Err("Unknown settings panel".to_string());
            };
            let Some((target, tool)) = commands.iter().find(|(name, _)| *name == target) else {
                return Err("Unknown settings panel".to_string());
            };
            let executable = backend.probes.runner.executable(tool)?.clone();
            backend
                .probes
                .runner
                .spawn(&[executable.to_string_lossy().as_ref()])?;
            Ok(PerformOutcome::Message(format!("Opened {target} settings")))
        }
        _ => Err(format!("Unknown action: {action}")),
    }
}

fn find_workspace<'a>(state: &'a Value, workspace_id: &str) -> Option<&'a Value> {
    state["workspaces"].as_array()?.iter().find(|item| {
        match item.get("id") {
            Some(Value::Number(n)) => n.as_i64().map(|v| v.to_string()),
            _ => None,
        }
        .as_deref()
            == Some(workspace_id)
    })
}

fn valid_device_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/org/bluez/hci") else {
        return false;
    };
    let Some((adapter, device)) = rest.split_once("/dev_") else {
        return false;
    };
    if adapter.is_empty() || !adapter.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let parts: Vec<&str> = device.split('_').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `perform_demo` port: mutate the demo state instead of the system.
pub fn perform_demo(
    backend: &mut DesktopBackend,
    action: &str,
    request: &Map<String, Value>,
) -> Result<(), String> {
    let snapshot = backend.demo_snapshot();
    let mut state = snapshot.as_object().cloned().unwrap_or_default();
    let system = state
        .get_mut("system")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "Demo state is unavailable".to_string())?;
    match action {
        "setting" => {
            let Some(key) = request.get("key").and_then(Value::as_str) else {
                return Err("Use the wallpaper action to change wallpaper".to_string());
            };
            if key == "wallpaper" {
                return Err("Use the wallpaper action to change wallpaper".to_string());
            }
            let validated = crate::prefs::validate_preference(
                key,
                request.get("value").unwrap_or(&Value::Null),
            )?;
            backend
                .preferences
                .values
                .insert(key.to_string(), validated);
        }
        "volume" => {
            system.get_mut("audio").unwrap()["volume"] =
                json!(percentage(request.get("value").unwrap_or(&Value::Null), 0)?);
        }
        "mute" => {
            let audio = system.get_mut("audio").unwrap();
            let muted = !audio["muted"].as_bool().unwrap_or(false);
            audio["muted"] = Value::Bool(muted);
        }
        "micVolume" => {
            system.get_mut("microphone").unwrap()["volume"] =
                json!(percentage(request.get("value").unwrap_or(&Value::Null), 0)?);
        }
        "micMute" => {
            let microphone = system.get_mut("microphone").unwrap();
            let muted = !microphone["muted"].as_bool().unwrap_or(false);
            microphone["muted"] = Value::Bool(muted);
        }
        "brightness" => {
            system.get_mut("brightness").unwrap()["percent"] =
                json!(percentage(request.get("value").unwrap_or(&Value::Null), 1)?);
        }
        "wifiToggle" => {
            let network = system.get_mut("network").unwrap();
            let enabled = !network["enabled"].as_bool().unwrap_or(false);
            network["enabled"] = Value::Bool(enabled);
            network["connected"] = Value::Bool(enabled);
        }
        "bluetoothToggle" => {
            let bluetooth = system.get_mut("bluetooth").unwrap();
            let enabled = !bluetooth["enabled"].as_bool().unwrap_or(false);
            bluetooth["enabled"] = Value::Bool(enabled);
        }
        "bluetoothDevice" => {
            let path = request
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(connected) = request.get("connected").and_then(Value::as_bool) else {
                return Err("Unknown Bluetooth device or state".to_string());
            };
            let devices = system
                .get_mut("bluetooth")
                .unwrap()
                .get_mut("devices")
                .and_then(Value::as_array_mut)
                .unwrap();
            let Some(device) = devices
                .iter_mut()
                .find(|item| item["path"].as_str() == Some(path))
            else {
                return Err("Unknown Bluetooth device or state".to_string());
            };
            device["connected"] = Value::Bool(connected);
        }
        "powerProfile" => {
            let Some(value) = request.get("value").and_then(Value::as_str) else {
                return Err("Invalid power profile".to_string());
            };
            if !POWER_PROFILES.contains(&value) {
                return Err("Invalid power profile".to_string());
            }
            system.insert("powerProfile".into(), Value::String(value.to_string()));
        }
        "chargeMode" => {
            let value = match request.get("value") {
                Some(Value::Number(n)) if n.is_i64() => n.as_i64(),
                _ => None,
            };
            if !matches!(value, Some(85 | 100)) {
                return Err("Choose conservation (85) or maximum charge (100).".to_string());
            }
            let charging = system.get_mut("charging").unwrap();
            charging["mode"] = json!(if value == Some(85) {
                "conservation"
            } else {
                "max"
            });
            charging["limitPercent"] = json!(value.unwrap_or(100));
        }
        "weatherSearch" => {
            let query = request
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let trimmed = query.trim();
            if trimmed.chars().count() < 2 || trimmed.chars().count() > 100 {
                return Err("Enter a city name of 2–100 characters.".to_string());
            }
            let weather = state.get_mut("weather").unwrap();
            weather["search"] = json!({
                "query": trimmed, "loading": false, "error": "",
                "results": [{"name": trimmed, "country": "Demo", "latitude": 10.82, "longitude": 106.63}],
            });
        }
        "weatherSelect" => {
            let place = validate_location(request.get("location").unwrap_or(&Value::Null))?;
            if place.as_object().is_some_and(|map| map.is_empty()) {
                return Err("Select a city first.".to_string());
            }
            backend
                .preferences
                .values
                .insert("weatherLocation".into(), place);
            backend
                .preferences
                .values
                .insert("weatherAutoLocation".into(), Value::Bool(false));
        }
        "weatherRefresh" | "weatherDetect" => {}
        "workspace" => {
            let id = identifier(request.get("id").unwrap_or(&Value::Null))?;
            let workspaces = state
                .get_mut("niri")
                .unwrap()
                .get_mut("workspaces")
                .and_then(Value::as_array_mut)
                .unwrap();
            if !workspaces.iter().any(|item| {
                item["id"].as_i64().map(|v| v.to_string()).as_deref() == Some(id.as_str())
            }) {
                return Err("Workspace is no longer available".to_string());
            }
            for workspace in workspaces {
                let matches =
                    workspace["id"].as_i64().map(|v| v.to_string()).as_deref() == Some(id.as_str());
                workspace["is_focused"] = Value::Bool(matches);
                workspace["is_active"] = Value::Bool(matches);
            }
        }
        "focusWindow" => {
            let id = identifier(request.get("id").unwrap_or(&Value::Null))?;
            let windows = state
                .get_mut("niri")
                .unwrap()
                .get_mut("windows")
                .and_then(Value::as_array_mut)
                .unwrap();
            if !windows.iter().any(|item| {
                item["id"].as_i64().map(|v| v.to_string()).as_deref() == Some(id.as_str())
            }) {
                return Err("Window is no longer available".to_string());
            }
            for window in windows {
                let matches =
                    window["id"].as_i64().map(|v| v.to_string()).as_deref() == Some(id.as_str());
                window["is_focused"] = Value::Bool(matches);
            }
        }
        "moveWindow" => {
            let window_id = identifier(request.get("id").unwrap_or(&Value::Null))?;
            let workspace_id = identifier(request.get("workspaceId").unwrap_or(&Value::Null))?;
            let niri = state.get_mut("niri").unwrap();
            let workspace_exists = niri["workspaces"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["id"].as_i64().map(|v| v.to_string()).as_deref()
                        == Some(workspace_id.as_str())
                })
            });
            if !workspace_exists {
                return Err("Window or workspace is no longer available".to_string());
            }
            let windows = niri.get_mut("windows").unwrap().as_array_mut().unwrap();
            let Some(window) = windows.iter_mut().find(|item| {
                item["id"].as_i64().map(|v| v.to_string()).as_deref() == Some(window_id.as_str())
            }) else {
                return Err("Window or workspace is no longer available".to_string());
            };
            window["workspace_id"] = json!(workspace_id.parse::<i64>().unwrap_or(0));
        }
        "launch" => {
            let app_id = request
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !state["apps"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["id"].as_str() == Some(app_id)))
            {
                return Err("Application is no longer available".to_string());
            }
        }
        "wallpaper" => {
            let Some(text) = request.get("path").and_then(Value::as_str) else {
                return Err("A wallpaper file path is required".to_string());
            };
            let path = std::fs::canonicalize(shellexpand_home(text))
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| text.to_string());
            let validated = crate::prefs::validate_preference("wallpaper", &Value::String(path))?;
            backend
                .preferences
                .values
                .insert("wallpaper".into(), validated);
        }
        "openSettings" => {
            let target = request
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(target, "network" | "bluetooth" | "audio") {
                return Err("Unknown settings panel".to_string());
            }
        }
        "refresh" | "lock" | "screenshot" | "screenshotArea" | "nativeOverview" | "logout"
        | "suspend" | "reboot" | "poweroff" => {}
        _ => return Err(format!("Unknown action: {action}")),
    }
    backend.set_demo_state(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentage_bounds_and_rounding() {
        assert_eq!(percentage(&json!(45), 0), Ok(45));
        assert_eq!(percentage(&json!(45.4), 0), Ok(45));
        assert!(percentage(&json!(101), 0).is_err());
        assert!(percentage(&json!(0), 1).is_err());
        assert!(percentage(&json!(null), 0).is_err());
        assert!(percentage(&json!(true), 0).is_err());
    }

    #[test]
    fn identifier_requires_nonnegative_int() {
        assert_eq!(identifier(&json!(3)), Ok("3".to_string()));
        assert!(identifier(&json!(-1)).is_err());
        assert!(identifier(&json!("3")).is_err());
        assert!(identifier(&json!(3.5)).is_err());
    }

    #[test]
    fn device_path_regex() {
        assert!(valid_device_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF"));
        assert!(!valid_device_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE"));
        assert!(!valid_device_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_GG"));
        assert!(!valid_device_path("/org/bluez/hc/dev_AA_BB_CC_DD_EE_FF"));
    }

    #[test]
    fn py_truthy_semantics() {
        use crate::probes::py_truthy;
        assert!(py_truthy(&json!(true)));
        assert!(!py_truthy(&json!(false)));
        assert!(!py_truthy(&json!(null)));
        assert!(py_truthy(&json!("x")));
        assert!(!py_truthy(&json!("")));
        assert!(py_truthy(&json!(0.5)));
        assert!(!py_truthy(&json!(0)));
    }
}
