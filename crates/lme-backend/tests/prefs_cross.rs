//! M4: settings.json cross-writer compatibility. The Python settings app
//! and the Rust daemon must both hold the same flock, merge without losing
//! each other's keys, and produce byte-identical file layouts.

mod common;

use common::{python3, python_tree};
use lme_backend::prefs::{default_preferences, Preferences};
use serde_json::{json, Value};
use std::path::PathBuf;

fn script(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lme-prefs-{name}-{}", std::process::id()))
}

/// Run a Python snippet that drives desktop_backend.Preferences against the
/// given settings path.
fn python_preferences(path: &std::path::Path, code: &str) -> String {
    let python = python3().expect("python3 available");
    let driver = format!(
        r#"
import json, sys
from pathlib import Path
sys.path.insert(0, {scripts:?})
from desktop_backend import Preferences
preferences = Preferences(Path({path:?}))
{code}
print(json.dumps(preferences.values, ensure_ascii=False))
"#,
        scripts = python_tree().join("scripts").display().to_string(),
        path = path.display().to_string(),
    );
    let output = std::process::Command::new(python)
        .arg("-c")
        .arg(driver)
        .env("PYTHONIOENCODING", "utf-8")
        .output()
        .expect("run python driver");
    assert!(
        output.status.success(),
        "python driver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn rust_reads_python_writes() {
    let root = script("read");
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("settings.json");
    // Python writes two keys under the lock.
    python_preferences(
        &path,
        r#"preferences.update({"opacity": 0.75, "tasks": [{"id": "t1", "text": "from python", "done": True}]})"#,
    );
    // Rust must observe them merged over the defaults.
    let preferences = Preferences::new(path.clone());
    assert_eq!(preferences.values["opacity"], json!(0.75));
    assert_eq!(
        preferences.values["tasks"],
        json!([{"id": "t1", "text": "from python", "done": true}])
    );
    assert_eq!(preferences.values["language"], json!("en"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn python_reads_rust_writes() {
    let root = script("write");
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("settings.json");
    let mut preferences = Preferences::new(path.clone());
    let mut updates = serde_json::Map::new();
    updates.insert("barFontSize".into(), json!(15));
    updates.insert(
        "tasks".into(),
        json!([{"id": "r1", "text": "from rust", "done": false}]),
    );
    preferences.update(updates).unwrap();

    let values: Value =
        serde_json::from_str(&python_preferences(&path, "")).expect("python can read our file");
    assert_eq!(values["barFontSize"], json!(15));
    assert_eq!(
        values["tasks"],
        json!([{"id": "r1", "text": "from rust", "done": false}])
    );
    // The file layout is Python-compatible: indent 2, trailing newline,
    // insertion-ordered default keys.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("{\n  \"language\": \"en\","));
    assert!(text.ends_with("}\n"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn concurrent_writers_do_not_lose_updates() {
    let root = script("stress");
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("settings.json");
    let python_path = path.clone();
    // Python writer loop: 15 alternating task-list updates.
    let python = std::thread::spawn(move || {
        for index in 0..15 {
            let code = format!(
                "preferences.update({{ \"tasks\": [{{\"id\": \"p{index}\", \"text\": \"py {index}\", \"done\": False}}] }})"
            );
            python_preferences(&python_path, &code);
        }
    });
    // Rust writer loop: 15 opacity updates interleaved under the same lock.
    let rust_path = path.clone();
    let rust = std::thread::spawn(move || {
        let mut preferences = Preferences::new(rust_path.clone());
        for index in 0..15 {
            preferences
                .set("opacity", json!(0.60 + index as f64 / 100.0))
                .unwrap();
        }
    });
    python.join().unwrap();
    rust.join().unwrap();
    // Both writers' final keys must survive: the last writer's task list
    // plus every rust opacity write being valid merges (>= 0.60, <= 1).
    let values: Value = serde_json::from_str(&python_preferences(&path, "")).unwrap();
    assert!(
        values["opacity"].as_f64().unwrap() >= 0.60,
        "opacity lost: {}",
        values["opacity"]
    );
    assert!(
        values["tasks"]
            .as_array()
            .is_some_and(|tasks| !tasks.is_empty()),
        "task list lost"
    );
    // And the file still parses with the full default key set.
    let keys: Vec<&str> = values
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let defaults = default_preferences();
    for key in defaults.keys() {
        assert!(keys.contains(&key.as_str()), "missing default key {key}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mode_bits_are_restricted() {
    let root = script("modes");
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("settings.json");
    let mut preferences = Preferences::new(path.clone());
    preferences.set("accent", json!("#ffffff")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let settings_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    let lock_mode = std::fs::metadata(path.with_extension("lock"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(settings_mode, 0o600);
    assert_eq!(lock_mode, 0o600);
    assert_eq!(dir_mode, 0o700);
    let _ = std::fs::remove_dir_all(&root);
}
