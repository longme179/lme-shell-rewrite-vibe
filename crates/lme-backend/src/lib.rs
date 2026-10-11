//! lme-backend: native Rust backend for LME Shell, JSON-lines compatible
//! with scripts/desktop_backend.py (see docs/REWRITE_INVENTORY.md).

pub mod actions;
pub mod apps;
pub mod backend;
pub mod charging;
pub mod clipboard;
pub mod loopd;
pub mod niri;
pub mod prefs;
pub mod probes;
pub mod pyutil;
pub mod runner;
pub mod weather;

/// Serialize + emit one snapshot line to stdout (compact separators,
/// UTF-8, like `json.dumps(..., ensure_ascii=False, separators=(",", ":"))`).
pub fn emit(value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    let text = serde_json::to_string(value).unwrap_or_default();
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(text.as_bytes())?;
    handle.write_all(b"\n")?;
    handle.flush()
}

pub use weather::real_wall_clock as wall_clock_now;
