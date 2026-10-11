//! Clipboard history service and one-shot capture mode (`clipboard.py`
//! port). The sqlite database and image directory are shared, on-disk state:
//! both the Python service and this implementation must remain read/write
//! compatible in either direction.

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead as _, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_TEXT: usize = 1024 * 1024;
pub const MAX_IMAGE: usize = 8 * 1024 * 1024;
pub const MAX_TOTAL: i64 = 64 * 1024 * 1024;
pub const MAX_PINS: i64 = 20;

pub fn history_directory() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    state.join("lme-shell/clipboard")
}

fn wall_clock() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

/// Sniff PNG/JPEG/GIF/BMP/WEBP like `image_type()`.
/// SIGTERM a child, then SIGKILL after a bounded wait (Popen.terminate).
pub fn terminate_process(child: &mut std::process::Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    match wait_with_timeout(child, Duration::from_secs(2)) {
        Some(_) => {}
        None => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Poll `try_wait` until the deadline (std has no `Child::wait_timeout`).
pub fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn image_type(data: &[u8]) -> Result<(String, String), String> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(("image/png".to_string(), "png".to_string()))
    } else if data.starts_with(b"\xff\xd8\xff") {
        Ok(("image/jpeg".to_string(), "jpg".to_string()))
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Ok(("image/gif".to_string(), "gif".to_string()))
    } else if data.starts_with(b"BM") {
        Ok(("image/bmp".to_string(), "bmp".to_string()))
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        Ok(("image/webp".to_string(), "webp".to_string()))
    } else {
        Err("Unsupported clipboard image format".to_string())
    }
}

struct ClipboardPreferences {
    clipboard_enabled: bool,
    clipboard_images: bool,
    clipboard_limit: i64,
}

impl Default for ClipboardPreferences {
    fn default() -> Self {
        Self {
            clipboard_enabled: true,
            clipboard_images: true,
            clipboard_limit: 100,
        }
    }
}

/// Read the three clipboard keys from settings.json with the same lenient
/// type checks as the Python `preferences()`.
fn clipboard_preferences() -> ClipboardPreferences {
    let mut config = ClipboardPreferences::default();
    let path = crate::prefs::Preferences::default_path();
    let Ok(text) = std::fs::read_to_string(path) else {
        return config;
    };
    let Ok(Value::Object(saved)) = serde_json::from_str(&text) else {
        return config;
    };
    if let Some(Value::Bool(enabled)) = saved.get("clipboardEnabled") {
        config.clipboard_enabled = *enabled;
    }
    if let Some(Value::Bool(images)) = saved.get("clipboardImages") {
        config.clipboard_images = *images;
    }
    if let Some(Value::Number(limit)) = saved.get("clipboardLimit") {
        if let Some(limit) = limit.as_i64() {
            if (20..=500).contains(&limit) {
                config.clipboard_limit = limit;
            }
        }
    }
    config
}

pub struct History {
    pub directory: PathBuf,
    images: PathBuf,
    limit: i64,
    max_total: i64,
    db: Connection,
}

