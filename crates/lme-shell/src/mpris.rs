// SPDX-License-Identifier: GPL-3.0-or-later
//! MPRIS media feed (Tier 1.2): live `org.mpris.MediaPlayer2.*` player
//! tracking for the bar media module (legacy BarModule.qml:
//! `Mpris.players.values.find(p => p.isPlaying) || players[0]`, caption
//! `trackTitle || identity`, module hidden while no player exists).
//!
//! Threading (D25): ONE thread runs the whole module inside a single
//! `async_io::block_on` — connection, NameOwnerChanged watcher, per-player
//! PropertiesChanged streams and every method call live as futures on one
//! executor, selected with `futures_lite::stream::select_all`. The previous
//! shape (a zbus BLOCKING connection shared by manager/watcher/listener
//! threads, the D20 pattern) hit an intermittent zbus 5.19 deadlock: the
//! first cross-thread blocking `get_property` after a signal-triggered
//! wake sometimes never saw its reply (Get went out, peer replied, socket
//! drained — the reply was never dispatched; every executor driver parked
//! and the async-io reactor had no interests). One-thread/one-block_on is
//! the canonical zbus shape and cannot hit that cross-thread waker chain.
//! A missing session bus is non-fatal — the module simply stays hidden.
//!
//! Scope (Tier 1.2): display feed only. Transport controls (PlayPause /
//! Next / Previous) land with the Tier-2 activity island, so the handle
//! carries no commands yet — it only keeps the feed alive.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_lite::stream::Stream;
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
// Events / feed plumbing
// ---------------------------------------------------------------------------

/// Feed → daemon (calloop channel).
pub enum MprisEvent {
    /// A player appeared or refreshed (upsert by bus name).
    Player(Box<MprisPlayer>),
    /// A player's bus name lost its owner (app quit).
    Gone { bus: String },
}

/// Handle to the running feed. Dropping the last clone stops the feed
/// thread (the shutdown channel closes and the select loop breaks), which
/// drops the connection and every stream. No commands yet — controls
/// arrive with the Tier-2 activity island.
#[derive(Clone)]
pub struct MprisHandle {
    _tx: async_channel::Sender<()>,
}

/// Start the MPRIS feed. Fails softly (no session bus) exactly like
/// notifications::spawn / tray::spawn.
pub fn spawn(tx: calloop::channel::Sender<MprisEvent>) -> Result<MprisHandle, String> {
    let (stop_tx, stop_rx) = async_channel::unbounded::<()>();
    std::thread::Builder::new()
        .name("lme-mpris".into())
        .spawn(move || {
            if let Err(e) = async_io::block_on(run_feed(tx, stop_rx)) {
                eprintln!("lme-shell: mpris feed ended: {e}");
            }
        })
        .map_err(|e| format!("spawn mpris thread: {e}"))?;
    Ok(MprisHandle { _tx: stop_tx })
}

/// What wakes the feed loop.
enum FeedEvent {
    /// A NameOwnerChanged message (decoded in the loop).
    Owner(zbus::message::Message),
    /// A player emitted PropertiesChanged (bus name).
    Refresh(String),
    /// Periodic reconciliation tick (D25: the push signal path
    /// intermittently loses its wakeup inside zbus/async-io in the full
    /// daemon; the tick makes the feed eventually consistent within ~1 s
    /// regardless — missed registers, missed NameLost and hung property
    /// reads all recover on the next tick).
    Tick,
    /// All handles dropped — shut down.
    Stop,
}

/// Reconcile cadence: fast while waiting for the first player, slow while
/// one is live (idle budget: ≤1 wakeup/s; see docs/PERF.md).
const TICK_EMPTY: std::time::Duration = std::time::Duration::from_secs(1);
const TICK_LIVE: std::time::Duration = std::time::Duration::from_secs(5);
/// Upper bound for any single D-Bus round-trip inside the feed (a wedged
/// call must not block the tick loop; zbus's own default is 25 s).
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Race a future against [`CALL_TIMEOUT`] (Err(()) = timed out).
async fn with_timeout<T>(fut: impl std::future::Future<Output = T>) -> Result<T, ()> {
    futures_lite::future::or(
        async {
            async_io::Timer::after(CALL_TIMEOUT).await;
            Err(())
        },
        async { Ok(fut.await) },
    )
    .await
}

