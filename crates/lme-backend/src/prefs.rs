//! `settings.json` preferences: defaults, validation, stat-stamp reload and
//! the flock-protected atomic merge, byte-compatible with the Python
//! implementation (including `ensure_ascii` escapes and key order).

use serde_json::{Map, Value};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const BAR_MODULES: &[&str] = &[
    "launcher",
    "overview",
    "workspaces",
    "windowTitle",
    "media",
    "resources",
    "tray",
    "notifications",
    "audio",
    "network",
    "battery",
    "clock",
    "settings",
    "clipboard",
    "activity",
];

/// Insertion-ordered defaults, same key order as `DEFAULT_PREFERENCES`.
pub fn default_preferences() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("language".into(), Value::String("en".into()));
    map.insert("animations".into(), Value::Bool(true));
    map.insert("opacity".into(), serde_json::json!(0.90));
    map.insert("accent".into(), Value::String("#9580ff".into()));
    map.insert("wallpaper".into(), Value::String(String::new()));
    map.insert("terminal".into(), Value::String("kitty".into()));
    map.insert("bluetoothAdapter".into(), Value::String(String::new()));
    map.insert("tasks".into(), Value::Array(Vec::new()));
    map.insert("weatherAutoLocation".into(), Value::Bool(true));
    map.insert("weatherLocation".into(), serde_json::json!({}));
    map.insert("weatherUnit".into(), Value::String("celsius".into()));
    map.insert("activityEnabled".into(), Value::Bool(true));
    map.insert("clipboardEnabled".into(), Value::Bool(true));
    map.insert("clipboardImages".into(), Value::Bool(true));
    map.insert("clipboardLimit".into(), serde_json::json!(100));
    map.insert("barStyle".into(), Value::String("connected".into()));
    map.insert("barFontSize".into(), serde_json::json!(13));
    let layout = serde_json::json!({
        "left": ["launcher", "overview", "workspaces"],
        "center": ["activity"],
        "right": ["clipboard", "tray", "notifications", "audio", "network", "battery", "clock", "settings"],
    });
    map.insert("barLayout".into(), layout);
    map
}

fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

