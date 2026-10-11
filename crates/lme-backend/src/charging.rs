//! Kernel charge-control discovery and state (`charge_control.py` port),
//! including the trusted-helper path check used before pkexec.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const HELPER_PATH: &str = "/usr/local/libexec/lme-shell-charge";

/// `trusted_helper()`: the helper and every parent must be root-owned and
/// not group/world-writable, and the helper itself must be a regular file
/// (not a symlink).
pub fn trusted_helper() -> bool {
    let path = Path::new(HELPER_PATH);
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    let mut current = Some(path);
    while let Some(item) = current {
        let Ok(info) = std::fs::metadata(item) else {
            return false;
        };
        if info.uid() != 0 || info.permissions().mode() & 0o022 != 0 {
            return false;
        }
        current = item.parent();
    }
    true
}

#[derive(Clone)]
pub struct ChargeControl {
    pub sysfs: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChargeKind {
    Threshold,
    ChargeTypes,
    Ideapad,
}

pub struct Discovered {
    pub kind: ChargeKind,
    pub path: PathBuf,
}

impl ChargeControl {
    pub fn new(sysfs: PathBuf) -> Self {
        Self { sysfs }
    }

    fn read(&self, path: &Path) -> String {
        std::fs::read_to_string(path)
            .map(|text| text.trim().to_string())
            .unwrap_or_default()
    }

