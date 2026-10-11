//! Shared fixture environment for the parity suite: identical HOME, XDG
//! directories, stub tools and locale for the Python backend and the Rust
//! binary, so their JSON-lines output can be compared field by field.

// Each integration-test binary includes this module but uses a different
// subset of the helpers, so per-binary dead code is expected, not a defect.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub struct Fixture {
    pub root: PathBuf,
    pub log: PathBuf,
}

pub fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = lme-backend/
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lme-parity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".local/share/applications")).unwrap();
        std::fs::create_dir_all(home.join(".config/lme-shell")).unwrap();
        std::fs::create_dir_all(root.join("share/applications")).unwrap();

        // Preferences: valid values plus one unknown key (must be skipped).
        std::fs::write(
            home.join(".config/lme-shell/settings.json"),
            "{\n  \"opacity\": 0.85,\n  \"accent\": \"#BD93F9\",\n  \"terminal\": \"kitty\",\n  \"barFontSize\": 14,\n  \"clipboardLimit\": 200,\n  \"tasks\": [{\"id\": \"a1\", \"text\": \"ship parity\", \"done\": false}],\n  \"weatherAutoLocation\": false,\n  \"weatherLocation\": {\"name\": \"Hanoi\", \"country\": \"VN\", \"latitude\": 21.03, \"longitude\": 105.8},\n  \"weatherUnit\": \"celsius\",\n  \"unknownKey\": true\n}\n",
        )
        .unwrap();

        // Desktop entries: plain, localized, terminal, filtered, broken.
        let user_apps = home.join(".local/share/applications");
        std::fs::write(
            user_apps.join("zeta.desktop"),
            "[Desktop Entry]\nType=Application\nName=Zeta\nExec=zeta %u\nIcon=zeta\n",
        )
        .unwrap();
        std::fs::write(
            user_apps.join("alpha.desktop"),
            "[Desktop Entry]\nType=Application\nName=Alpha\nName[en_US]=Alpha US\nComment[en_US]=US comment\nGenericName[en_US]=Editor US\nComment=Generic comment\nKeywords=text;editor;\nExec=/bin/alpha %c\nIcon=alpha\n",
        )
        .unwrap();
        std::fs::write(
            user_apps.join("term.desktop"),
            "[Desktop Entry]\nType=Application\nName=Terminal App\nTerminal=true\nExec=terminal-app --run %f\nTryExec=sh\n",
        )
        .unwrap();
        std::fs::write(
            user_apps.join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\nExec=x\n",
        )
        .unwrap();
        std::fs::write(
            user_apps.join("kde-only.desktop"),
            "[Desktop Entry]\nType=Application\nName=KDE Only\nOnlyShowIn=KDE;\nExec=x\n",
        )
        .unwrap();
        std::fs::write(
            user_apps.join("broken.desktop"),
            "[Desktop Entry]\nType=Application\nName=Broken\nExec=bad %%%\n",
        )
        .unwrap();
        // A duplicate id in a *later* data dir must not override the user one.
        std::fs::write(
            root.join("share/applications/alpha.desktop"),
            "[Desktop Entry]\nType=Application\nName=Alpha Override\nExec=/bin/other\n",
        )
        .unwrap();
        std::fs::write(
            root.join("share/applications/system.desktop"),
            "[Desktop Entry]\nType=Application\nName=System Tool\nExec=/usr/bin/system-tool\nIcon=system-tool\n",
        )
        .unwrap();

        let log = root.join("calls.log");
        Self { root, log }
    }

    pub fn env(&self) -> std::collections::HashMap<String, String> {
        let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
        let stubs = repo_root().join("bench/stubs");
        env.insert(
            "PATH".to_string(),
            format!(
                "{}:{}",
                stubs.display(),
                env.get("PATH").cloned().unwrap_or_default()
            ),
        );
        env.insert(
            "HOME".to_string(),
            self.root.join("home").display().to_string(),
        );
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            self.root.join("home/.config").display().to_string(),
        );
        env.insert(
            "XDG_DATA_HOME".to_string(),
            self.root.join("home/.local/share").display().to_string(),
        );
        env.insert(
            "XDG_DATA_DIRS".to_string(),
            self.root.join("share").display().to_string(),
        );
        env.insert("XDG_CURRENT_DESKTOP".to_string(), "niri".to_string());
        env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        env.remove("LC_ALL");
        env.remove("LC_MESSAGES");
        env.remove("NIRI_SOCKET");
        env.insert("LME_STUB_LOG".to_string(), self.log.display().to_string());
        env
    }

    pub fn reset_log(&self) {
        let _ = std::fs::remove_file(&self.log);
    }

    pub fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .map(|text| text.lines().map(String::from).collect())
            .unwrap_or_default()
    }
}

/// Python is available for live parity; tests skip with a clear message
/// otherwise (the golden snapshots still pin the shape).
pub fn python3() -> Option<PathBuf> {
    which(&std::env::var("PATH").unwrap_or_default(), "python3")
}

pub fn which(path_env: &str, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Run a command with the fixture environment and capture stdout.
pub fn capture(fixture: &Fixture, program: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(fixture.env())
        .output()
        .expect("spawn backend");
    assert!(
        output.status.success(),
        "{} exited with {:?}: {}",
        program.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap_or_default()
}

/// Replace host-volatile values (memory, temperature, wall-clock stamps)
/// and the fixture's absolute paths so two snapshots taken seconds apart
/// from different fixture directories compare equal.
pub fn normalize(snapshot: &mut serde_json::Value, root: &Path) {
    if let Some(system) = snapshot
        .get_mut("system")
        .and_then(|value| value.as_object_mut())
    {
        for key in ["memoryPercent", "memoryUsedGiB", "memoryTotalGiB", "tempC"] {
            system.insert(key.to_string(), serde_json::Value::Null);
        }
    }
    if let Some(weather) = snapshot
        .get_mut("weather")
        .and_then(|value| value.as_object_mut())
    {
        weather.insert("updatedAt".to_string(), serde_json::Value::Null);
    }
    let prefix = format!("{}/", root.display());
    fn walk(value: &mut serde_json::Value, prefix: &str) {
        match value {
            serde_json::Value::String(text) => {
                if let Some(stripped) = text.strip_prefix(prefix) {
                    *text = format!("<FIXTURE>/{stripped}");
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, prefix);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values_mut() {
                    walk(item, prefix);
                }
            }
            _ => {}
        }
    }
    walk(snapshot, &prefix);
}
