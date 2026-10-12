// SPDX-License-Identifier: GPL-3.0-or-later
//! MPRIS media feed (Tier 1.2): live `org.mpris.MediaPlayer2.*` player
//! tracking for the bar media module (legacy BarModule.qml:
//! `Mpris.players.values.find(p => p.isPlaying) || players[0]`, caption
//! `trackTitle || identity`, module hidden while no player exists).
//!
//! Threading (ADR 0002 / D4, the notifications/tray pattern): one manager
//! thread owns a blocking connection; per-player PropertiesChanged
//! listeners and the NameOwnerChanged watcher forward ManagerMsgs to it,
//! and it forwards MprisEvents to the daemon's calloop loop. A missing
//! session bus is non-fatal — the media module simply stays hidden.
//!
//! Scope (Tier 1.2): display feed only. Transport controls (PlayPause /
//! Next / Previous) land with the Tier-2 activity island, so the handle
//! carries no commands yet — it only keeps the manager alive.

use std::collections::HashMap;
use std::sync::Arc;

use zbus::zvariant::OwnedValue;

pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// One player snapshot (rebuilt wholesale on PropertiesChanged — the
/// legacy Quickshell Mpris model exposes the same fields).
#[derive(Clone)]
pub struct MprisPlayer {
    /// Bus name (`org.mpris.MediaPlayer2.<suffix>`).
    pub bus: String,
    /// Root Identity (the app name), bus suffix as fallback.
    pub identity: String,
    /// `xesam:title` ("" when the player exposes none).
    pub track_title: String,
    /// `xesam:artist` entries.
    pub artists: Vec<String>,
    /// Playing | Paused | Stopped.
    pub status: String,
}

impl MprisPlayer {
    pub fn is_playing(&self) -> bool {
        self.status == "Playing"
    }

    /// Legacy caption: trackTitle || identity.
    pub fn display_title(&self) -> &str {
        if !self.track_title.is_empty() {
            &self.track_title
        } else {
            &self.identity
        }
    }

    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }
}

// ---------------------------------------------------------------------------
// Events / manager plumbing
// ---------------------------------------------------------------------------

/// Manager → daemon (calloop channel).
pub enum MprisEvent {
    /// A player appeared or refreshed (upsert by bus name).
    Player(Box<MprisPlayer>),
    /// A player's bus name lost its owner (app quit).
    Gone { bus: String },
}

/// Everything the manager thread consumes. Refresh messages collapse:
/// the listener only forwards interesting PropertiesChanged members.
enum ManagerMsg {
    AddPlayer { bus: String },
    Gone { bus: String },
    Refresh { bus: String },
}

/// Handle to the running manager. Dropping it stops the manager thread,
/// which drops the connection and every listener. No commands yet —
/// controls arrive with the Tier-2 activity island.
#[derive(Clone)]
pub struct MprisHandle {
    _tx: Arc<std::sync::mpsc::Sender<ManagerMsg>>,
}

/// Start the MPRIS feed. Fails softly (no session bus) exactly like
/// notifications::spawn / tray::spawn.
pub fn spawn(tx: calloop::channel::Sender<MprisEvent>) -> Result<MprisHandle, String> {
    let (mgr_tx, mgr_rx) = std::sync::mpsc::channel::<ManagerMsg>();
    let mgr_tx = Arc::new(mgr_tx);
    let conn = zbus::blocking::connection::Builder::session()
        .map_err(|e| format!("session bus: {e}"))?
        .build()
        .map_err(|e| format!("connect: {e}"))?;

    // Initial scan: every owned MediaPlayer2 name.
    let dbus = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|e| format!("dbus proxy: {e}"))?;
    let names: Vec<String> = dbus
        .call("ListNames", &())
        .map_err(|e| format!("ListNames: {e}"))?;
    for name in names {
        if name.starts_with(MPRIS_PREFIX) {
            // the manager thread's records deduplicate
            mgr_tx
                .send(ManagerMsg::AddPlayer { bus: name })
                .expect("manager alive at spawn");
        }
    }

    // Owner changes: new players appear, quitting players disappear.
    spawn_owner_watcher(&conn, &mgr_tx);

    let handle_tx = Arc::clone(&mgr_tx);
    std::thread::spawn(move || {
        let mut records: HashMap<String, ()> = HashMap::new();
        for msg in mgr_rx {
            match msg {
                ManagerMsg::AddPlayer { bus } => {
                    if records.contains_key(&bus) {
                        continue;
                    }
                    match fetch_player(&conn, &bus) {
                        Ok(player) => {
                            records.insert(bus.clone(), ());
                            spawn_player_listener(&conn, &mgr_tx, &bus);
                            let _ = tx.send(MprisEvent::Player(Box::new(player)));
                        }
                        Err(e) => eprintln!("lme-shell: mpris {bus}: {e}"),
                    }
                }
                ManagerMsg::Gone { bus } => {
                    if records.remove(&bus).is_some() {
                        let _ = tx.send(MprisEvent::Gone { bus });
                    }
                }
                ManagerMsg::Refresh { bus } => {
                    if records.contains_key(&bus) {
                        match fetch_player(&conn, &bus) {
                            Ok(player) => {
                                let _ = tx.send(MprisEvent::Player(Box::new(player)));
                            }
                            Err(e) => eprintln!("lme-shell: mpris refresh {bus}: {e}"),
                        }
                    }
                }
            }
        }
        // mgr_rx closed: daemon is shutting down; threads die with the conn.
    });
    Ok(MprisHandle { _tx: handle_tx })
}