    fn batteries(&self) -> Vec<PathBuf> {
        let mut supplies: Vec<PathBuf> = std::fs::read_dir(self.sysfs.join("class/power_supply"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        supplies.sort();
        supplies
            .into_iter()
            .filter(|path| self.read(&path.join("type")) == "Battery")
            .collect()
    }

    pub fn discover(&self) -> Option<Discovered> {
        for battery in self.batteries() {
            let end = battery.join("charge_control_end_threshold");
            let value = self.read(&end);
            if !value.is_empty()
                && value.bytes().all(|b| b.is_ascii_digit())
                && matches!(value.parse::<i64>(), Ok(limit) if (1..=100).contains(&limit))
            {
                return Some(Discovered {
                    kind: ChargeKind::Threshold,
                    path: end,
                });
            }
        }
        for battery in self.batteries() {
            let types = battery.join("charge_types");
            let text = self.read(&types);
            if text.contains("Standard")
                && text.contains("Long Life")
                && (text.contains("[Standard]") || text.contains("[Long Life]"))
            {
                return Some(Discovered {
                    kind: ChargeKind::ChargeTypes,
                    path: types,
                });
            }
        }
        let mut devices: Vec<PathBuf> = std::fs::read_dir(self.sysfs.join("bus/platform/devices"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().starts_with("VPC2004:"))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        devices.sort();
        for device in devices {
            let path = device.join("conservation_mode");
            let value = self.read(&path);
            if value == "0" || value == "1" {
                return Some(Discovered {
                    kind: ChargeKind::Ideapad,
                    path,
                });
            }
        }
        None
    }

    pub fn state(&self) -> Value {
        let discovered = self.discover();
        let kind = discovered.as_ref().map(|found| &found.kind);
        let threshold = kind == Some(&ChargeKind::Threshold);
        let mut base = json!({
            "available": discovered.is_some(),
            "kind": match kind {
                Some(ChargeKind::Threshold) => "threshold",
                Some(ChargeKind::ChargeTypes) => "charge-types",
                Some(ChargeKind::Ideapad) => "ideapad",
                None => "",
            },
            "mode": "",
            "limitPercent": null,
            "supports85": threshold,
            "writable": false,
            "reason": "No charge-control interface is exposed by this kernel/firmware.",
        });
        let Some(found) = discovered else {
            return base;
        };
        let writable = rustix::fs::access(&found.path, rustix::fs::Access::WRITE_OK).is_ok();
        let object = base.as_object_mut().unwrap();
        object.insert("writable".into(), Value::Bool(writable));
        object.insert("reason".into(), Value::String(String::new()));
        let value = self.read(&found.path);
        match found.kind {
            ChargeKind::Threshold => {
                let Ok(limit) = value.parse::<i64>() else {
                    return base;
                };
                object.insert("limitPercent".into(), json!(limit));
                object.insert(
                    "mode".into(),
                    json!(if limit == 100 { "max" } else { "conservation" }),
                );
            }
            ChargeKind::ChargeTypes => {
                object.insert(
                    "mode".into(),
                    json!(if value.contains("[Long Life]") {
                        "conservation"
                    } else {
                        "max"
                    }),
                );
            }
            ChargeKind::Ideapad => {
                object.insert(
                    "mode".into(),
                    json!(if value == "1" { "conservation" } else { "max" }),
                );
            }
        }
        if !threshold {
            object.insert(
                "reason".into(),
                Value::String(
                    "Conservation uses a fixed firmware limit; this interface cannot set an exact 85%."
                        .to_string(),
                ),
            );
        }
        base
    }

    /// Port of `set_mode`, including the Long-Life disarm and rollback.
    pub fn set_mode(&self, limit: i64) -> Result<Value, String> {
        if !matches!(limit, 85 | 100) {
            return Err("Choose conservation (85) or maximum charge (100).".to_string());
        }
        let Some(found) = self.discover() else {
            return Err(self.state()["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string());
        };
        let mut changes: Vec<(PathBuf, String)> = Vec::new();
        let mut write = |target: &Path, value: &str| -> Result<(), String> {
            changes.push((target.to_path_buf(), read_path(target)));
            std::fs::write(target, format!("{value}\n")).map_err(|error| error.to_string())
        };
        let result = (|| -> Result<(), String> {
            match found.kind {
                ChargeKind::Threshold => {
                    // Explicit percentages must not be overridden by firmware's
                    // separate Long Life mode. Preserve the start threshold
                    // unless it would be invalid with the requested stop.
                    let types = found.path.parent().unwrap().join("charge_types");
                    let text = read_path(&types);
                    if text.contains("[Long Life]") {
                        write(&types, "Standard")?;
                        if !read_path(&types).contains("[Standard]") {
                            return Err(
                                "The driver did not disable its fixed Long Life mode.".to_string()
                            );
                        }
                    }
                    let start = found
                        .path
                        .parent()
                        .unwrap()
                        .join("charge_control_start_threshold");
                    let value = read_path(&start);
                    if !value.is_empty()
                        && value.bytes().all(|b| b.is_ascii_digit())
                        && matches!(value.parse::<i64>(), Ok(existing) if existing >= limit)
                    {
                        write(&start, &(limit - 5).max(0).to_string())?;
                    }
                    write(&found.path, &limit.to_string())?;
                    if read_path(&found.path) != limit.to_string() {
                        return Err("The driver did not confirm the requested charge threshold."
                            .to_string());
                    }
                }
                ChargeKind::ChargeTypes => {
                    let desired = if limit == 85 { "Long Life" } else { "Standard" };
                    write(&found.path, desired)?;
                    if !read_path(&found.path).contains(&format!("[{desired}]")) {
                        return Err(
                            "The driver did not confirm the requested charge mode.".to_string()
                        );
                    }
                }
                ChargeKind::Ideapad => {
                    let desired = if limit == 85 { "1" } else { "0" };
                    write(&found.path, desired)?;
                    if read_path(&found.path) != desired {
                        return Err("The firmware did not confirm conservation mode.".to_string());
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (target, old) in changes.iter().rev() {
                // Enum sysfs files return the choice list; restore the
                // selected enum, not the entire list.
                let selected = old
                    .contains("[Standard]")
                    .then_some("Standard")
                    .or_else(|| old.contains("[Long Life]").then_some("Long Life"))
                    .unwrap_or(old);
                let _ = std::fs::write(target, format!("{selected}\n"));
            }
            return Err(error);
        }
        Ok(self.state())
    }
}

fn read_path(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("lme-charge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn threshold_discovery_and_state() {
        let root = fixture("threshold");
        let battery = root.join("class/power_supply/BAT0");
        std::fs::create_dir_all(&battery).unwrap();
        std::fs::write(battery.join("type"), "Battery\n").unwrap();
        std::fs::write(battery.join("charge_control_end_threshold"), "85\n").unwrap();
        std::fs::write(battery.join("charge_control_start_threshold"), "0\n").unwrap();
        let control = ChargeControl::new(root.clone());
        let state = control.state();
        assert_eq!(state["available"], json!(true));
        assert_eq!(state["kind"], json!("threshold"));
        assert_eq!(state["limitPercent"], json!(85));
        assert_eq!(state["mode"], json!("conservation"));
        assert_eq!(state["supports85"], json!(true));
        // Setting 100 writes the threshold and confirms it.
        control.set_mode(100).unwrap();
        assert_eq!(
            std::fs::read_to_string(battery.join("charge_control_end_threshold"))
                .unwrap()
                .trim(),
            "100"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn charge_types_and_ideapad() {
        let root = fixture("types");
        let battery = root.join("class/power_supply/BAT0");
        std::fs::create_dir_all(&battery).unwrap();
        std::fs::write(battery.join("type"), "Battery\n").unwrap();
        std::fs::write(battery.join("charge_types"), "Standard [Long Life] \n").unwrap();
        let control = ChargeControl::new(root.clone());
        let state = control.state();
        assert_eq!(state["kind"], json!("charge-types"));
        assert_eq!(state["mode"], json!("conservation"));
        assert_eq!(state["supports85"], json!(false));
        assert!(state["reason"]
            .as_str()
            .unwrap()
            .contains("fixed firmware limit"));
        let _ = std::fs::remove_dir_all(&root);

        let root = fixture("ideapad");
        let device = root.join("bus/platform/devices/VPC2004:00");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("conservation_mode"), "1\n").unwrap();
        let control = ChargeControl::new(root.clone());
        let state = control.state();
        assert_eq!(state["kind"], json!("ideapad"));
        assert_eq!(state["mode"], json!("conservation"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unavailable_when_no_interface() {
        let root = fixture("none");
        std::fs::create_dir_all(root.join("class/power_supply")).unwrap();
        let control = ChargeControl::new(root.clone());
        let state = control.state();
        assert_eq!(state["available"], json!(false));
        assert_eq!(state["mode"], json!(""));
        let _ = std::fs::remove_dir_all(&root);
    }
}