impl History {
    pub fn new(directory: &Path, limit: i64, max_total: i64) -> Result<Self, String> {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
        let images = directory.join("images");
        std::fs::create_dir_all(&images).map_err(|error| error.to_string())?;
        let _ = std::fs::set_permissions(&images, std::fs::Permissions::from_mode(0o700));
        let db = Connection::open(directory.join("history.sqlite3"))
            .map_err(|error| error.to_string())?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, mime TEXT NOT NULL,
                data BLOB NOT NULL, preview TEXT NOT NULL, search TEXT NOT NULL,
                suffix TEXT NOT NULL, created REAL NOT NULL, pinned INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
            INSERT OR IGNORE INTO meta VALUES ('revision', 0), ('paused', 0);",
        )
        .map_err(|error| error.to_string())?;
        let _ = std::fs::set_permissions(
            directory.join("history.sqlite3"),
            std::fs::Permissions::from_mode(0o600),
        );
        Ok(Self {
            directory: directory.to_path_buf(),
            images,
            limit,
            max_total,
            db,
        })
    }

    pub fn revision(&self) -> i64 {
        self.db
            .query_row("SELECT value FROM meta WHERE key='revision'", [], |row| {
                row.get(0)
            })
            .unwrap_or(0)
    }

    pub fn paused(&self) -> bool {
        self.db
            .query_row("SELECT value FROM meta WHERE key='paused'", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|value| value != 0)
            .unwrap_or(false)
    }

    pub fn pause(&mut self, value: bool) -> Result<(), String> {
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Clipboard operation failed".to_string())?;
        transaction
            .execute(
                "UPDATE meta SET value=? WHERE key='paused'",
                params![i64::from(value)],
            )
            .map_err(|_| "Clipboard operation failed".to_string())?;
        transaction
            .execute("UPDATE meta SET value=value+1 WHERE key='revision'", [])
            .map_err(|_| "Clipboard operation failed".to_string())?;
        transaction
            .commit()
            .map_err(|_| "Clipboard operation failed".to_string())?;
        Ok(())
    }

    /// Standalone trim (own immediate transaction), used by tests.
    pub fn trim(&mut self) -> Result<(), String> {
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Clipboard operation failed".to_string())?;
        trim_entries(&transaction, &self.images, self.limit, self.max_total)?;
        transaction
            .commit()
            .map_err(|_| "Clipboard operation failed".to_string())
    }

    pub fn configure(&mut self, limit: i64) -> Result<(), String> {
        if !(20..=500).contains(&limit) {
            return Err("History limit must be between 20 and 500".to_string());
        }
        if limit == self.limit {
            return Ok(());
        }
        self.limit = limit;
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Clipboard operation failed".to_string())?;
        trim_entries(&transaction, &self.images, self.limit, self.max_total)?;
        bump_revision(&transaction)?;
        transaction
            .commit()
            .map_err(|_| "Clipboard operation failed".to_string())?;
        Ok(())
    }

    /// `store` port. Returns the entry id when stored.
    pub fn store(&mut self, kind: &str, data: &[u8]) -> Result<Option<String>, String> {
        if data.is_empty() || self.paused() {
            return Ok(None);
        }
        if !matches!(kind, "text" | "image") {
            return Err("Unknown clipboard type".to_string());
        }
        let limit = if kind == "text" { MAX_TEXT } else { MAX_IMAGE };
        if data.len() > limit {
            return Err("Clipboard item exceeds the size limit".to_string());
        }
        let (mime, suffix, preview, search) = if kind == "text" {
            // errors="replace": invalid UTF-8 becomes U+FFFD runs.
            let text = String::from_utf8_lossy(data).into_owned();
            (
                "text/plain;charset=utf-8".to_string(),
                String::new(),
                text.chars().take(240).collect::<String>(),
                crate::pyutil::casefold(&text),
            )
        } else {
            let (mime, suffix) = image_type(data)?;
            let size = crate::pyutil::py_round_int(data.len() as f64 / 1024.0);
            (
                mime.clone(),
                suffix.clone(),
                format!("{} image · {} KB", suffix.to_uppercase(), size),
                format!("image {mime}"),
            )
        };
        let mut hasher = Sha256::new();
        hasher.update(mime.as_bytes());
        hasher.update(b"\0");
        hasher.update(data);
        let ident = format!("{:x}", hasher.finalize());
        {
            let transaction = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| "Clipboard operation failed".to_string())?;
            // Re-check inside the write lock, after an in-flight pause action.
            let paused: i64 = transaction
                .query_row("SELECT value FROM meta WHERE key='paused'", [], |row| {
                    row.get(0)
                })
                .unwrap_or(0);
            if paused != 0 {
                return Ok(None);
            }
            transaction
                .execute(
                    "INSERT INTO entries VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)
                     ON CONFLICT(id) DO UPDATE SET created=excluded.created",
                    params![
                        ident,
                        kind,
                        mime,
                        data,
                        preview,
                        search,
                        suffix,
                        wall_clock()
                    ],
                )
                .map_err(|_| "Clipboard operation failed".to_string())?;
            if !suffix.is_empty() {
                let path = self.images.join(format!("{ident}.{suffix}"));
                if !path.exists() {
                    let temporary = self.images.join(format!(".{}.tmp", std::process::id()));
                    std::fs::write(&temporary, data)
                        .map_err(|_| "Clipboard operation failed".to_string())?;
                    let _ = std::fs::set_permissions(
                        &temporary,
                        std::fs::Permissions::from_mode(0o600),
                    );
                    std::fs::rename(&temporary, &path)
                        .map_err(|_| "Clipboard operation failed".to_string())?;
                }
            }
            trim_entries(&transaction, &self.images, self.limit, self.max_total)?;
            bump_revision(&transaction)?;
            transaction
                .commit()
                .map_err(|_| "Clipboard operation failed".to_string())?;
        }
        Ok(Some(ident))
    }

    pub fn entries(&self, query: &str, kind: &str) -> Result<Vec<Value>, String> {
        if !matches!(kind, "all" | "text" | "image") {
            return Err("Unknown history filter".to_string());
        }
        let folded: String = crate::pyutil::casefold(query).chars().take(300).collect();
        let mut statement = self
            .db
            .prepare(
                "SELECT id, kind, mime, preview, suffix, created, pinned, length(data) AS bytes
                 FROM entries WHERE instr(search, ?1) > 0 AND (?2='all' OR kind=?2)
                 ORDER BY pinned DESC, created DESC, id",
            )
            .map_err(|_| "Clipboard operation failed".to_string())?;
        let rows = statement
            .query_map(params![folded, kind], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|_| "Clipboard operation failed".to_string())?;
        let mut result = Vec::new();
        for row in rows.flatten() {
            let image = if row.4.is_empty() {
                String::new()
            } else {
                Path::new(&self.images.join(format!("{}.{}", row.0, row.4)))
                    .to_string_lossy()
                    .to_string()
            };
            result.push(json!({
                "id": row.0, "kind": row.1, "mime": row.2, "preview": row.3,
                "created": row.5, "pinned": row.6 != 0, "bytes": row.7,
                "image": format!("file://{image}"),
            }));
        }
        Ok(result)
    }

    pub fn item(&self, ident: &Value) -> Result<(String, String, Vec<u8>), String> {
        const GENERIC: &str =
            "Clipboard operation failed. Check the Wayland session and local storage.";
        let Some(id) = ident.as_str() else {
            return Err("Invalid clipboard item".to_string());
        };
        if id.len() != 64 {
            return Err("Invalid clipboard item".to_string());
        }
        let row: Option<(String, Vec<u8>)> = self
            .db
            .query_row(
                "SELECT mime, data FROM entries WHERE id=?",
                params![id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(()),
            })
            .map_err(|_| GENERIC.to_string())?;
        match row {
            Some((mime, data)) => Ok((id.to_string(), mime, data)),
            None => Err("This clipboard item is no longer available".to_string()),
        }
    }

    pub fn pin(&mut self, ident: &Value) -> Result<(), String> {
        let (id, _, _) = self.item(ident)?;
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Clipboard operation failed".to_string())?;
        let pinned: i64 = transaction
            .query_row(
                "SELECT pinned FROM entries WHERE id=?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|_| "This clipboard item is no longer available".to_string())?;
        if pinned == 0 {
            let count: i64 = transaction
                .query_row("SELECT count(*) FROM entries WHERE pinned=1", [], |row| {
                    row.get(0)
                })
                .unwrap_or(0);
            if count >= MAX_PINS {
                return Err("Up to 20 clipboard items can be pinned".to_string());
            }
        }
        transaction
            .execute(
                "UPDATE entries SET pinned=? WHERE id=?",
                params![1 - pinned, id],
            )
            .map_err(|_| "Clipboard operation failed".to_string())?;
        trim_entries(&transaction, &self.images, self.limit, self.max_total)?;
        bump_revision(&transaction)?;
        transaction
            .commit()
            .map_err(|_| "Clipboard operation failed".to_string())?;
        Ok(())
    }

    pub fn delete(&mut self, ident: &Value) -> Result<(), String> {
        let (id, _, _) = self.item(ident)?;
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Clipboard operation failed".to_string())?;
        let suffix: Option<String> = transaction
            .query_row(
                "SELECT suffix FROM entries WHERE id=?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|_| "This clipboard item is no longer available".to_string())?;
        transaction
            .execute("DELETE FROM entries WHERE id=?", params![id])
            .map_err(|_| "Clipboard operation failed".to_string())?;
        if let Some(suffix) = suffix {
            if !suffix.is_empty() {
                let _ = std::fs::remove_file(self.images.join(format!("{id}.{suffix}")));
            }
        }
        transaction
            .execute("UPDATE meta SET value=value+1 WHERE key='revision'", [])
            .map_err(|_| "Clipboard operation failed".to_string())?;
        transaction
            .commit()
            .map_err(|_| "Clipboard operation failed".to_string())?;
        Ok(())
    }

    pub fn clear(&mut self, include_pinned: bool) -> Result<(), String> {
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Clipboard operation failed".to_string())?;
        let filter = if include_pinned {
            ""
        } else {
            " WHERE pinned=0"
        };
        let suffixes: Vec<(String, String)> = transaction
            .prepare(&format!("SELECT id, suffix FROM entries{filter}"))
            .map_err(|_| "Clipboard operation failed".to_string())?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|_| "Clipboard operation failed".to_string())?
            .flatten()
            .collect();
        transaction
            .execute(&format!("DELETE FROM entries{filter}"), [])
            .map_err(|_| "Clipboard operation failed".to_string())?;
        for (id, suffix) in &suffixes {
            if !suffix.is_empty() {
                let _ = std::fs::remove_file(self.images.join(format!("{id}.{suffix}")));
            }
        }
        transaction
            .execute("UPDATE meta SET value=value+1 WHERE key='revision'", [])
            .map_err(|_| "Clipboard operation failed".to_string())?;
        transaction
            .commit()
            .map_err(|_| "Clipboard operation failed".to_string())?;
        Ok(())
    }

    pub fn total(&self) -> i64 {
        self.db
            .query_row("SELECT count(*) FROM entries", [], |row| row.get(0))
            .unwrap_or(0)
    }
}