fn json_finite_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// Port of `weather.location()`.
pub fn validate_location(value: &Value) -> Result<Value, String> {
    if value.as_object().is_some_and(|m| m.is_empty()) {
        return Ok(serde_json::json!({}));
    }
    let object = value
        .as_object()
        .ok_or_else(|| "A location needs a name, latitude and longitude.".to_string())?;
    for key in ["name", "latitude", "longitude"] {
        if !object.contains_key(key) {
            return Err("A location needs a name, latitude and longitude.".to_string());
        }
    }
    let name = object["name"]
        .as_str()
        .ok_or_else(|| "A location needs a name, latitude and longitude.".to_string())?;
    let country = object.get("country").and_then(Value::as_str).unwrap_or("");
    let name_stripped = name.trim();
    if name_stripped.is_empty() || name_stripped.len() > 120 || country.len() > 100 {
        return Err("Invalid location name.".to_string());
    }
    for (key, bound) in [("latitude", 90.0), ("longitude", 180.0)] {
        let coordinate = json_finite_number(&object[key])
            .ok_or_else(|| "Invalid location coordinates.".to_string())?;
        if !(-bound..=bound).contains(&coordinate) {
            return Err("Invalid location coordinates.".to_string());
        }
    }
    // round(x, 6) keeps the JSON number type: ints stay ints.
    let rounded = |key: &str| -> Value {
        match &object[key] {
            Value::Number(n) if n.is_i64() || n.is_u64() => object[key].clone(),
            Value::Number(n) => {
                let value = crate::pyutil::py_round_nd(n.as_f64().unwrap_or(0.0), 6);
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    };
    Ok(serde_json::json!({
        "name": name_stripped,
        "country": country.trim(),
        "latitude": rounded("latitude"),
        "longitude": rounded("longitude"),
    }))
}

/// Port of `validate_preference` (same error strings, same type strictness).
pub fn validate_preference(key: &str, value: &Value) -> Result<Value, String> {
    let invalid = || format!("Invalid preference: {key}");
    match key {
        "activityEnabled" | "clipboardEnabled" | "clipboardImages" => {
            value.as_bool().ok_or_else(invalid).map(Value::Bool)
        }
        "clipboardLimit" => {
            let limit = json_int(value).ok_or_else(invalid)?;
            if (20..=500).contains(&limit) {
                Ok(serde_json::json!(limit))
            } else {
                Err(invalid())
            }
        }
        "barStyle" => {
            let style = value.as_str().ok_or_else(invalid)?;
            if matches!(style, "islands" | "connected") {
                Ok(Value::String(style.to_string()))
            } else {
                Err(invalid())
            }
        }
        "barFontSize" => {
            let size = json_int(value).ok_or_else(invalid)?;
            if (12..=16).contains(&size) {
                Ok(serde_json::json!(size))
            } else {
                Err(invalid())
            }
        }
        "barLayout" => {
            let object = value
                .as_object()
                .ok_or_else(|| "Invalid bar zone".to_string())?;
            if object.len() != 3
                || !object
                    .keys()
                    .all(|k| ["left", "center", "right"].contains(&k.as_str()))
            {
                return Err("Invalid bar zone".to_string());
            }
            let mut normalized = Map::new();
            for zone in ["left", "center", "right"] {
                let modules = object
                    .get(zone)
                    .and_then(Value::as_array)
                    .ok_or_else(|| "Invalid bar zone".to_string())?;
                if modules.len() > BAR_MODULES.len() {
                    return Err("Invalid bar zone".to_string());
                }
                let mut seen = std::collections::HashSet::new();
                let mut names = Vec::new();
                for module in modules {
                    let name = module
                        .as_str()
                        .ok_or_else(|| "Unknown or duplicate bar module".to_string())?;
                    if !BAR_MODULES.contains(&name) || !seen.insert(name) {
                        return Err("Unknown or duplicate bar module".to_string());
                    }
                    names.push(Value::String(name.to_string()));
                }
                normalized.insert(zone.to_string(), Value::Array(names));
            }
            Ok(Value::Object(normalized))
        }
        "animations" | "weatherAutoLocation" => {
            value.as_bool().ok_or_else(invalid).map(Value::Bool)
        }
        "weatherLocation" => validate_location(value),
        "weatherUnit" => {
            let unit = value.as_str().ok_or_else(invalid)?;
            if matches!(unit, "celsius" | "fahrenheit") {
                Ok(Value::String(unit.to_string()))
            } else {
                Err(invalid())
            }
        }
        "opacity" => {
            let opacity = json_finite_number(value).ok_or_else(invalid)?;
            if (0.60..=1.0).contains(&opacity) {
                Ok(serde_json::json!(opacity))
            } else {
                Err(invalid())
            }
        }
        "accent" => {
            let accent = value.as_str().ok_or_else(invalid)?;
            let bytes = accent.as_bytes();
            if accent.len() == 7
                && bytes[0] == b'#'
                && bytes[1..].iter().all(|b| b.is_ascii_hexdigit())
            {
                Ok(Value::String(accent.to_lowercase()))
            } else {
                Err(invalid())
            }
        }
        "language" => {
            if value.as_str() == Some("en") {
                Ok(Value::String("en".into()))
            } else {
                Err(invalid())
            }
        }
        "terminal" => {
            let terminal = value.as_str().ok_or_else(invalid)?;
            let valid = !terminal.is_empty()
                && terminal.len() <= 80
                && terminal
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'));
            if valid {
                Ok(Value::String(terminal.to_string()))
            } else {
                Err(invalid())
            }
        }
        "bluetoothAdapter" => {
            let adapter = value.as_str().ok_or_else(invalid)?;
            if adapter.is_empty()
                || (adapter.starts_with("/org/bluez/hci")
                    && adapter["/org/bluez/hci".len()..]
                        .bytes()
                        .all(|b| b.is_ascii_digit()))
            {
                Ok(Value::String(adapter.to_string()))
            } else {
                Err(invalid())
            }
        }
        "tasks" => {
            let tasks = value.as_array().ok_or_else(invalid)?;
            if tasks.len() > 100 {
                return Err(invalid());
            }
            let mut result = Vec::new();
            let mut ids = std::collections::HashSet::new();
            for item in tasks {
                let object = item.as_object().ok_or_else(|| "Invalid task".to_string())?;
                let ident = object.get("id").and_then(Value::as_str);
                let text = object.get("text").and_then(Value::as_str);
                let done = object.get("done").and_then(Value::as_bool);
                let ident = ident.ok_or_else(|| "Invalid task".to_string())?;
                let text = text.ok_or_else(|| "Invalid task".to_string())?;
                let done = done.ok_or_else(|| "Invalid task".to_string())?;
                let valid_id = !ident.is_empty()
                    && ident.len() <= 64
                    && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
                let trimmed = text.trim();
                if !valid_id
                    || !ids.insert(ident.to_string())
                    || trimmed.is_empty()
                    || trimmed.len() > 240
                {
                    return Err("Invalid task".to_string());
                }
                result.push(serde_json::json!({
                    "id": ident, "text": trimmed, "done": done,
                }));
            }
            Ok(Value::Array(result))
        }
        "wallpaper" => {
            let wallpaper = value.as_str().ok_or_else(invalid)?;
            if wallpaper.is_empty()
                || (Path::new(wallpaper).is_absolute() && Path::new(wallpaper).is_file())
            {
                Ok(Value::String(wallpaper.to_string()))
            } else {
                Err(invalid())
            }
        }
        // DND deadline: 0 = off, -1 = until turned off, else epoch
        // milliseconds (mirrors legacy desktop_backend.py).
        "dndUntil" => {
            let v = json_int(value).ok_or_else(invalid)?;
            if (-1..=4_102_444_800_000).contains(&v) {
                Ok(serde_json::json!(v))
            } else {
                Err(invalid())
            }
        }
        // Apps whose popups are suppressed (legacy notificationMutedApps:
        // <= 100 entries after dedupe — the Python backend normalizes
        // duplicates on write, it does not reject them).
        "notificationMutedApps" => {
            let list = value.as_array().ok_or_else(invalid)?;
            if list.len() > 100 {
                return Err(invalid());
            }
            let mut seen = std::collections::HashSet::new();
            let mut names = Vec::new();
            for item in list {
                let name = item.as_str().ok_or_else(invalid)?.trim().to_string();
                if name.is_empty() {
                    return Err(invalid());
                }
                if seen.insert(name.clone()) {
                    names.push(Value::String(name));
                }
            }
            Ok(Value::Array(names))
        }
        _ => Err(invalid()),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct StatStamp {
    mtime_ns: i128,
    size: u64,
    ino: u64,
}

fn stamp_of(metadata: &std::fs::Metadata) -> StatStamp {
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0);
    StatStamp {
        mtime_ns,
        size: metadata.size(),
        ino: metadata.ino(),
    }
}

pub struct Preferences {
    pub path: PathBuf,
    pub values: Map<String, Value>,
    stamp: Option<StatStamp>,
}

impl Preferences {
    pub fn new(path: PathBuf) -> Self {
        let mut preferences = Self {
            path,
            values: default_preferences(),
            stamp: None,
        };
        preferences.reload(false);
        preferences
    }

    pub fn default_path() -> PathBuf {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));
        config.join("lme-shell").join("settings.json")
    }

    /// Observe atomic writes from the standalone settings app or shell.
    pub fn reload(&mut self, force: bool) {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(_) => {
                if self.stamp.is_some() {
                    self.values = default_preferences();
                    self.stamp = None;
                }
                return;
            }
        };
        let stamp = stamp_of(&metadata);
        if !force && Some(&stamp) == self.stamp.as_ref() {
            return;
        }
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            // Retain the last valid snapshot while an external editor writes.
            Err(_) => return,
        };
        let Ok(Value::Object(saved)) = serde_json::from_str(&text) else {
            return;
        };
        let mut values = default_preferences();
        for (key, value) in &saved {
            if let Ok(validated) = validate_preference(key, value) {
                values.insert(key.clone(), validated);
            }
        }
        self.values = values;
        self.stamp = Some(stamp);
    }

    pub fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
        let mut updates = Map::new();
        updates.insert(key.to_string(), value);
        self.update(updates)
    }

    /// Serialize the app and shell's writers, including task updates.
    pub fn update(&mut self, values: Map<String, Value>) -> Result<(), String> {
        let mut validated = Map::new();
        for (key, value) in &values {
            validated.insert(key.clone(), validate_preference(key, value)?);
        }
        self.ensure_parent()?;
        let lock_path = self.path.with_extension("lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| error.to_string())?;
        let _mode = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
        let result = (|| -> Result<(), String> {
            rustix::fs::flock(&lock_file, rustix::fs::FlockOperation::LockExclusive)
                .map_err(|error| error.to_string())?;
            self.reload(true);
            let mut merged = self.values.clone();
            merged.extend(validated);
            self.write_atomic(&merged)
        })();
        drop(lock_file);
        result
    }

    fn ensure_parent(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        Ok(())
    }

    fn write_atomic(&mut self, updated: &Map<String, Value>) -> Result<(), String> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        let unique = format!(
            ".settings-{}.{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temporary = parent.join(unique);
        let result = (|| -> Result<(), String> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
            let body = python_json_pretty(&Value::Object(updated.clone()));
            file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
            file.write_all(b"\n").map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, &self.path).map_err(|e| e.to_string())?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        result?;
        self.values = updated.clone();
        if let Ok(metadata) = std::fs::metadata(&self.path) {
            self.stamp = Some(stamp_of(&metadata));
        }
        Ok(())
    }
}