/// The feed's event sources, polled as one stream: the NameOwnerChanged
/// stream, every player's PropertiesChanged stream and the shutdown
/// channel. (futures-lite has no select_all; this is ~30 lines and keeps
/// everything on the single feed thread.)
struct Feeds {
    stop: Pin<Box<async_channel::Receiver<()>>>,
    owner: Option<Pin<Box<zbus::proxy::SignalStream<'static>>>>,
    players: Vec<(String, Pin<Box<zbus::proxy::SignalStream<'static>>>)>,
    /// One-shot timer, recreated after each tick.
    timer: Pin<Box<async_io::Timer>>,
}

impl Feeds {
    fn new(
        stop_rx: async_channel::Receiver<()>,
        owner_changes: zbus::proxy::SignalStream<'static>,
    ) -> Self {
        Self {
            stop: Box::pin(stop_rx),
            owner: Some(Box::pin(owner_changes)),
            players: Vec::new(),
            timer: Box::pin(async_io::Timer::after(TICK_EMPTY)),
        }
    }
}

impl Stream for Feeds {
    type Item = FeedEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<FeedEvent>> {
        let this = &mut *self;
        // shutdown channel first (cheap; None = all handles dropped)
        match this.stop.as_mut().poll_next(cx) {
            Poll::Ready(Some(())) => return Poll::Ready(Some(FeedEvent::Stop)),
            Poll::Ready(None) => return Poll::Ready(Some(FeedEvent::Stop)),
            Poll::Pending => {}
        }
        // reconciliation tick (D25)
        use std::future::Future as _;
        if this.timer.as_mut().poll(cx).is_ready() {
            let period = if this.players.is_empty() {
                TICK_EMPTY
            } else {
                TICK_LIVE
            };
            this.timer = Box::pin(async_io::Timer::after(period));
            return Poll::Ready(Some(FeedEvent::Tick));
        }
        if let Some(owner) = this.owner.as_mut() {
            match owner.as_mut().poll_next(cx) {
                Poll::Ready(Some(msg)) => return Poll::Ready(Some(FeedEvent::Owner(msg))),
                // the owner stream never ends while the connection lives
                Poll::Ready(None) => this.owner = None,
                Poll::Pending => {}
            }
        }
        let mut i = 0;
        while i < this.players.len() {
            match this.players[i].1.as_mut().poll_next(cx) {
                Poll::Ready(Some(_)) => {
                    return Poll::Ready(Some(FeedEvent::Refresh(this.players[i].0.clone())))
                }
                // player vanished without NameOwnerChanged: drop the stream
                Poll::Ready(None) => {
                    this.players.remove(i);
                }
                Poll::Pending => i += 1,
            }
        }
        Poll::Pending
    }
}

async fn run_feed(
    tx: calloop::channel::Sender<MprisEvent>,
    stop_rx: async_channel::Receiver<()>,
) -> Result<(), String> {
    let conn = zbus::connection::Builder::session()
        .map_err(|e| format!("session bus: {e}"))?
        .build()
        .await
        .map_err(|e| format!("connect: {e}"))?;
    if std::env::var_os("LME_MPRIS_DEBUG").is_some() {
        eprintln!("lme-mpris: connected");
    }

    let dbus = zbus::Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    .map_err(|e| format!("dbus proxy: {e}"))?;

    // Subscribe BEFORE scanning (D25): a player that registers between the
    // ListNames scan and the NameOwnerChanged subscription would be missed
    // forever. Queued stream events + the players map deduplicate overlap.
    let owner_changes = dbus
        .receive_signal("NameOwnerChanged")
        .await
        .map_err(|e| format!("NameOwnerChanged subscribe: {e}"))?;
    if std::env::var_os("LME_MPRIS_DEBUG").is_some() {
        eprintln!("lme-mpris: subscribed");
    }

    // Initial scan: every owned MediaPlayer2 name.
    let names: Vec<String> = dbus
        .call("ListNames", &())
        .await
        .map_err(|e| format!("ListNames: {e}"))?;

    // Per-player PropertiesChanged streams. NB: subscribe on a
    // Properties-INTERFACE proxy — zbus receive_* filters on the proxy's
    // own interface, so a MediaPlayer2.Player proxy would never see
    // PropertiesChanged (D24).
    let mut feeds = Feeds::new(stop_rx, owner_changes);
    for name in names {
        if name.starts_with(MPRIS_PREFIX) {
            add_player(&conn, &tx, &mut feeds.players, &name).await;
        }
    }

    // The feed loop: every source polled as one stream on this thread.
    use futures_lite::StreamExt as _;
    while let Some(ev) = feeds.next().await {
        match ev {
            FeedEvent::Stop => break,
            FeedEvent::Tick => {
                // Reconcile (D25): a full ListNames diff repairs anything
                // the push path missed or lost to a wedged call.
                let scan = with_timeout(dbus.call::<_, _, Vec<String>>("ListNames", &())).await;
                let live = match scan {
                    Err(()) => {
                        eprintln!("lme-shell: mpris rescan timed out");
                        continue;
                    }
                    Ok(Ok(names)) => names,
                    Ok(Err(e)) => {
                        eprintln!("lme-shell: mpris rescan: {e}");
                        continue;
                    }
                };
                let live: Vec<String> = live
                    .into_iter()
                    .filter(|n| n.starts_with(MPRIS_PREFIX))
                    .collect();
                for name in &live {
                    if feeds.players.iter().all(|(b, _)| b != name) {
                        add_player(&conn, &tx, &mut feeds.players, name).await;
                    }
                }
                let stale: Vec<String> = feeds
                    .players
                    .iter()
                    .map(|(b, _)| b.clone())
                    .filter(|b| !live.contains(b))
                    .collect();
                for bus in stale {
                    feeds.players.retain(|(b, _)| *b != bus);
                    let _ = tx.send(MprisEvent::Gone { bus });
                }
            }
            FeedEvent::Owner(msg) => {
                let Ok((name, old, new)) = msg.body().deserialize::<(String, String, String)>()
                else {
                    continue;
                };
                if !name.starts_with(MPRIS_PREFIX) {
                    continue;
                }
                if new.is_empty() && !old.is_empty() {
                    if feeds.players.iter().position(|(b, _)| *b == name).is_some() {
                        feeds.players.retain(|(b, _)| *b != name);
                        let _ = tx.send(MprisEvent::Gone { bus: name });
                    }
                } else if !new.is_empty()
                    && old.is_empty()
                    && feeds.players.iter().all(|(b, _)| *b != name)
                {
                    add_player(&conn, &tx, &mut feeds.players, &name).await;
                }
            }
            FeedEvent::Refresh(bus) => match with_timeout(fetch_player(&conn, &bus)).await {
                Err(()) => eprintln!("lme-shell: mpris refresh {bus}: timed out"),
                Ok(Ok(player)) => {
                    let _ = tx.send(MprisEvent::Player(Box::new(player)));
                }
                Ok(Err(e)) => eprintln!("lme-shell: mpris refresh {bus}: {e}"),
            },
        }
    }
    Ok(())
}

/// Fetch the snapshot, subscribe to its property changes, announce it.
async fn add_player(
    conn: &zbus::Connection,
    tx: &calloop::channel::Sender<MprisEvent>,
    players: &mut Vec<(String, Pin<Box<zbus::proxy::SignalStream<'static>>>)>,
    bus: &str,
) {
    let dbg = std::env::var_os("LME_MPRIS_DEBUG").is_some();
    if dbg {
        eprintln!("lme-mpris: add_player {bus} (fetching)");
    }
    match with_timeout(fetch_player(conn, bus)).await {
        Err(()) => {
            eprintln!("lme-shell: mpris {bus}: fetch timed out (will retry on tick)");
        }
        Ok(Ok(player)) => {
            let sub = with_timeout(async {
                let props = zbus::Proxy::new(
                    conn,
                    bus.to_string(),
                    MPRIS_PATH,
                    "org.freedesktop.DBus.Properties",
                )
                .await?;
                props.receive_signal("PropertiesChanged").await
            })
            .await;
            match sub {
                Err(()) => {
                    eprintln!("lme-shell: mpris listener {bus}: subscribe timed out");
                }
                Ok(Ok(stream)) => {
                    players.push((bus.to_string(), Box::pin(stream)));
                    let _ = tx.send(MprisEvent::Player(Box::new(player)));
                }
                Ok(Err(e)) => {
                    eprintln!("lme-shell: mpris listener {bus}: {e}");
                }
            }
        }
        Ok(Err(e)) => eprintln!("lme-shell: mpris {bus}: {e}"),
    }
    if dbg {
        eprintln!("lme-mpris: add_player {bus} done");
    }
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
/// CacheProperties::No (D25): plain `Get` calls, no background cache task.
async fn fetch_player(conn: &zbus::Connection, bus: &str) -> Result<MprisPlayer, String> {
    let root: zbus::Proxy<'_> = zbus::proxy::Builder::new(conn)
        .destination(bus)
        .map_err(|e| format!("destination: {e}"))?
        .path(MPRIS_PATH)
        .map_err(|e| format!("path: {e}"))?
        .interface(ROOT_IFACE)
        .map_err(|e| format!("interface: {e}"))?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .map_err(|e| format!("root proxy: {e}"))?;
    let player: zbus::Proxy<'_> = zbus::proxy::Builder::new(conn)
        .destination(bus)
        .map_err(|e| format!("destination: {e}"))?
        .path(MPRIS_PATH)
        .map_err(|e| format!("path: {e}"))?
        .interface(PLAYER_IFACE)
        .map_err(|e| format!("interface: {e}"))?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .map_err(|e| format!("player proxy: {e}"))?;

    let identity: String = root
        .get_property("Identity")
        .await
        .unwrap_or_else(|_| bus.trim_start_matches(MPRIS_PREFIX).to_string());
    if std::env::var_os("LME_MPRIS_DEBUG").is_some() {
        eprintln!("lme-mpris: fetched Identity={identity}");
    }
    let status: String = player
        .get_property("PlaybackStatus")
        .await
        .unwrap_or_default();
    let metadata: HashMap<String, OwnedValue> =
        player.get_property("Metadata").await.unwrap_or_default();

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
