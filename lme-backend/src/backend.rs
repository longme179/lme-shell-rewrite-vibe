//! `DesktopBackend` port: snapshot assembly, system-probe cache window,
//! demo state, and the settings-app mode.

use crate::apps::Applications;
use crate::charging::ChargeControl;
use crate::niri;
use crate::prefs::Preferences;
use crate::probes::SystemProbes;
use crate::runner::CommandRunner;
use crate::weather::WeatherClient;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const SYSTEM_CACHE_SECONDS: f64 = 1.5;
const APP_RESCAN_SECONDS: f64 = 60.0;
pub const SETTINGS_ACTIONS: &[&str] = &[
    "setting",
    "wallpaper",
    "refresh",
    "weatherSearch",
    "weatherDetect",
    "weatherSelect",
];
pub const CACHE_INVALIDATING_ACTIONS: &[&str] = &[
    "refresh",
    "volume",
    "mute",
    "micVolume",
    "micMute",
    "brightness",
    "wifiToggle",
    "bluetoothToggle",
    "bluetoothDevice",
    "powerProfile",
];
pub type ChargeSlot = Arc<Mutex<Option<Result<String, String>>>>;

pub struct DesktopBackend {
    pub demo: bool,
    pub settings_only: bool,
    pub preferences: Preferences,
    pub applications: Applications,
    pub weather: WeatherClient,
    pub weather_fetch: bool,
    pub probes: SystemProbes,
    pub charger: ChargeControl,
    use_niri_socket: bool,
    pub apps: Vec<Value>,
    pub last_app_scan: Instant,
    system_cache: Option<Map<String, Value>>,
    last_system_sample: Option<Instant>,
    pub result_sequence: i64,
    result_unemitted: bool,
    pub last_result: Map<String, Value>,
    pub niri: Value,
    demo_state: Option<Map<String, Value>>,
    charge_slot: Option<ChargeSlot>,
}