/// Trim to the configured limit/total, keeping pinned entries (Python `trim`).
fn trim_entries(
    transaction: &Transaction<'_>,
    images: &Path,
    limit: i64,
    max_total: i64,
) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "SELECT id, suffix, pinned, length(data) AS bytes FROM entries
             ORDER BY created DESC, id",
        )
        .map_err(|_| "Clipboard operation failed".to_string())?;
    let rows: Vec<(String, String, i64, i64)> = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(|_| "Clipboard operation failed".to_string())?
        .flatten()
        .collect();
    drop(statement);
    let mut count = rows.len() as i64;
    let mut size: i64 = rows.iter().map(|row| row.3).sum();
    for row in rows.iter().rev() {
        if count <= limit && size <= max_total {
            break;
        }
        if row.2 != 0 {
            continue; // pinned
        }
        transaction
            .execute("DELETE FROM entries WHERE id=?", params![row.0])
            .map_err(|_| "Clipboard operation failed".to_string())?;
        if !row.1.is_empty() {
            let _ = std::fs::remove_file(images.join(format!("{}.{}", row.0, row.1)));
        }
        count -= 1;
        size -= row.3;
    }
    Ok(())
}

fn bump_revision(transaction: &Transaction<'_>) -> Result<(), String> {
    transaction
        .execute("UPDATE meta SET value=value+1 WHERE key='revision'", [])
        .map_err(|_| "Clipboard operation failed".to_string())?;
    Ok(())
}