/// NameOwnerChanged → AddPlayer / Gone (MediaPlayer2 names only).
fn spawn_owner_watcher(
    conn: &zbus::blocking::Connection,
    mgr_tx: &Arc<std::sync::mpsc::Sender<ManagerMsg>>,
) {
    let Ok(proxy) = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    ) else {
        return;
    };
    let Ok(changes) = proxy.receive_signal("NameOwnerChanged") else {
        return;
    };
    let tx = Arc::clone(mgr_tx);
    std::thread::spawn(move || {
        for msg in changes {
            let Ok((name, old, new)) = msg.body().deserialize::<(String, String, String)>() else {
                continue;
            };
            if !name.starts_with(MPRIS_PREFIX) {
                continue;
            }
            let res = if new.is_empty() && !old.is_empty() {
                tx.send(ManagerMsg::Gone { bus: name })
            } else if !new.is_empty() && old.is_empty() {
                tx.send(ManagerMsg::AddPlayer { bus: name })
            } else {
                continue;
            };
            if res.is_err() {
                break;
            }
        }
    });
}

/// One listener per player. NB: zbus `receive_all_signals` filters on the
/// PROXY's interface — `org.freedesktop.DBus.Properties.PropertiesChanged`
/// (the update signal real players emit) would never match a
/// MediaPlayer2.Player proxy. Subscribe on a Properties-interface proxy at
/// the same destination/path instead (D24).
fn spawn_player_listener(
    conn: &zbus::blocking::Connection,
    mgr_tx: &Arc<std::sync::mpsc::Sender<ManagerMsg>>,
    bus: &str,
) {
    let Ok(proxy) =
        zbus::blocking::Proxy::new(conn, bus, MPRIS_PATH, "org.freedesktop.DBus.Properties")
    else {
        return;
    };
    let Ok(signals) = proxy.receive_signal("PropertiesChanged") else {
        return;
    };
    let bus = bus.to_string();
    let tx = Arc::clone(mgr_tx);
    std::thread::spawn(move || {
        for _msg in signals {
            if tx.send(ManagerMsg::Refresh { bus: bus.clone() }).is_err() {
                break;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Daemon-side state (impl Shell, the tray.rs pattern)
// ---------------------------------------------------------------------------

impl crate::daemon::Shell {
    /// MprisEvent handler (calloop channel source).
    pub fn mpris_event(&mut self, state: &mut lme_ui::wayland::UiState, ev: MprisEvent) {
        match ev {
            MprisEvent::Player(p) => match self.mpris.iter().position(|x| x.bus == p.bus) {
                Some(i) => self.mpris[i] = *p,
                None => self.mpris.push(*p),
            },
            MprisEvent::Gone { bus } => {
                self.mpris.retain(|p| p.bus != bus);
            }
        }
        self.update_media_title();
        self.rebuild_bars(state);
    }

    /// Legacy pick: the playing player, else the first; caption
    /// trackTitle || identity; None (module hidden) when no player.
    pub fn update_media_title(&mut self) {
        self.media_title = self
            .mpris
            .iter()
            .find(|p| p.is_playing())
            .or_else(|| self.mpris.first())
            .map(|p| p.display_title().to_string());
    }
}

/// Read one player snapshot (Identity + PlaybackStatus + Metadata).
fn fetch_player(conn: &zbus::blocking::Connection, bus: &str) -> Result<MprisPlayer, String> {
    let root = zbus::blocking::Proxy::new(conn, bus, MPRIS_PATH, ROOT_IFACE)
        .map_err(|e| format!("root proxy: {e}"))?;
    let player = zbus::blocking::Proxy::new(conn, bus, MPRIS_PATH, PLAYER_IFACE)
        .map_err(|e| format!("player proxy: {e}"))?;

    let identity: String = root
        .get_property("Identity")
        .unwrap_or_else(|_| bus.trim_start_matches(MPRIS_PREFIX).to_string());
    let status: String = player.get_property("PlaybackStatus").unwrap_or_default();
    let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata").unwrap_or_default();

    // OwnedValue derefs to Value — match through it.
    let track_title = match metadata.get("xesam:title") {
        Some(v) => match &**v {
            zbus::zvariant::Value::Str(s) => s.to_string(),
            _ => String::new(),
        },
        None => String::new(),
    };
    let artists = match metadata.get("xesam:artist") {
        Some(v) => match &**v {
            zbus::zvariant::Value::Array(a) => a
                .iter()
                .filter_map(|e| match e {
                    zbus::zvariant::Value::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    Ok(MprisPlayer {
        bus: bus.to_string(),
        identity,
        track_title,
        artists,
        status,
    })
}
