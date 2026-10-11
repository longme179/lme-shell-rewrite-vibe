//! System probes: /proc and /sys readers plus the CLI-tool probes
//! (`nmcli`, `wpctl`, `brightnessctl`, `powerprofilesctl`), each a faithful
//! port of the corresponding Python method, including value clamps, rounding
//! and JSON key order.

use crate::pyutil::{py_round_int, py_round_nd};
use crate::runner::CommandRunner;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const POWER_PROFILE_SERVICES: &[(&str, &str, &str)] = &[
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    ),
    (
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
    ),
];
pub const POWER_PROFILES: &[&str] = &["power-saver", "balanced", "performance"];

/// Read a (trimmed) text file, `None` on any error.
pub fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

pub fn read_f64(path: &Path) -> Option<f64> {
    read(path).and_then(|text| text.parse::<f64>().ok())
}

/// `nmcli -t` field splitter: backslash escapes any character, `:` separates.
pub fn split_nmcli(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ':' {
            fields.push(current.clone());
            current.clear();
        } else {
            current.push(character);
        }
    }
    current.push_str(if escaped { "\\" } else { "" });
    fields.push(current);
    fields
}

pub struct SystemProbes {
    pub runner: CommandRunner,
    pub proc: PathBuf,
    pub sysfs: PathBuf,
    last_cpu: Option<(i64, i64)>,
    power_profile_provider: Option<Vec<String>>,
}

impl SystemProbes {
    pub fn new(runner: CommandRunner, proc: PathBuf, sysfs: PathBuf) -> Self {
        Self {
            runner,
            proc,
            sysfs,
            last_cpu: None,
            power_profile_provider: None,
        }
    }

    pub fn try_run(&self, name: &str, args: &[&str]) -> Option<String> {
        if !self.runner.available(name) {
            return None;
        }
        self.runner.run(name, args).ok()
    }

