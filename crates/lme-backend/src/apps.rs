//! `.desktop` application scanning, byte-compatible with the Python
//! `Applications` class: locale handling, field codes, Hidden/NoDisplay/
//! OnlyShowIn rules, insertion-ordered output sorted by (casefold(name), id).

use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Python `locale_candidates()` port.
pub fn locale_candidates() -> Vec<String> {
    let raw = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let language = raw.split('.').next().unwrap_or("");
    if matches!(language, "C" | "POSIX" | "") {
        return Vec::new();
    }
    let (base, modifier) = match language.split_once('@') {
        Some((base, modifier)) => (base, Some(modifier)),
        None => (language, None),
    };
    let short = base.split('_').next().unwrap_or(base);
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        if !candidates.iter().any(|existing| existing == value) {
            candidates.push(value.to_string());
        }
    };
    push(language);
    push(base);
    if let Some(modifier) = modifier {
        push(&format!("{short}@{modifier}"));
    }
    push(short);
    candidates
}

/// Decode desktop-entry string escapes, leaving Exec quoting escapes intact.
pub fn desktop_unescape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.next() {
                Some('s') => result.push(' '),
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(character);
        }
    }
    result
}

/// Tokenize Exec without shell expansion and implement desktop field codes.
pub fn desktop_exec(
    value: &str,
    name: &str,
    icon: &str,
    path: &str,
) -> Result<Vec<String>, String> {
    let tokens = crate::pyutil::shlex_split(&desktop_unescape(value))?;
    let removed: HashSet<char> = "fFuUdDnNvm".chars().collect();
    let mut result: Vec<String> = Vec::new();
    for token in tokens {
        if token == "%i" {
            if !icon.is_empty() {
                result.push("--icon".to_string());
                result.push(icon.to_string());
            }
            continue;
        }
        if token.len() == 2
            && token.starts_with('%')
            && removed.contains(&token.chars().nth(1).unwrap())
        {
            continue;
        }
        let mut expanded = String::new();
        let mut characters = token.chars().peekable();
        while let Some(character) = characters.next() {
            if character != '%' {
                expanded.push(character);
                continue;
            }
            let Some(code) = characters.next() else {
                return Err("Incomplete desktop field code".to_string());
            };
            match code {
                '%' => expanded.push('%'),
                'c' => expanded.push_str(name),
                'k' => expanded.push_str(path),
                'F' | 'U' | 'i' => {
                    return Err("Multi-argument field code embedded in token".to_string())
                }
                other if removed.contains(&other) => {}
                other => return Err(format!("Unknown desktop field code: %{other}")),
            }
        }
        if !expanded.is_empty() {
            result.push(expanded);
        }
    }
    match result.first() {
        Some(first) if !first.is_empty() => Ok(result),
        _ => Err("Desktop entry has no executable".to_string()),
    }
}

/// Minimal INI parser matching Python `configparser(strict=False,
/// interpolation=None, optionxform=str)` for desktop files: case-sensitive
/// keys, `=` or `:` delimiters, `;`/`#` comments, indented continuations,
/// duplicate keys keep the last value, section names case-insensitive.
fn parse_desktop_entry(text: &str) -> Option<HashMap<String, String>> {
    let mut section: Option<String> = None;
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent > 0 {
            // Continuation of the previous key.
            if let (Some(name), Some(table)) = (&current, section.as_ref()) {
                if let Some(entry) = sections.get_mut(table) {
                    let existing = entry.get(name).cloned().unwrap_or_default();
                    entry.insert(name.clone(), format!("{existing}\n{trimmed}"));
                }
            }
            continue;
        }
        if trimmed.starts_with('[') {
            let end = trimmed.find(']')?;
            let name = trimmed[1..end].trim().to_lowercase();
            sections.entry(name.clone()).or_default();
            section = Some(name);
            current = None;
            continue;
        }
        let delimiter = trimmed.find(['=', ':'])? + 1;
        let key = trimmed[..delimiter - 1].trim_end().to_string();
        let value = trimmed[delimiter..].trim_start().to_string();
        if let Some(table) = &section {
            if let Some(entry) = sections.get_mut(table) {
                entry.insert(key.clone(), value);
                current = Some(key);
            }
        }
    }
    section?;
    sections.remove("desktop entry")
}

