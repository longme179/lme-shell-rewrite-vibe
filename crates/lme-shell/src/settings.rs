// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings service for the native daemon. Unlike the frozen `lme-backend`
//! `Preferences` (which validates the Phase 0 key set and drops unknown
//! keys), the daemon PRESERVES every saved key it does not know — mission
//! decision 4: settings.json keeps its format and keys, including the ones
//! upstream added after the freeze (see docs/UPSTREAM_SYNC.md).
//!
//! Writes follow the same discipline as the legacy writers: flock on
//! `settings.json.lock`, force-reload, merge, atomic 0600 temp + rename.

use lme_backend::prefs::{python_json_pretty, validate_preference};
use lme_ui::theme::Palette;
use serde_json::{Map, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub struct SettingsService {
    pub path: PathBuf,
    pub values: Map<String, Value>,
    pub palette_path: PathBuf,
    pub palette: Palette,
}

impl SettingsService {
    pub fn load() -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));
        let dir = config.join("lme-shell");
        Self::load_from(dir.join("settings.json"), dir.join("palette.json"))
    }

    pub fn load_from(path: PathBuf, palette_path: PathBuf) -> Self {
        let mut service = Self {
            path,
            values: lme_backend::prefs::default_preferences(),
            palette_path,
            palette: Palette::default(),
        };
        service.reload_settings();
        service.reload_palette();
        service
    }

    /// Reload settings.json. Returns true when values changed.
    pub fn reload_settings(&mut self) -> bool {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return false;
        };
        let Ok(Value::Object(saved)) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        let mut merged = lme_backend::prefs::default_preferences();
        let mut changed = false;
        for (key, value) in saved {
            // Known Phase 0 keys: validated (invalid values ignored, not
            // fatal). Unknown keys (upstream wave and anything else): kept
            // verbatim so other LME clients never lose their state.
            match validate_preference(&key, &value) {
                Ok(validated) => {
                    if merged.get(&key) != Some(&validated) {
                        changed = true;
                    }
                    merged.insert(key, validated);
                }
                Err(_) => {
                    if merged.get(&key) != Some(&value) {
                        changed = true;
                    }
                    merged.insert(key, value);
                }
            }
        }
        if changed || self.values != merged {
            changed = true;
        }
        self.values = merged;
        changed
    }

    /// Reload palette.json. Returns true when the palette changed.
    pub fn reload_palette(&mut self) -> bool {
        let new = Palette::load(&self.palette_path);
        let changed = format!("{new:?}") != format!("{:?}", self.palette);
        self.palette = new;
        changed
    }

    /// Validated write of one key (lock → reload → merge → atomic replace).
    /// (Used by the settings app path in later tiers; kept for the contract.)
    #[allow(dead_code)]
    pub fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
        let _validated =
            validate_preference(key, &value).map_err(|_| format!("Invalid preference: {key}"))?;
        self.update(&[(key.to_string(), value)].into_iter().collect())
    }

    #[allow(dead_code)]
    pub fn update(&mut self, updates: &Map<String, Value>) -> Result<(), String> {
        for (key, value) in updates {
            validate_preference(key, value).map_err(|_| format!("Invalid preference: {key}"))?;
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let lock_path = self.path.with_extension("lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| e.to_string())?;
        let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
        let result = (|| -> Result<(), String> {
            rustix::fs::flock(&lock_file, rustix::fs::FlockOperation::LockExclusive)
                .map_err(|e| e.to_string())?;
            self.reload_settings();
            let mut merged = self.values.clone();
            for (key, value) in updates {
                merged.insert(key.clone(), value.clone());
            }
            write_settings_atomic(&self.path, &merged)
        })();
        drop(lock_file);
        result?;
        self.reload_settings();
        Ok(())
    }
}

fn write_settings_atomic(
    path: &std::path::Path,
    values: &Map<String, Value>,
) -> Result<(), String> {
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let unique = format!(
        ".settings-{}.{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let temporary = parent.join(unique);
    let result = (|| -> Result<(), String> {
        let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let body = python_json_pretty(&Value::Object(values.clone()));
        file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_unknown_upstream_keys() {
        let dir = std::env::temp_dir().join(format!("lme-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(
            &settings,
            r#"{"barHeight": 48, "favoriteApps": ["a", "b"], "nightLightEnabled": true, "opacity": 0.8}"#,
        )
        .unwrap();
        let mut svc = SettingsService::load_from(settings.clone(), dir.join("palette.json"));
        // upstream keys survive the load…
        assert_eq!(svc.values.get("barHeight"), Some(&serde_json::json!(48)));
        assert_eq!(
            svc.values.get("favoriteApps"),
            Some(&serde_json::json!(["a", "b"]))
        );
        // …and a validated write keeps them on disk.
        svc.set("opacity", serde_json::json!(0.9)).unwrap();
        let text = std::fs::read_to_string(&settings).unwrap();
        assert!(text.contains("\"barHeight\""));
        assert!(text.contains("\"favoriteApps\""));
        assert!(text.contains("\"nightLightEnabled\""));
        assert!(text.contains("\"opacity\": 0.9"));
        // invalid value for a known key is rejected
        assert!(svc.set("opacity", serde_json::json!(0.1)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