    /// `/proc/stat` + `/proc/meminfo`; first sample reports `cpu: null`.
    pub fn cpu_memory(&mut self) -> Map<String, Value> {
        let mut result = Map::new();
        result.insert("cpu".into(), Value::Null);
        result.insert("memoryPercent".into(), Value::Null);
        result.insert("memoryUsedGiB".into(), Value::Null);
        result.insert("memoryTotalGiB".into(), Value::Null);
        if let Some(stat) = read(&self.proc.join("stat")) {
            if let Some(first_line) = stat.lines().next() {
                // guest and guest_nice already count in user and nice.
                let times: Option<Vec<i64>> = first_line
                    .split_whitespace()
                    .skip(1)
                    .take(8)
                    .map(|value| value.parse().ok())
                    .collect();
                if let Some(times) = times {
                    if times.len() >= 4 {
                        let total: i64 = times.iter().sum();
                        let idle = times[3] + times.get(4).copied().unwrap_or(0);
                        if let Some((last_total, last_idle)) = self.last_cpu {
                            let delta_total = total - last_total;
                            let delta_idle = idle - last_idle;
                            if delta_total > 0 {
                                let usage = (100.0
                                    * (1.0 - delta_idle as f64 / delta_total as f64))
                                    .clamp(0.0, 100.0);
                                result
                                    .insert("cpu".into(), serde_json::json!(py_round_nd(usage, 1)));
                            }
                        }
                        self.last_cpu = Some((total, idle));
                    }
                }
            }
        }
        if let Some(meminfo) = read(&self.proc.join("meminfo")) {
            let mut data: HashMap<&str, f64> = HashMap::new();
            for line in meminfo.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                if let Ok(parsed) = value.split_whitespace().next().unwrap_or("").parse::<f64>() {
                    data.insert(key, parsed);
                }
            }
            if let (Some(&total), Some(&available)) =
                (data.get("MemTotal"), data.get("MemAvailable"))
            {
                if total > 0.0 {
                    let used = (total - available).max(0.0);
                    result.insert(
                        "memoryPercent".into(),
                        serde_json::json!(py_round_nd(100.0 * used / total, 1)),
                    );
                    result.insert(
                        "memoryUsedGiB".into(),
                        serde_json::json!(py_round_nd(used / 1024.0 / 1024.0, 2)),
                    );
                    result.insert(
                        "memoryTotalGiB".into(),
                        serde_json::json!(py_round_nd(total / 1024.0 / 1024.0, 2)),
                    );
                }
            }
        }
        result
    }

    /// CPU-package sensors only: querying the NVIDIA GPU can wake a dGPU.
    pub fn temperature(&self) -> Value {
        let mut candidates: Vec<PathBuf> = Vec::new();
        let hwmon = self.sysfs.join("class/hwmon");
        let mut directories: Vec<PathBuf> = glob_entries(&hwmon, "hwmon*");
        directories.sort();
        for directory in directories {
            let Some(name) = read(&directory.join("name")) else {
                continue;
            };
            if !matches!(name.as_str(), "k10temp" | "zenpower" | "coretemp") {
                continue;
            }
            let mut sensors: Vec<PathBuf> = glob_entries(&directory, "temp*_input");
            sensors.sort();
            for sensor in sensors {
                let label_path = sensor.with_file_name(
                    sensor
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .replace("_input", "_label"),
                );
                let label = read(&label_path);
                let include = match &label {
                    None => true,
                    Some(text) => matches!(text.as_str(), "Tctl" | "Tdie" | "Package id 0"),
                };
                if include {
                    candidates.push(sensor);
                }
            }
        }
        let thermal = self.sysfs.join("class/thermal");
        let mut zones: Vec<PathBuf> = glob_entries(&thermal, "thermal_zone*");
        zones.sort();
        for zone in zones {
            if matches!(
                read(&zone.join("type")).as_deref(),
                Some("x86_pkg_temp") | Some("cpu-thermal") | Some("acpitz")
            ) {
                candidates.push(zone.join("temp"));
            }
        }
        for sensor in candidates {
            if let Some(value) = read_f64(&sensor) {
                let celsius = value / 1000.0;
                if celsius.is_finite() && (-20.0..=150.0).contains(&celsius) {
                    return serde_json::json!(py_round_nd(celsius, 1));
                }
            }
        }
        Value::Null
    }

    pub fn battery(&self) -> Value {
        let supplies = self.sysfs.join("class/power_supply");
        let mut batteries: Vec<PathBuf> = glob_entries(&supplies, "*")
            .into_iter()
            .filter(|path| read(&path.join("type")).as_deref() == Some("Battery"))
            .collect();
        batteries.sort();
        let mut capacities: Vec<f64> = Vec::new();
        let mut charging = false;
        for battery in batteries {
            if let Some(capacity) = read_f64(&battery.join("capacity")) {
                if capacity.is_finite() && (0.0..=100.0).contains(&capacity) {
                    capacities.push(capacity);
                }
            }
            if read(&battery.join("status")).as_deref() == Some("Charging") {
                charging = true;
            }
        }
        let percent = if capacities.is_empty() {
            Value::Null
        } else {
            serde_json::json!(py_round_int(
                capacities.iter().sum::<f64>() / capacities.len() as f64
            ))
        };
        serde_json::json!({
            "available": !capacities.is_empty(), "percent": percent, "charging": charging,
        })
    }

    pub fn network(&self) -> Value {
        let mut result = serde_json::json!({
            "available": false, "enabled": false, "connected": false,
            "ssid": "", "strength": null, "networks": [],
        });
        let object = result.as_object_mut().unwrap();
        let Some(radio) = self.try_run("nmcli", &["radio", "wifi"]) else {
            return result;
        };
        object.insert("available".into(), Value::Bool(true));
        object.insert("enabled".into(), Value::Bool(radio == "enabled"));
        let devices = self.try_run(
            "nmcli",
            &["-t", "-f", "TYPE,STATE,CONNECTION", "device", "status"],
        );
        for line in devices.unwrap_or_default().lines() {
            let fields = split_nmcli(line);
            if fields.len() >= 3 && fields[1] == "connected" {
                object.insert("connected".into(), Value::Bool(true));
                if fields[0] == "ethernet" {
                    object.insert("ssid".into(), Value::String("Ethernet".into()));
                }
            }
        }
        let wireless = self.try_run(
            "nmcli",
            &[
                "-t",
                "-f",
                "ACTIVE,SSID,SIGNAL",
                "device",
                "wifi",
                "list",
                "--rescan",
                "no",
            ],
        );
        let mut networks: Vec<Value> = Vec::new();
        for line in wireless.unwrap_or_default().lines() {
            let fields = split_nmcli(line);
            if fields.len() < 3 {
                continue;
            }
            if !fields[1].is_empty()
                && !networks
                    .iter()
                    .any(|item| item["ssid"].as_str() == Some(fields[1].as_str()))
            {
                let strength = fields[2]
                    .parse::<i64>()
                    .map(|value| value.clamp(0, 100))
                    .unwrap_or(0);
                networks.push(serde_json::json!({
                    "ssid": fields[1], "strength": strength, "connected": fields[0] == "yes",
                }));
            }
            if fields[0] == "yes" {
                object.insert("connected".into(), Value::Bool(true));
                object.insert("ssid".into(), Value::String(fields[1].clone()));
                if let Ok(strength) = fields[2].parse::<i64>() {
                    object.insert("strength".into(), serde_json::json!(strength.clamp(0, 100)));
                }
            }
        }
        networks.sort_by_key(|item| {
            let connected = item["connected"].as_bool().unwrap_or(false);
            let strength = item["strength"].as_i64().unwrap_or(0);
            (!connected, -strength)
        });
        networks.truncate(12);
        object.insert("networks".into(), Value::Array(networks));
        result
    }

    pub fn bluetooth(&self, preferred_adapter: &str) -> Value {
        let blocks = self.rfkill_bluetooth_blocks();
        let mut result = serde_json::json!({
            "available": false, "enabled": false, "adapters": [], "devices": [],
            "adapter": "",
            "softBlocked": blocks.iter().any(|block| block.soft),
            "hardBlocked": blocks.iter().any(|block| block.hard),
        });
        let object = result.as_object_mut().unwrap();
        if let Some(objects) = self.bluez_objects() {
            let mut paths: Vec<&String> = objects.keys().collect();
            paths.sort();
            for path in &paths {
                let Some(interfaces) = objects.get(*path) else {
                    continue;
                };
                if !is_hci_path(path) {
                    continue;
                }
                let Some(adapter) = interfaces.get("org.bluez.Adapter1") else {
                    continue;
                };
                let alias = bluez_value(
                    adapter,
                    "Alias",
                    Value::String(path.rsplit('/').next().unwrap().to_string()),
                );
                object["adapters"].as_array_mut().unwrap().push(serde_json::json!({
                    "path": path,
                    "name": alias,
                    "enabled": bluez_value(adapter, "Powered", Value::Bool(false)) == Value::Bool(true),
                }));
            }
            let adapters = object["adapters"].as_array().unwrap().clone();
            let selected = adapters
                .iter()
                .find(|item| item["path"].as_str() == Some(preferred_adapter))
                .or_else(|| adapters.first())
                .cloned();
            if let Some(selected) = selected {
                let adapter_path = selected["path"].as_str().unwrap_or_default().to_string();
                object.insert("available".into(), Value::Bool(true));
                object.insert("enabled".into(), selected["enabled"].clone());
                object.insert("adapter".into(), Value::String(adapter_path.clone()));
                let prefix = format!("{adapter_path}/dev_");
                for path in &paths {
                    let Some(interfaces) = objects.get(*path) else {
                        continue;
                    };
                    let Some(device) = interfaces.get("org.bluez.Device1") else {
                        continue;
                    };
                    if !path.starts_with(&prefix) {
                        continue;
                    }
                    let paired = bluez_value(device, "Paired", Value::Bool(false));
                    let connected_value = bluez_value(device, "Connected", Value::Bool(false));
                    if py_truthy(&paired) || py_truthy(&connected_value) {
                        object["devices"].as_array_mut().unwrap().push(serde_json::json!({
                            "path": path,
                            "name": bluez_value(device, "Alias", Value::String("Bluetooth device".into())),
                            "connected": connected_value == Value::Bool(true),
                        }));
                    }
                }
            }
            return result;
        }
        if let Some(output) = self.try_run("bluetoothctl", &["show"]) {
            if output.lines().any(controller_line) {
                object.insert("available".into(), Value::Bool(true));
                object.insert(
                    "enabled".into(),
                    Value::Bool(regex_controller_powered(&output)),
                );
            }
        }
        result
    }

    pub fn rfkill_bluetooth_blocks(&self) -> Vec<RfkillBlock> {
        let mut blocks = Vec::new();
        let mut paths: Vec<PathBuf> = glob_entries(&self.sysfs.join("class/rfkill"), "rfkill*");
        paths.sort();
        for path in paths {
            if read(&path.join("type")).as_deref() == Some("bluetooth") {
                blocks.push(RfkillBlock {
                    id: path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .trim_start_matches("rfkill")
                        .to_string(),
                    soft: read(&path.join("soft")).as_deref() == Some("1"),
                    hard: read(&path.join("hard")).as_deref() == Some("1"),
                });
            }
        }
        blocks
    }

    fn bluez_objects(&self) -> Option<Map<String, Value>> {
        let output = self.try_run(
            "busctl",
            &[
                "--system",
                "--json=short",
                "call",
                "org.bluez",
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
            ],
        )?;
        let parsed: Value = serde_json::from_str(&output).ok()?;
        let data = parsed.get("data")?;
        match data {
            Value::Array(items) if items.len() == 1 => items
                .iter()
                .next()
                .and_then(|item| item.as_object().cloned()),
            Value::Object(map) => Some(map.clone()),
            _ => None,
        }
    }

    pub fn audio(&self, source: &str) -> Value {
        let output = self.try_run("wpctl", &["get-volume", source]);
        let Some(output) = output else {
            return serde_json::json!({"available": false, "volume": null, "muted": false});
        };
        let Some(value) = extract_volume(&output) else {
            return serde_json::json!({"available": false, "volume": null, "muted": false});
        };
        if !value.is_finite() || value < 0.0 {
            return serde_json::json!({"available": false, "volume": null, "muted": false});
        }
        serde_json::json!({
            "available": true,
            "volume": py_round_int(value * 100.0),
            "muted": output.contains("[MUTED]"),
        })
    }

    /// Backlight-only class excludes keyboard and wireless status LEDs.
    pub fn brightness(&self) -> Value {
        let output = self.try_run("brightnessctl", &["--class=backlight", "-m"]);
        for line in output.unwrap_or_default().lines() {
            if let Some(percent) = extract_brightness(line) {
                return serde_json::json!({"available": true, "percent": percent.min(100)});
            }
        }
        serde_json::json!({"available": false, "percent": null})
    }

    fn read_power_profile(&self, provider: &[String]) -> Option<String> {
        let value = if provider.first().map(String::as_str) == Some("dbus") {
            let output = self.try_run(
                "busctl",
                &[
                    "--system",
                    "get-property",
                    &provider[1],
                    &provider[2],
                    &provider[3],
                    "ActiveProfile",
                ],
            )?;
            // busctl prints the D-Bus signature followed by its quoted value.
            let fields = crate::pyutil::shlex_split(&output).ok()?;
            if fields.len() != 2 || fields[0] != "s" {
                return None;
            }
            fields[1].clone()
        } else {
            self.try_run("powerprofilesctl", &["get"])?
        };
        POWER_PROFILES.contains(&value.as_str()).then_some(value)
    }

    /// The cached power-profile provider (for the set-property action).
    pub fn provider(&self) -> Option<Vec<String>> {
        self.power_profile_provider.clone()
    }

    /// Fedora uses tuned-ppd; both it and power-profiles-daemon expose the
    /// standard API. Keep the working endpoint first on later samples.
    pub fn power_profile(&mut self) -> Option<String> {
        let mut candidates: Vec<Vec<String>> = POWER_PROFILE_SERVICES
            .iter()
            .map(|(service, path, interface)| {
                vec![
                    "dbus".to_string(),
                    service.to_string(),
                    path.to_string(),
                    interface.to_string(),
                ]
            })
            .collect();
        candidates.push(vec!["cli".to_string()]);
        if let Some(cached) = &self.power_profile_provider {
            if let Some(position) = candidates.iter().position(|candidate| candidate == cached) {
                let found = candidates.remove(position);
                candidates.insert(0, found);
            }
        }
        for provider in &candidates {
            if let Some(value) = self.read_power_profile(provider) {
                self.power_profile_provider = Some(provider.clone());
                return Some(value);
            }
        }
        self.power_profile_provider = None;
        None
    }

    /// Read supported recorder processes belonging to this session's user.
    pub fn recording(&self) -> Value {
        let supported: &[(&str, &str)] = &[
            ("wf-recorder", "wf-recorder"),
            ("gpu-screen-recorder", "GPU Screen Recorder"),
        ];
        let Ok(uptime_text) = std::fs::read_to_string(self.proc.join("uptime")) else {
            return serde_json::json!({"active": false});
        };
        let Ok(uptime) = uptime_text
            .split_whitespace()
            .next()
            .unwrap_or("")
            .parse::<f64>()
        else {
            return serde_json::json!({"active": false});
        };
        const TICKS: f64 = 100.0; // USER_HZ on Linux; matches os.sysconf("SC_CLK_TCK").
        let uid = crate::pyutil::current_uid();
        let mut best: Option<(i64, &str)> = None;
        let Ok(entries) = std::fs::read_dir(&self.proc) else {
            return serde_json::json!({"active": false});
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(entry.path()) else {
                continue;
            };
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != uid {
                continue;
            }
            let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else {
                continue;
            };
            let exe_name = exe
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Some(&(_, display)) = supported.iter().find(|(process, _)| *process == exe_name)
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(after_comm) = stat.rsplit(')').next() else {
                continue;
            };
            let fields: Vec<&str> = after_comm.split_whitespace().collect();
            if fields.first() == Some(&"Z") {
                continue;
            }
            let Some(starttime) = fields.get(19).and_then(|v| v.parse::<f64>().ok()) else {
                continue;
            };
            let elapsed = ((uptime - starttime / TICKS).max(0.0)) as i64;
            if best.is_none() || best.is_some_and(|(current, _)| elapsed > current) {
                best = Some((elapsed, display));
            }
        }
        match best {
            Some((elapsed, name)) => {
                serde_json::json!({"active": true, "name": name, "elapsed": elapsed})
            }
            None => serde_json::json!({"active": false}),
        }
    }
}