struct CaptureService {
    watchers: Vec<(&'static str, std::process::Child)>,
    lock: Option<std::fs::File>,
    error: String,
    last_retry: Option<Instant>,
}

impl CaptureService {
    fn new() -> Self {
        Self {
            watchers: Vec::new(),
            lock: None,
            error: String::new(),
            last_retry: None,
        }
    }

    fn stop(&mut self) {
        for (_, mut child) in self.watchers.drain(..) {
            stop_child(&mut child);
        }
        self.lock = None;
    }

    fn capturing(&mut self) -> bool {
        !self.watchers.is_empty()
            && self
                .watchers
                .iter_mut()
                .all(|(_, child)| child.try_wait().ok().flatten().is_none())
    }

    fn update(&mut self, history: &mut History, config: &ClipboardPreferences) {
        let _ = history.configure(config.clipboard_limit);
        let mut wanted: Vec<&'static str> = vec!["text"];
        if config.clipboard_images {
            wanted.push("image");
        }
        let enabled = config.clipboard_enabled && !history.paused();
        if !enabled {
            self.stop();
            self.error = String::new();
            return;
        }
        let kinds: Vec<&str> = self.watchers.iter().map(|(kind, _)| *kind).collect();
        let alive = self.capturing();
        if kinds.len() == wanted.len() && kinds.iter().all(|kind| wanted.contains(kind)) && alive {
            return;
        }
        if !self.watchers.is_empty() && !alive {
            self.error =
                "Clipboard capture stopped. Check Wayland data-control support and the session."
                    .to_string();
        }
        if self
            .last_retry
            .is_some_and(|retry| retry.elapsed().as_secs_f64() < 5.0)
        {
            return;
        }
        self.last_retry = Some(Instant::now());
        self.stop();
        let paste = match crate::pyutil::which("wl-paste") {
            Some(path) => path,
            None => {
                self.error =
                    "Install wl-clipboard to capture and copy clipboard items.".to_string();
                return;
            }
        };
        if crate::pyutil::which("wl-copy").is_none() {
            self.error = "Install wl-clipboard to capture and copy clipboard items.".to_string();
            return;
        }
        let lock_path = history.directory.join("capture.lock");
        let lock = match std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(_) => {
                self.error =
                    "Clipboard capture could not start. Check the Wayland session.".to_string();
                return;
            }
        };
        match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(_) => {
                self.error = "Another shell instance owns clipboard capture.".to_string();
                return;
            }
        }
        self.lock = Some(lock);
        let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lme-backend"));
        for kind in wanted {
            let child = Command::new(&paste)
                .args([
                    "--no-newline",
                    "--type",
                    kind,
                    "--watch",
                    executable.to_string_lossy().as_ref(),
                    "--capture",
                    kind,
                    "--directory",
                    history.directory.to_string_lossy().as_ref(),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match child {
                Ok(child) => self.watchers.push((kind, child)),
                Err(_) => {
                    self.stop();
                    self.error =
                        "Clipboard capture could not start. Check the Wayland session.".to_string();
                    return;
                }
            }
        }
        self.error = String::new();
    }
}

impl Drop for CaptureService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stop_child(child: &mut std::process::Child) {
    terminate_process(child);
}

enum Input {
    Line(String),
    Eof,
}

/// One-shot capture mode (`--capture kind`).
pub fn capture(kind: &str, directory: &Path, data: &[u8]) {
    if std::env::var("CLIPBOARD_STATE").unwrap_or_else(|_| "data".to_string()) != "data" {
        return;
    }
    let config = clipboard_preferences();
    if !config.clipboard_enabled || (kind == "image" && !config.clipboard_images) {
        return;
    }
    let Ok(mut history) = History::new(directory, config.clipboard_limit, MAX_TOTAL) else {
        return;
    };
    let _ = history.store(kind, data);
}

/// Clipboard daemon mode (`--clipboard`).
pub fn serve(directory: &Path) -> std::process::ExitCode {
    let _ = rustix::process::umask(rustix::fs::Mode::from_bits_retain(0o077));
    let mut history = match History::new(directory, 100, MAX_TOTAL) {
        Ok(history) => history,
        Err(error) => {
            eprintln!("Clipboard history could not be opened: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut capture = CaptureService::new();
    let (sender, receiver) = std::sync::mpsc::channel::<Input>();
    std::thread::Builder::new()
        .name("lme-clip-stdin".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut handle = stdin.lock();
            let mut line = String::new();
            loop {
                line.clear();
                match handle.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = sender.send(Input::Eof);
                        return;
                    }
                    Ok(_) => {
                        let _ = sender.send(Input::Line(line.trim_end_matches('\n').to_string()));
                    }
                }
            }
        })
        .expect("clipboard stdin thread");

    let mut query = String::new();
    let mut kind = "all".to_string();
    let mut last_result: Option<Map<String, Value>> = None;
    let mut sequence: i64 = 0;
    let mut last_config = Instant::now() - Duration::from_secs(2);
    let mut available = false;
    type Signature = (i64, bool, bool, bool, String, i64, String, String);
    let mut signature: Option<Signature> = None;

    loop {
        if last_config.elapsed().as_secs_f64() >= 1.0 {
            capture.update(&mut history, &clipboard_preferences());
            available = crate::pyutil::which("wl-copy").is_some()
                && crate::pyutil::which("wl-paste").is_some();
            last_config = Instant::now();
        }
        let revision = history.revision();
        let paused = history.paused();
        let capturing = capture.capturing();
        let current = (
            revision,
            paused,
            capturing,
            available,
            capture.error.clone(),
            sequence,
            query.clone(),
            kind.clone(),
        );
        if signature.as_ref() != Some(&current) {
            // Poll a tiny revision counter; do not sort/serialize all history
            // four times a second when the clipboard is idle.
            let state = json!({
                "entries": history.entries(&query, &kind).unwrap_or_default(),
                "paused": paused, "capturing": capturing, "available": available,
                "total": history.total(), "revision": revision,
                "error": capture.error, "lastResult": last_result,
            });
            if crate::emit(&state).is_err() {
                break;
            }
            signature = Some(current);
        }
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) | Ok(Input::Eof) => break,
            Ok(Input::Line(line)) => {
                let mut action = String::new();
                let mut ok = true;
                let mut message = String::new();
                let request: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(_) => {
                        action = String::new();
                        ok = false;
                        message = "Invalid JSON input".to_string();
                        Value::Null
                    }
                };
                if !request.is_null() {
                    action = request
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let outcome: Result<(), String> = (|| match action.as_str() {
                        "search" => {
                            let request_query = request.get("query").and_then(Value::as_str);
                            let request_kind = request.get("kind").and_then(Value::as_str);
                            let (Some(request_query), Some(request_kind)) =
                                (request_query, request_kind)
                            else {
                                return Err("Invalid search".to_string());
                            };
                            if !matches!(request_kind, "all" | "text" | "image") {
                                return Err("Invalid search".to_string());
                            }
                            query = request_query.chars().take(300).collect();
                            kind = request_kind.to_string();
                            Ok(())
                        }
                        "copy" => {
                            let (_id, mime, data) =
                                history.item(request.get("id").unwrap_or(&Value::Null))?;
                            let Some(wl_copy) = crate::pyutil::which("wl-copy") else {
                                return Err("Install wl-clipboard to copy items".to_string());
                            };
                            let mut child = Command::new(wl_copy)
                                    .args(["--type", &mime])
                                    .stdin(Stdio::piped())
                                    .stdout(Stdio::null())
                                    .stderr(Stdio::null())
                                    .spawn()
                                    .map_err(|_| {
                                        "Clipboard operation failed. Check the Wayland session and local storage.".to_string()
                                    })?;
                            if let Some(stdin) = child.stdin.as_mut() {
                                let _ = stdin.write_all(&data);
                            }
                            drop(child.stdin.take());
                            match wait_with_timeout(&mut child, Duration::from_secs(4)) {
                                Some(status) if status.success() => {}
                                _ => {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    return Err("Clipboard operation failed. Check the Wayland session and local storage.".to_string());
                                }
                            }
                            message = "Copied to clipboard".to_string();
                            Ok(())
                        }
                        "pin" => history.pin(request.get("id").unwrap_or(&Value::Null)),
                        "delete" => history.delete(request.get("id").unwrap_or(&Value::Null)),
                        "clear" => {
                            let include_pinned = request
                                .get("includePinned")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            history.clear(include_pinned)
                        }
                        "pause" => {
                            let value = request.get("value").and_then(Value::as_bool);
                            let Some(value) = value else {
                                return Err("Pause must be true or false".to_string());
                            };
                            history.pause(value)?;
                            capture.last_retry = None;
                            capture.update(&mut history, &clipboard_preferences());
                            Ok(())
                        }
                        _ => Err("Unknown clipboard action".to_string()),
                    })();
                    match outcome {
                        Ok(()) => {}
                        Err(error) => {
                            ok = false;
                            message = if matches!(
                                action.as_str(),
                                "search" | "copy" | "pin" | "delete" | "clear" | "pause"
                            ) && is_value_error(&error)
                            {
                                error.chars().take(180).collect()
                            } else {
                                "Clipboard operation failed. Check the Wayland session and local storage.".to_string()
                            };
                            if action == "search" {
                                query = String::new();
                                kind = "all".to_string();
                            }
                        }
                    }
                }
                sequence += 1;
                last_result = Some(
                    json!({
                        "action": action, "ok": ok, "message": message, "sequence": sequence,
                    })
                    .as_object()
                    .cloned()
                    .unwrap(),
                );
            }
        }
    }
    capture.stop();
    std::process::ExitCode::SUCCESS
}