pub struct Applications {
    pub paths: Vec<PathBuf>,
    pub entries: HashMap<String, Map<String, Value>>,
}

impl Applications {
    /// `$XDG_DATA_HOME/applications` plus each `$XDG_DATA_DIRS/applications`.
    pub fn default_paths() -> Vec<PathBuf> {
        let user = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(|| PathBuf::from(".local/share"));
        let system = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
        let mut paths = vec![user.join("applications")];
        for directory in system.split(':') {
            if !directory.is_empty() {
                paths.push(Path::new(directory).join("applications"));
            }
        }
        paths
    }

    pub fn new(paths: Option<Vec<PathBuf>>) -> Self {
        Self {
            paths: paths.unwrap_or_else(Self::default_paths),
            entries: HashMap::new(),
        }
    }

    pub fn scan(&mut self) -> Vec<Value> {
        self.entries.clear();
        let mut seen: HashSet<String> = HashSet::new();
        let current_desktops: HashSet<String> = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_else(|_| "niri".to_string())
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        for base in &self.paths {
            if !base.is_dir() {
                continue;
            }
            let mut files = Vec::new();
            collect_desktop_files(base, &mut files);
            files.sort();
            for path in files {
                let Ok(relative) = path.strip_prefix(base) else {
                    continue;
                };
                let desktop_id = relative.to_string_lossy().replace(['/', '\\'], "-");
                if !seen.insert(desktop_id.clone()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some(entry) = parse_desktop_entry(&text) else {
                    continue;
                };
                let get = |key: &str| entry.get(key).cloned().unwrap_or_default();
                if get("Type") != "Application"
                    || get("Hidden").to_lowercase() == "true"
                    || get("NoDisplay").to_lowercase() == "true"
                {
                    continue;
                }
                let only_raw = get("OnlyShowIn");
                let only: HashSet<&str> = only_raw.split(';').filter(|s| !s.is_empty()).collect();
                let excluded_raw = get("NotShowIn");
                let excluded: HashSet<&str> =
                    excluded_raw.split(';').filter(|s| !s.is_empty()).collect();
                let current: HashSet<&str> = current_desktops.iter().map(|s| s.as_str()).collect();
                if (!only.is_empty() && only.is_disjoint(&current))
                    || !excluded.is_disjoint(&current)
                {
                    continue;
                }
                let try_exec = desktop_unescape(&get("TryExec"));
                if !try_exec.is_empty() && crate::pyutil::which(&try_exec).is_none() {
                    continue;
                }
                let localized = |field: &str| -> String {
                    let mut value = get(field);
                    for candidate in locale_candidates() {
                        let key = format!("{field}[{candidate}]");
                        if let Some(found) = entry.get(&key) {
                            if !found.is_empty() {
                                value = found.clone();
                                break;
                            }
                        }
                    }
                    desktop_unescape(&value)
                };
                let name = localized("Name");
                if name.is_empty() {
                    continue;
                }
                let icon = desktop_unescape(&get("Icon"));
                let absolute = match std::fs::canonicalize(&path) {
                    Ok(resolved) => resolved.to_string_lossy().to_string(),
                    Err(_) => continue,
                };
                let argv = match desktop_exec(&get("Exec"), &name, &icon, &absolute) {
                    Ok(argv) => argv,
                    Err(_) => continue,
                };
                let terminal = get("Terminal").to_lowercase() == "true";
                let mut record = Map::new();
                record.insert("id".into(), Value::String(desktop_id.clone()));
                record.insert("name".into(), Value::String(name));
                record.insert("icon".into(), Value::String(icon));
                record.insert("path".into(), Value::String(absolute));
                record.insert("terminal".into(), Value::Bool(terminal));
                record.insert(
                    "argv".into(),
                    Value::Array(argv.into_iter().map(Value::String).collect()),
                );
                record.insert(
                    "genericName".into(),
                    Value::String(localized("GenericName")),
                );
                let comment = localized("Comment");
                record.insert("comment".into(), Value::String(comment));
                let keywords: Vec<Value> = localized("Keywords")
                    .split(';')
                    .filter(|word| !word.is_empty())
                    .map(|word| Value::String(word.to_string()))
                    .collect();
                record.insert("keywords".into(), Value::Array(keywords));
                self.entries.insert(desktop_id, record);
            }
        }
        let mut apps: Vec<&Map<String, Value>> = self.entries.values().collect();
        let sort_key = |app: &&Map<String, Value>| {
            (
                crate::pyutil::casefold(app["name"].as_str().unwrap_or("")),
                app["id"].as_str().unwrap_or("").to_string(),
            )
        };
        apps.sort_by_key(sort_key);
        apps.iter()
            .map(|app| {
                // shift_remove keeps the Python dict order (swap_remove would
                // move the last key into argv's slot).
                let mut public = (*app).clone();
                public.shift_remove("argv");
                Value::Object(public)
            })
            .collect()
    }
}

/// Recursive `*.desktop` collection mirroring `Path.rglob`: no directory
/// symlinks are followed (Python 3.12 glob semantics) and hidden entries
/// are skipped (glob `*` does not match leading dots).
fn collect_desktop_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = read_dir.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_desktop_files(&path, out);
        } else if file_type.is_file() && name.ends_with(".desktop") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_and_field_codes() {
        assert_eq!(desktop_unescape(r"a\sb\nc"), "a b\nc");
        assert_eq!(desktop_unescape(r"keep \q"), r"keep \q");
        let argv = desktop_exec("code %F -t %u --name %c", "My App", "", "/x.desktop").unwrap();
        assert_eq!(argv, vec!["code", "-t", "--name", "My App"]);
        let argv = desktop_exec("app %i", "N", "icon-x", "p").unwrap();
        assert_eq!(argv, vec!["app", "--icon", "icon-x"]);
        assert!(desktop_exec("bad %z", "n", "i", "p").is_err());
        assert!(desktop_exec("nope %", "n", "i", "p").is_err());
        assert!(desktop_exec(" %f ", "n", "i", "p").is_err());
    }

    #[test]
    fn locale_candidates_chain() {
        // Simulated env: exercised fully in the parity suite through LANG.
        std::env::set_var("LC_ALL", "en_US.UTF-8@pinyin");
        std::env::remove_var("LC_MESSAGES");
        std::env::remove_var("LANG");
        // Python: raw.split(".")[0] drops everything from the encoding dot.
        assert_eq!(locale_candidates(), vec!["en_US", "en"]);
        std::env::remove_var("LC_ALL");
        std::env::set_var("LC_ALL", "C");
        assert!(locale_candidates().is_empty());
        std::env::remove_var("LC_ALL");
    }

    #[test]
    fn scan_filters_and_sorts() {
        let directory = std::env::temp_dir().join(format!("lme-apps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let applications = directory.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::write(
            applications.join("zeta.desktop"),
            "[Desktop Entry]\nType=Application\nName=Zeta\nExec=zeta %u\n",
        )
        .unwrap();
        std::fs::write(
            applications.join("alpha.desktop"),
            "[Desktop Entry]\nType=Application\nName[en]=Alpha EN\nName=Alpha\nExec=/bin/alpha %c\nIcon=alpha\n",
        )
        .unwrap();
        std::fs::write(
            applications.join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\nExec=x\n",
        )
        .unwrap();
        std::fs::write(
            applications.join("broken.desktop"),
            "[Desktop Entry]\nType=Application\nName=Broken\nExec=bad %%%\n",
        )
        .unwrap();
        let mut scanner = Applications::new(Some(vec![applications]));
        std::env::remove_var("LC_ALL");
        std::env::remove_var("LC_MESSAGES");
        std::env::set_var("LANG", "en_US.UTF-8");
        let apps = scanner.scan();
        std::env::remove_var("LANG");
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0]["name"], "Alpha EN");
        assert_eq!(apps[0]["icon"], "alpha");
        assert_eq!(apps[1]["name"], "Zeta");
        assert!(apps[0].get("argv").is_none(), "argv must stay private");
        // The cached entry keeps argv for launching.
        let entry = scanner.entries.get("alpha.desktop").unwrap();
        assert_eq!(entry["argv"], serde_json::json!(["/bin/alpha", "Alpha EN"]));
        let _ = std::fs::remove_dir_all(&directory);
    }
}