impl DesktopBackend {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        demo: bool,
        settings_only: bool,
        preferences_path: Option<PathBuf>,
        apps_paths: Option<Vec<PathBuf>>,
        proc_root: PathBuf,
        sysfs_root: PathBuf,
        weather_fetch: bool,
        use_niri_socket: bool,
        weather: Option<WeatherClient>,
    ) -> Self {
        let preferences =
            Preferences::new(preferences_path.unwrap_or_else(Preferences::default_path));
        let mut applications = Applications::new(apps_paths);
        let apps = if demo || settings_only {
            Vec::new()
        } else {
            applications.scan()
        };
        Self {
            demo,
            settings_only,
            preferences,
            applications,
            weather: weather
                .unwrap_or_else(|| WeatherClient::new(Arc::new(crate::weather::HttpFetch::new()))),
            weather_fetch,
            probes: SystemProbes::new(CommandRunner::new(), proc_root, sysfs_root),
            charger: ChargeControl::new(PathBuf::from("/sys")),
            use_niri_socket,
            apps,
            last_app_scan: Instant::now(),
            system_cache: None,
            last_system_sample: None,
            result_sequence: 0,
            result_unemitted: false,
            last_result: json!({
                "ok": true, "action": "ready",
                "message": "Backend ready", "sequence": 0,
            })
            .as_object()
            .cloned()
            .unwrap(),
            niri: niri::unavailable(),
            demo_state: None,
            charge_slot: None,
        }
    }

    pub fn uses_socket(&self) -> bool {
        self.use_niri_socket
    }

    pub fn set_niri_state(&mut self, state: Value) {
        self.niri = state;
    }

    pub fn set_demo_state(&mut self, state: Map<String, Value>) {
        self.demo_state = Some(state);
    }

    pub fn charge_job_running(&self) -> bool {
        self.charge_slot.is_some()
    }

    /// `charging()` port (state + canSet + busy shaping).
    pub fn charging(&self, busy: bool) -> Value {
        let mut state = self.charger.state();
        let available = state["available"].as_bool().unwrap_or(false);
        let writable = state["writable"].as_bool().unwrap_or(false);
        let can_set = available
            && (writable
                || (self.probes.runner.available("pkexec") && crate::charging::trusted_helper()));
        let object = state.as_object_mut().unwrap();
        object.insert("canSet".into(), Value::Bool(can_set));
        object.insert("busy".into(), Value::Bool(busy));
        if available && !can_set {
            object.insert(
                "reason".into(),
                Value::String(
                    "Install the administrator charge helper. See docs/CHARGING.md.".to_string(),
                ),
            );
        }
        state
    }

    /// `finish_charge` port: fold a finished background job into lastResult.
    fn finish_charge(&mut self) {
        let Some(slot) = &self.charge_slot else {
            return;
        };
        let Some(outcome) = slot.lock().ok().and_then(|mut guard| guard.take()) else {
            return;
        };
        self.result_sequence += 1;
        self.last_result = match outcome {
            Ok(message) => json!({
                "ok": true, "action": "chargeMode", "message": message,
            }),
            Err(message) => json!({
                "ok": false, "action": "chargeMode",
                "message": message.chars().take(400).collect::<String>(),
            }),
        }
        .as_object()
        .cloned()
        .unwrap();
        self.last_result
            .insert("sequence".into(), json!(self.result_sequence));
        self.charge_slot = None;
        self.system_cache = None;
    }

    /// `handle` port: one input line → one updated lastResult.
    pub fn handle(&mut self, request: Value) -> Value {
        self.result_sequence += 1;
        self.result_unemitted = true;
        let action = request
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let object = request.as_object();
        let outcome: Result<crate::actions::PerformOutcome, String> = match object {
            None => Err("Input must be an object with an action".to_string()),
            Some(_request) if action.is_empty() => {
                Err("Input must be an object with an action".to_string())
            }
            Some(_request)
                if self.settings_only && !SETTINGS_ACTIONS.contains(&action.as_str()) =>
            {
                Err("This action is unavailable in the settings app".to_string())
            }
            Some(request) if self.demo => crate::actions::perform_demo(self, &action, request)
                .map(|_| crate::actions::PerformOutcome::Demo),
            Some(request) => crate::actions::perform(self, &action, request),
        };
        match outcome {
            Ok(crate::actions::PerformOutcome::Demo) => {
                self.last_result = json!({
                    "ok": true, "action": action,
                    "message": "Demo preview: no system command executed",
                })
                .as_object()
                .cloned()
                .unwrap();
            }
            Ok(crate::actions::PerformOutcome::ChargeSubmit(value)) => {
                let slot: ChargeSlot = Arc::new(Mutex::new(None));
                let worker_slot = Arc::clone(&slot);
                let runner = CommandRunner::new();
                let charger = self.charger.clone();
                std::thread::Builder::new()
                    .name("lme-charge".to_string())
                    .spawn(move || {
                        let result = crate::actions::set_charge_static(&runner, &charger, value);
                        if let Ok(mut guard) = worker_slot.lock() {
                            *guard = Some(result);
                        }
                    })
                    .ok();
                self.charge_slot = Some(slot);
                self.system_cache = None;
                self.last_result = json!({
                    "ok": true, "action": action,
                    "message": "Changing charge mode; administrator authentication may be required",
                    "pending": true,
                })
                .as_object()
                .cloned()
                .unwrap();
            }
            Ok(crate::actions::PerformOutcome::Message(message)) => {
                if CACHE_INVALIDATING_ACTIONS.contains(&action.as_str()) {
                    self.system_cache = None;
                }
                self.last_result = json!({"ok": true, "action": action, "message": message})
                    .as_object()
                    .cloned()
                    .unwrap();
            }
            Err(message) => {
                self.last_result = json!({
                    "ok": false, "action": action,
                    "message": message.chars().take(400).collect::<String>(),
                })
                .as_object()
                .cloned()
                .unwrap();
                if ["bluetoothToggle", "bluetoothDevice"].contains(&action.as_str()) {
                    self.system_cache = None;
                }
            }
        }
        self.last_result
            .insert("sequence".into(), json!(self.result_sequence));
        Value::Object(self.last_result.clone())
    }

    pub fn invalid_input(&mut self, message: &str) {
        self.last_result = json!({"ok": false, "action": "", "message": message})
            .as_object()
            .cloned()
            .unwrap();
        self.last_result
            .insert("sequence".into(), json!(self.result_sequence));
    }

    pub fn snapshot(&mut self) -> Value {
        if self.demo {
            return self.demo_snapshot();
        }
        self.preferences.reload(false);
        // Emit each input acknowledgement before a completed background job
        // replaces lastResult, matching the Python ordering guarantee.
        if !self.result_unemitted {
            self.finish_charge();
        }
        self.result_unemitted = false;
        let allow_fetch = self.weather_fetch && !self.settings_only;
        let weather = self.weather.snapshot(&self.preferences.values, allow_fetch);
        if self.settings_only {
            return json!({
                "apps": [], "niri": self.niri.clone(), "system": {},
                "weather": weather, "preferences": Value::Object(self.preferences.values.clone()),
                "lastResult": Value::Object(self.last_result.clone()),
            });
        }
        if self.last_app_scan.elapsed().as_secs_f64() > APP_RESCAN_SECONDS {
            self.apps = self.applications.scan();
            self.last_app_scan = Instant::now();
        }
        self.niri = niri::niri_state(&self.probes.runner, self.use_niri_socket);
        // Window/focus event bursts must not cause expensive hardware probes.
        let expired = self
            .last_system_sample
            .is_none_or(|sample| sample.elapsed().as_secs_f64() >= SYSTEM_CACHE_SECONDS);
        if self.system_cache.is_none() || expired {
            let mut system = self.probes.cpu_memory();
            system.insert("tempC".into(), self.probes.temperature());
            system.insert("battery".into(), self.probes.battery());
            system.insert("network".into(), self.probes.network());
            system.insert(
                "bluetooth".into(),
                self.probes.bluetooth(
                    self.preferences
                        .values
                        .get("bluetoothAdapter")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
            );
            system.insert("audio".into(), self.probes.audio("@DEFAULT_AUDIO_SINK@"));
            system.insert(
                "microphone".into(),
                self.probes.audio("@DEFAULT_AUDIO_SOURCE@"),
            );
            system.insert("brightness".into(), self.probes.brightness());
            system.insert(
                "powerProfile".into(),
                match self.probes.power_profile() {
                    Some(profile) => Value::String(profile),
                    None => Value::Null,
                },
            );
            system.insert("charging".into(), self.charging(self.charge_job_running()));
            system.insert("recording".into(), self.probes.recording());
            self.system_cache = Some(system);
            self.last_system_sample = Some(Instant::now());
        }
        json!({
            "apps": Value::Array(self.apps.clone()),
            "niri": self.niri.clone(),
            "system": Value::Object(self.system_cache.clone().unwrap_or_default()),
            "weather": weather,
            "preferences": Value::Object(self.preferences.values.clone()),
            "lastResult": Value::Object(self.last_result.clone()),
        })
    }

    pub fn demo_snapshot(&mut self) -> Value {
        // Python builds the base state, then immediately re-enters this
        // method so the first emit already carries the preferences-derived
        // updates. Structure it the same way here.
        if self.demo_state.is_none() {
            let state = demo_state();
            self.demo_state = Some(state);
        }
        let Some(state) = &mut self.demo_state else {
            unreachable!("demo state was just created")
        };
        {
            state.insert(
                "preferences".into(),
                Value::Object(self.preferences.values.clone()),
            );
            state.insert("lastResult".into(), Value::Object(self.last_result.clone()));
            let unit_is_fahrenheit = self
                .preferences
                .values
                .get("weatherUnit")
                .and_then(Value::as_str)
                == Some("fahrenheit");
            let automatic = self
                .preferences
                .values
                .get("weatherAutoLocation")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let weather = state.get_mut("weather").unwrap().as_object_mut().unwrap();
            weather.insert("automatic".into(), Value::Bool(automatic));
            weather.insert(
                "unit".into(),
                Value::String(if unit_is_fahrenheit { "°F" } else { "°C" }.to_string()),
            );
            weather.insert(
                "temperature".into(),
                json!(if unit_is_fahrenheit { 84 } else { 29 }),
            );
            weather.insert(
                "feelsLike".into(),
                json!(if unit_is_fahrenheit { 90 } else { 32 }),
            );
            weather.insert(
                "wind".into(),
                json!(if unit_is_fahrenheit { 6 } else { 10 }),
            );
            weather.insert(
                "windUnit".into(),
                Value::String(if unit_is_fahrenheit { "mph" } else { "km/h" }.to_string()),
            );
            let days: [(&str, i64, i64, i64); 3] = [
                ("2026-10-11", 25, 31, 30),
                ("2026-10-12", 24, 30, 65),
                ("2026-10-13", 25, 32, 20),
            ];
            let daily: Vec<Value> = days
                .iter()
                .map(|(date, low, high, rain)| {
                    let (low, high) = if unit_is_fahrenheit {
                        (
                            crate::pyutil::py_round_int(*low as f64 * 1.8 + 32.0),
                            crate::pyutil::py_round_int(*high as f64 * 1.8 + 32.0),
                        )
                    } else {
                        (*low, *high)
                    };
                    json!({"date": date, "min": low, "max": high, "rainChance": rain})
                })
                .collect();
            weather.insert("daily".into(), Value::Array(daily));
            let location = if automatic {
                json!({"name": "Ho Chi Minh City", "country": "Vietnam", "latitude": 10.82, "longitude": 106.63})
            } else {
                self.preferences
                    .values
                    .get("weatherLocation")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            };
            weather.insert("location".into(), location);
        }
        Value::Object(state.clone())
    }
}