/// Distinguish surfaced ValueError messages from generic failures, mirroring
/// the Python `isinstance(error, ValueError)` check.
fn is_value_error(message: &str) -> bool {
    matches!(
        message,
        "Invalid clipboard item"
            | "This clipboard item is no longer available"
            | "Invalid search"
            | "Unknown clipboard action"
            | "Unknown history filter"
            | "Unknown clipboard type"
            | "Clipboard item exceeds the size limit"
            | "Unsupported clipboard image format"
            | "Up to 20 clipboard items can be pinned"
            | "History limit must be between 20 and 500"
            | "Pause must be true or false"
            | "Install wl-clipboard to copy items"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_history(name: &str) -> (PathBuf, History) {
        let directory =
            std::env::temp_dir().join(format!("lme-clip-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let history = History::new(&directory, 100, MAX_TOTAL).unwrap();
        (directory, history)
    }

    #[test]
    fn store_text_and_search() {
        let (_directory, mut history) = temp_history("store");
        let id = history.store("text", b"Hello World").unwrap().unwrap();
        assert_eq!(id.len(), 64);
        // Ids hash the raw bytes; identical data re-stores under one id.
        let id_again = history.store("text", b"Hello World").unwrap().unwrap();
        assert_eq!(id, id_again);
        // Casefold only affects the search column, not the identity.
        let id2 = history.store("text", b"hello world").unwrap().unwrap();
        assert_ne!(id, id2);
        history.store("text", "unicode ✓ text".as_bytes()).unwrap();
        // The search column is casefolded, so "hello" matches both
        // "Hello World" and "hello world" — same as the Python service.
        let results = history.entries("hello", "all").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["bytes"], json!(11));
        let results = history.entries("unicode", "all").unwrap();
        assert_eq!(results.len(), 1);
        let results = history.entries("", "all").unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(history.total(), 3);
    }

    #[test]
    fn pin_limit_and_delete() {
        let (_directory, mut history) = temp_history("pin");
        for index in 0..20 {
            let id = history
                .store("text", format!("item {index}").as_bytes())
                .unwrap()
                .unwrap();
            history.pin(&Value::String(id)).unwrap();
        }
        let extra = history.store("text", b"one pin too many").unwrap().unwrap();
        assert_eq!(
            history.pin(&Value::String(extra)).unwrap_err(),
            "Up to 20 clipboard items can be pinned"
        );
        let count = history
            .db
            .query_row("SELECT count(*) FROM entries WHERE pinned=1", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(count, 20, "pin limit enforced");
        let first = history.entries("", "all").unwrap()[0]["id"].clone();
        history.delete(&first).unwrap();
        assert_eq!(history.total(), 20);
    }

    #[test]
    fn image_sniffing_and_store() {
        let (_directory, mut history) = temp_history("image");
        let png = b"\x89PNG\r\n\x1a\nrest-of-image";
        let id = history.store("image", png).unwrap().unwrap();
        let results = history.entries("", "image").unwrap();
        assert_eq!(results[0]["preview"], json!("PNG image · 0 KB"));
        assert!(results[0]["image"].as_str().unwrap().starts_with("file://"));
        assert!(results[0]["image"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{id}.png")));
        assert!(history.store("image", b"not an image").is_err());
    }

    #[test]
    fn trim_respects_limit_and_pins() {
        let (directory, mut history) = temp_history("trim");
        history.limit = 3;
        for index in 0..5 {
            history
                .store("text", format!("entry {index}").as_bytes())
                .unwrap();
        }
        history.trim().unwrap();
        assert_eq!(history.total(), 3);
        // Pinned entries survive the trim.
        let id = history.entries("", "all").unwrap()[0]["id"].clone();
        history.pin(&id).unwrap();
        history.limit = 1;
        history.trim().unwrap();
        assert_eq!(history.total(), 1);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn clear_and_pause() {
        let (directory, mut history) = temp_history("clear");
        history.store("text", b"one").unwrap();
        history.store("text", b"two").unwrap();
        let id = history.entries("", "all").unwrap()[0]["id"].clone();
        history
            .pin(&Value::String(id.as_str().unwrap().to_string()))
            .unwrap();
        history.clear(false).unwrap();
        assert_eq!(history.total(), 1, "pinned survives clear");
        history.pause(true).unwrap();
        assert!(history.paused());
        assert_eq!(history.store("text", b"paused").unwrap(), None);
        history.pause(false).unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn image_type_sniffs_all_formats() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n").unwrap().1, "png");
        assert_eq!(image_type(b"\xff\xd8\xff\xe0").unwrap().1, "jpg");
        assert_eq!(image_type(b"GIF89a").unwrap().1, "gif");
        assert_eq!(image_type(b"BM1000").unwrap().1, "bmp");
        assert_eq!(image_type(b"RIFF\x00\x00\x00\x00WEBP").unwrap().1, "webp");
        assert!(image_type(b"XXXX").is_err());
    }
}