/// `json.dumps(obj, indent=2)`: two-space indent plus `ensure_ascii=True`
/// non-ASCII escaping, so files stay byte-identical to the Python writer.
pub fn python_json_pretty(value: &Value) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    let mut escaped = String::with_capacity(pretty.len());
    for character in pretty.chars() {
        let code = character as u32;
        if code >= 0x7f {
            if code > 0xffff {
                let adjusted = code - 0x10000;
                escaped.push_str(&format!("\\u{:04x}", 0xd800 + (adjusted >> 10)));
                escaped.push_str(&format!("\\u{:04x}", 0xdc00 + (adjusted & 0x3ff)));
            } else {
                escaped.push_str(&format!("\\u{code:04x}"));
            }
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_python_key_order() {
        let values = default_preferences();
        let keys: Vec<&String> = values.keys().collect();
        assert_eq!(keys.first().map(|s| s.as_str()), Some("language"));
        assert_eq!(keys.last().map(|s| s.as_str()), Some("barLayout"));
        assert_eq!(keys.len(), 18);
    }

    #[test]
    fn validation_type_strictness() {
        assert!(validate_preference("animations", &serde_json::json!(1)).is_err());
        assert!(validate_preference("clipboardLimit", &serde_json::json!(100.0)).is_err());
        assert!(validate_preference("clipboardLimit", &serde_json::json!(100)).is_ok());
        assert!(validate_preference("opacity", &serde_json::json!(0.95)).is_ok());
        assert!(validate_preference("opacity", &serde_json::json!(0.6)).is_ok());
        assert!(validate_preference("opacity", &serde_json::json!(0.59)).is_err());
        assert!(validate_preference("accent", &serde_json::json!("#9580FF")).is_ok());
        assert!(validate_preference("terminal", &serde_json::json!("kitty")).is_ok());
        assert!(validate_preference("terminal", &serde_json::json!("my term")).is_err());
        assert_eq!(
            validate_preference("wallpaper", &serde_json::json!("/nonexistent/x.png")),
            Err("Invalid preference: wallpaper".to_string())
        );
        assert!(validate_preference("unknownKey", &serde_json::json!(1)).is_err());
    }

    #[test]
    fn location_rounds_and_keeps_int_types() {
        let input = serde_json::json!({"name": " Hanoi ", "country": " VN ", "latitude": 21.028511123, "longitude": 105});
        let output = validate_location(&input).unwrap();
        assert_eq!(output["name"], "Hanoi");
        assert_eq!(output["country"], "VN");
        assert_eq!(output["latitude"], serde_json::json!(21.028511));
        assert_eq!(output["longitude"], serde_json::json!(105));
        assert!(output["longitude"].is_i64());
    }

    #[test]
    fn pretty_print_matches_python_bytes() {
        let value = serde_json::json!({"text": "héllo", "n": 0.9});
        assert_eq!(
            python_json_pretty(&value),
            "{\n  \"text\": \"h\\u00e9llo\",\n  \"n\": 0.9\n}"
        );
    }

    #[test]
    fn bar_layout_rejects_duplicates_and_unknown() {
        assert!(validate_preference(
            "barLayout",
            &serde_json::json!({"left": ["clock", "clock"], "center": [], "right": []})
        )
        .is_err());
        assert!(validate_preference(
            "barLayout",
            &serde_json::json!({"left": ["clock", "bogus"], "center": [], "right": []})
        )
        .is_err());
        let ok = validate_preference(
            "barLayout",
            &serde_json::json!({"right": ["clock"], "center": ["tray"], "left": []}),
        )
        .unwrap();
        // Zone order normalizes to left, center, right.
        let keys: Vec<&String> = ok.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["left", "center", "right"]);
    }

    #[test]
    fn update_writes_python_compatible_file() {
        let directory = std::env::temp_dir().join(format!("lme-prefs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let path = directory.join("settings.json");
        let mut preferences = Preferences::new(path.clone());
        preferences.set("opacity", serde_json::json!(0.85)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n  \"language\": \"en\","));
        assert!(text.contains("\"opacity\": 0.85"));
        assert!(text.ends_with("}\n"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Reload observes the same values.
        let reloaded = Preferences::new(path);
        assert_eq!(reloaded.values["opacity"], serde_json::json!(0.85));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn dnd_until_validation_mirrors_python() {
        assert!(validate_preference("dndUntil", &serde_json::json!(0)).is_ok());
        assert!(validate_preference("dndUntil", &serde_json::json!(-1)).is_ok());
        assert!(validate_preference("dndUntil", &serde_json::json!(4_102_444_800_000i64)).is_ok());
        // out of range or wrong type (mirrors test_upgrades.py)
        assert!(validate_preference("dndUntil", &serde_json::json!(-2)).is_err());
        assert!(validate_preference("dndUntil", &serde_json::json!(4_102_444_800_001i64)).is_err());
        assert!(validate_preference("dndUntil", &serde_json::json!("soon")).is_err());
    }

    #[test]
    fn muted_apps_dedupe_and_limits() {
        // legacy dedupes ["Music", "Music"] to ["Music"]
        let ok = validate_preference(
            "notificationMutedApps",
            &serde_json::json!(["Music", "Music"]),
        )
        .unwrap();
        assert_eq!(ok, serde_json::json!(["Music"]));
        assert!(
            validate_preference("notificationMutedApps", &serde_json::json!(["", "Music"]),)
                .is_err()
        );
        let many: Vec<String> = (0..101).map(|i| format!("app{i}")).collect();
        assert!(validate_preference("notificationMutedApps", &serde_json::json!(many),).is_err());
    }
}