fn demo_state() -> Map<String, Value> {
    let mut state = Map::new();
    state.insert(
        "apps".into(),
        json!([
            {"id": "org.mozilla.firefox.desktop", "name": "Firefox", "icon": "firefox", "path": "", "terminal": false},
            {"id": "kitty.desktop", "name": "Kitty", "icon": "kitty", "path": "", "terminal": false},
            {"id": "org.gnome.Nautilus.desktop", "name": "Files", "icon": "org.gnome.Nautilus", "path": "", "terminal": false},
        ]),
    );
    state.insert(
        "niri".into(),
        json!({
            "available": true,
            "workspaces": [
                {"id": 1, "idx": 1, "name": null, "output": "eDP-1", "is_focused": true, "is_active": true},
                {"id": 2, "idx": 2, "name": null, "output": "eDP-1", "is_focused": false, "is_active": false},
                {"id": 3, "idx": 3, "name": null, "output": "eDP-1", "is_focused": false, "is_active": false},
                {"id": 4, "idx": 4, "name": null, "output": "eDP-1", "is_focused": false, "is_active": false},
            ],
            "windows": [
                {"id": 1, "title": "LME Shell — Preview", "app_id": "firefox", "workspace_id": 1, "is_focused": true},
                {"id": 2, "title": "Terminal · project", "app_id": "kitty", "workspace_id": 1, "is_focused": false},
                {"id": 3, "title": "Documents", "app_id": "org.gnome.Nautilus", "workspace_id": 2, "is_focused": false},
            ],
        }),
    );
    state.insert(
        "system".into(),
        json!({
            "cpu": 12.4, "memoryPercent": 34.2, "memoryUsedGiB": 5.47, "memoryTotalGiB": 16,
            "tempC": 47,
            "battery": {"available": true, "percent": 82, "charging": false},
            "network": {"available": true, "enabled": true, "connected": true, "ssid": "Home Wi-Fi", "strength": 88,
                        "networks": [{"ssid": "Home Wi-Fi", "strength": 88, "connected": true},
                                      {"ssid": "Guest", "strength": 62, "connected": false}]},
            "bluetooth": {"available": true, "enabled": true, "softBlocked": false, "hardBlocked": false, "adapter": "/org/bluez/hci0",
                          "adapters": [{"path": "/org/bluez/hci0", "name": "Laptop Bluetooth", "enabled": true}],
                          "devices": [{"path": "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF", "name": "Headphones", "connected": true}]},
            "audio": {"available": true, "volume": 42, "muted": false},
            "microphone": {"available": true, "volume": 60, "muted": false},
            "brightness": {"available": true, "percent": 65},
            "powerProfile": "balanced",
            "charging": {"available": true, "canSet": true, "kind": "threshold", "mode": "conservation",
                         "limitPercent": 85, "supports85": true, "reason": "", "busy": false},
        }),
    );
    let mut weather = Map::new();
    weather.insert("available".into(), json!(true));
    weather.insert("configured".into(), json!(true));
    weather.insert("automatic".into(), json!(true));
    weather.insert("loading".into(), json!(false));
    weather.insert("stale".into(), json!(false));
    weather.insert("error".into(), json!(""));
    weather.insert(
        "location".into(),
        json!({"name": "Ho Chi Minh City", "country": "Vietnam", "latitude": 10.82, "longitude": 106.63}),
    );
    weather.insert("temperature".into(), json!(29));
    weather.insert("feelsLike".into(), json!(32));
    weather.insert("humidity".into(), json!(76));
    weather.insert("wind".into(), json!(10));
    weather.insert("windUnit".into(), json!("km/h"));
    weather.insert("unit".into(), json!("°C"));
    weather.insert("condition".into(), json!("Partly cloudy"));
    weather.insert("icon".into(), json!("weather-cloud"));
    weather.insert("updatedAt".into(), json!(crate::wall_clock_now()));
    weather.insert(
        "daily".into(),
        json!([
            {"date": "2026-10-11", "min": 25, "max": 31, "rainChance": 30},
            {"date": "2026-10-12", "min": 24, "max": 30, "rainChance": 65},
            {"date": "2026-10-13", "min": 25, "max": 32, "rainChance": 20},
        ]),
    );
    weather.insert(
        "search".into(),
        json!({"query": "", "loading": false, "results": [], "error": ""}),
    );
    state.insert("weather".into(), Value::Object(weather));
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend_with_root(root: &std::path::Path) -> DesktopBackend {
        DesktopBackend::new(
            false,
            false,
            Some(root.join("config/settings.json")),
            Some(vec![root.join("share/applications")]),
            root.join("proc"),
            root.join("sys"),
            false,
            false,
            None,
        )
    }

    #[test]
    fn settings_only_snapshot_shape() {
        let root = std::env::temp_dir().join(format!("lme-backend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/lme-shell")).unwrap();
        let mut backend = backend_with_root(&root);
        backend.settings_only = true;
        let snapshot = backend.snapshot();
        assert_eq!(snapshot["apps"], json!([]));
        assert_eq!(snapshot["system"], json!({}));
        assert_eq!(snapshot["niri"]["available"], json!(false));
        assert_eq!(snapshot["lastResult"]["action"], json!("ready"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn demo_snapshot_structure() {
        let root = std::env::temp_dir().join(format!("lme-demo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/lme-shell")).unwrap();
        let mut backend = backend_with_root(&root);
        backend.demo = true;
        let snapshot = backend.snapshot();
        assert_eq!(snapshot["niri"]["available"], json!(true));
        assert_eq!(snapshot["apps"].as_array().unwrap().len(), 3);
        assert_eq!(snapshot["system"]["battery"]["percent"], json!(82));
        assert_eq!(snapshot["weather"]["temperature"], json!(29));
        // Second call updates in place and stays structurally identical.
        let again = backend.snapshot();
        assert_eq!(again["system"]["cpu"], json!(12.4));
        let _ = std::fs::remove_dir_all(&root);
    }
}