#[derive(Clone)]
pub struct RfkillBlock {
    pub id: String,
    pub soft: bool,
    pub hard: bool,
}

fn is_hci_path(path: &str) -> bool {
    let rest = path.strip_prefix("/org/bluez/hci").unwrap_or("");
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// `bluez_value`: unwrap the `{"type": …, "data": …}` busctl envelope,
/// falling back to `default` exactly like the Python helper.
pub fn bluez_value(properties: &Value, key: &str, default: Value) -> Value {
    let value = properties
        .get(key)
        .cloned()
        .unwrap_or_else(|| default.clone());
    match value {
        Value::Object(map) => map.get("data").cloned().unwrap_or(default),
        other => other,
    }
}

/// Python truthiness for JSON values (`or` chains in the Python source).
pub fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(boolean) => *boolean,
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `^\\s*Controller\\s` multiline search without a regex engine.
fn controller_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("Controller")
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// `Powered:\s+yes` search without a regex engine.
fn regex_controller_powered(output: &str) -> bool {
    output
        .lines()
        .any(|line| match line.split_once("Powered:") {
            Some((_, rest)) => rest.trim_start() == "yes",
            None => false,
        })
}

/// `Volume:\s+([0-9.]+)` search.
fn extract_volume(output: &str) -> Option<f64> {
    let position = output.find("Volume:")?;
    let rest = output[position + "Volume:".len()..].trim_start();
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse::<f64>().ok()
}

/// `,(\d+)%,` search within one brightnessctl line.
fn extract_brightness(line: &str) -> Option<i64> {
    let chars = line.char_indices();
    for (index, _) in chars {
        if line.as_bytes().get(index) == Some(&b',')
            && line[index + 1..].starts_with(|c: char| c.is_ascii_digit())
        {
            let rest = &line[index + 1..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = &rest[digits.len()..];
            if after.starts_with("%,") {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// Non-hidden children of `directory` matching a `prefix*` glob pattern.
fn glob_entries(directory: &Path, pattern: &str) -> Vec<PathBuf> {
    let prefix = pattern.trim_end_matches('*');
    let Ok(read_dir) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    read_dir
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            !name.starts_with('.') && name.starts_with(prefix)
        })
        .map(|entry| entry.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmcli_splitter_matches_python() {
        assert_eq!(
            split_nmcli("yes:Home\\:Net:88"),
            vec!["yes", "Home:Net", "88"]
        );
        assert_eq!(split_nmcli("a\\:"), vec!["a:"]);
        assert_eq!(split_nmcli("trailing\\"), vec!["trailing\\"]);
        assert_eq!(split_nmcli(""), vec![""]);
    }

    #[test]
    fn volume_and_brightness_extraction() {
        assert_eq!(extract_volume("Volume: 0.45\n"), Some(0.45));
        assert_eq!(extract_volume("Volume: 0.45 [MUTED]"), Some(0.45));
        assert_eq!(extract_volume("Muted"), None);
        assert_eq!(
            extract_brightness("amd_backlight,backlight,19712,43807,45%,1"),
            Some(45)
        );
        // The regex matches anywhere in the line, exactly like Python.
        assert_eq!(extract_brightness("no,45%,percent"), Some(45));
        assert_eq!(extract_brightness("plain,text,45%"), None);
        assert_eq!(extract_brightness("no,percent,here"), None);
    }

    #[test]
    fn hci_path_regex() {
        assert!(is_hci_path("/org/bluez/hci0"));
        assert!(is_hci_path("/org/bluez/hci12"));
        assert!(!is_hci_path("/org/bluez/hci"));
        assert!(!is_hci_path("/org/bluez/hci0/dev_X"));
    }

    #[test]
    fn controller_powered_detection() {
        assert!(regex_controller_powered(
            "Controller 00:11\n\tPowered: yes\n"
        ));
        assert!(!regex_controller_powered("Powered: no"));
        assert!(!regex_controller_powered("Discovering: yes"));
    }
}
