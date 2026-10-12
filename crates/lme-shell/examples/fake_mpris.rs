// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake `org.mpris.MediaPlayer2` player for the media-module e2e test
//! (tests/golden.rs::mpris_media_module) — a Rust port of the legacy
//! `legacy/tests/mpris_fixture.py` (the spec source).
//!
//! Usage: `fake_mpris <log-file>` — connects to the session bus from
//! `DBUS_SESSION_BUS_ADDRESS` (the e2e private bus), owns
//! `org.mpris.MediaPlayer2.lmeTest`, serves the root + Player interfaces
//! at `/org/mpris/MediaPlayer2`, and appends received control calls
//! (`playpause`, `next`, `previous`, …) to the log file. PlayPause /
//! Next / Previous flip the shared state AND emit
//! `org.freedesktop.DBus.Properties.PropertiesChanged` exactly like a
//! real player, exercising the shell's refresh path. Exits when killed.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const SERVICE: &str = "org.mpris.MediaPlayer2.lmeTest";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

fn log(path: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[derive(Clone)]
struct PlayerState {
    status: String,
    title: String,
    log_path: String,
}

/// Root interface — static properties only, no state needed.
struct Mpris;

/// `org.mpris.MediaPlayer2` root interface.
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Mpris {
    #[zbus(property)]
    fn identity(&self) -> &str {
        "LME Test Player"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "lme-test"
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["file".to_string()]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec!["audio/ogg".to_string()]
    }
}

/// `org.mpris.MediaPlayer2.Player`.
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.state.lock().expect("state").status.clone()
    }
    #[zbus(property)]
    fn loop_status(&self) -> &str {
        "None"
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        0.5
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
    /// `a{sv}`: xesam:title, xesam:artist, mpris:trackid, mpris:length —
    /// exactly the fields the shell reads.
    #[zbus(property)]
    fn metadata(&self) -> std::collections::HashMap<String, OwnedValue> {
        let state = self.state.lock().expect("state");
        let mut map = std::collections::HashMap::new();
        map.insert(
            "xesam:title".to_string(),
            OwnedValue::try_from(Value::from(state.title.as_str())).expect("title value"),
        );
        map.insert(
            "xesam:artist".to_string(),
            OwnedValue::try_from(Value::from(vec!["Test Artist".to_string()]))
                .expect("artist value"),
        );
        map.insert(
            "mpris:trackid".to_string(),
            OwnedValue::try_from(Value::ObjectPath(
                ObjectPath::try_from("/track/1").expect("track path"),
            ))
            .expect("trackid value"),
        );
        map.insert(
            "mpris:length".to_string(),
            OwnedValue::try_from(Value::I64(120_000_000)).expect("length value"),
        );
        map
    }

    /// Flip status + emit PropertiesChanged(PlaybackStatus) like a real
    /// player (the legacy fixture does the same).
    fn play_pause(&self) -> zbus::fdo::Result<()> {
        let state = &mut *self.state.lock().expect("state");
        state.status = if state.status == "Playing" {
            "Paused".to_string()
        } else {
            "Playing".to_string()
        };
        log(&state.log_path, "playpause");
        let mut changed = std::collections::HashMap::new();
        changed.insert("PlaybackStatus", Value::from(state.status.clone()));
        emit_changed(&self.conn, changed);
        Ok(())
    }

    fn next(&self) -> zbus::fdo::Result<()> {
        let state = &mut *self.state.lock().expect("state");
        state.title = "Next track".to_string();
        log(&state.log_path, "next");
        let changed = std::collections::HashMap::from([("Metadata", metadata_value(&state.title))]);
        emit_changed(&self.conn, changed);
        Ok(())
    }

    fn previous(&self) -> zbus::fdo::Result<()> {
        let state = &mut *self.state.lock().expect("state");
        state.title = "Previous track".to_string();
        log(&state.log_path, "previous");
        let changed = std::collections::HashMap::from([("Metadata", metadata_value(&state.title))]);
        emit_changed(&self.conn, changed);
        Ok(())
    }

    fn play(&self) -> zbus::fdo::Result<()> {
        let state = &mut *self.state.lock().expect("state");
        state.status = "Playing".to_string();
        log(&state.log_path, "play");
        let changed = std::collections::HashMap::from([("PlaybackStatus", Value::from("Playing"))]);
        emit_changed(&self.conn, changed);
        Ok(())
    }

    fn pause(&self) -> zbus::fdo::Result<()> {
        let state = &mut *self.state.lock().expect("state");
        state.status = "Paused".to_string();
        log(&state.log_path, "pause");
        let changed = std::collections::HashMap::from([("PlaybackStatus", Value::from("Paused"))]);
        emit_changed(&self.conn, changed);
        Ok(())
    }

    fn stop(&self) -> zbus::fdo::Result<()> {
        let state = &mut *self.state.lock().expect("state");
        state.status = "Stopped".to_string();
        log(&state.log_path, "stop");
        let changed = std::collections::HashMap::from([("PlaybackStatus", Value::from("Stopped"))]);
        emit_changed(&self.conn, changed);
        Ok(())
    }
}

/// Build the PropertiesChanged body and emit (accepts whatever reference
/// depth the interface macro hands over via a Deref pass-through).
fn emit_changed(
    conn: &zbus::blocking::Connection,
    changed: std::collections::HashMap<&str, Value<'_>>,
) {
    emit_properties_changed(conn, changed);
}

fn emit_properties_changed(
    conn: &zbus::blocking::Connection,
    changed: std::collections::HashMap<&str, Value<'_>>,
) {
    let changed: std::collections::HashMap<String, OwnedValue> = changed
        .into_iter()
        .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v).expect("owned value")))
        .collect();
    let _ = conn.emit_signal(
        None::<&str>,
        PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(PLAYER_IFACE, changed, Vec::<String>::new()),
    );
}

fn metadata_value(title: &str) -> Value<'static> {
    let map: std::collections::HashMap<String, OwnedValue> = std::collections::HashMap::from([
        (
            "xesam:title".to_string(),
            OwnedValue::try_from(Value::from(title)).expect("title"),
        ),
        (
            "xesam:artist".to_string(),
            OwnedValue::try_from(Value::from(vec!["Test Artist".to_string()])).expect("artist"),
        ),
        (
            "mpris:trackid".to_string(),
            OwnedValue::try_from(Value::ObjectPath(
                ObjectPath::try_from("/track/2").expect("track path"),
            ))
            .expect("trackid"),
        ),
        (
            "mpris:length".to_string(),
            OwnedValue::try_from(Value::I64(180_000_000)).expect("length"),
        ),
    ]);
    Value::from(map)
}

struct Player {
    state: Arc<Mutex<PlayerState>>,
    conn: zbus::blocking::Connection,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: fake_mpris <log-file>");
        std::process::exit(2);
    }
    let log_path = args[1].clone();

    let state = Arc::new(Mutex::new(PlayerState {
        status: "Playing".to_string(),
        title: "Native track".to_string(),
        log_path: log_path.clone(),
    }));
    // connect first, then hand the interfaces a clone so their methods can
    // emit PropertiesChanged without macro-injected connection args
    let conn = zbus::blocking::connection::Builder::session()?.build()?;
    let root = Mpris;
    let player = Player {
        state: Arc::clone(&state),
        conn: conn.clone(),
    };
    conn.object_server().at(PATH, root)?;
    conn.object_server().at(PATH, player)?;
    conn.request_name(SERVICE)?;
    log(&log_path, "registered");

    // Serve until killed (NameOwnerChanged then exercises the removal).
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
