// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal reproducer for the MPRIS e2e stall (D25 investigation):
//! the exact async shape of mpris::run_feed — one thread, one block_on,
//! NameOwnerChanged subscription + Get on the discovered player — with no
//! daemon, no wayland, no other connections. Usage:
//!   mpris_probe <result-log> — exits 0 when the player was tracked
//!   (writes "tracked" to the log); a stall hangs until killed and the
//!   harness reports the timeout.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_lite::stream::Stream;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";

enum Feed {
    Owner(zbus::message::Message),
    Stop,
}

struct Feeds {
    stop: Pin<Box<async_channel::Receiver<()>>>,
    owner: Option<Pin<Box<zbus::proxy::SignalStream<'static>>>>,
}

impl Stream for Feeds {
    type Item = Feed;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Feed>> {
        let this = &mut *self;
        match this.stop.as_mut().poll_next(cx) {
            Poll::Ready(_) => return Poll::Ready(Some(Feed::Stop)),
            Poll::Pending => {}
        }
        if let Some(owner) = this.owner.as_mut() {
            match owner.as_mut().poll_next(cx) {
                Poll::Ready(Some(msg)) => return Poll::Ready(Some(Feed::Owner(msg))),
                Poll::Ready(None) => this.owner = None,
                Poll::Pending => {}
            }
        }
        Poll::Pending
    }
}

fn log(path: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: mpris_probe <result-log>");
        std::process::exit(2);
    }
    let log_path = args[1].clone();
    let (_stop_tx, stop_rx) = async_channel::unbounded::<()>();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);

    let result = async_io::block_on(async move {
        let conn = match zbus::connection::Builder::session() {
            Ok(b) => match b.build().await {
                Ok(c) => c,
                Err(e) => return format!("connect: {e}"),
            },
            Err(e) => return format!("builder: {e}"),
        };
        let dbus = match zbus::Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        {
            Ok(p) => p,
            Err(e) => return format!("proxy: {e}"),
        };
        let owner_changes = match dbus.receive_signal("NameOwnerChanged").await {
            Ok(s) => s,
            Err(e) => return format!("subscribe: {e}"),
        };
        let mut feeds = Feeds {
            stop: Box::pin(stop_rx),
            owner: Some(Box::pin(owner_changes)),
        };
        use futures_lite::StreamExt as _;
        while let Some(ev) = feeds.next().await {
            if std::time::Instant::now() > deadline {
                return "timeout: no trackable player in 25s".to_string();
            }
            if let Feed::Stop = ev {
                return "stopped".to_string();
            }
            let Feed::Owner(msg) = ev else { continue };
            let Ok((name, _old, new)) = msg.body().deserialize::<(String, String, String)>() else {
                continue;
            };
            if name.starts_with(MPRIS_PREFIX) && !new.is_empty() {
                // the player appeared: fetch Identity like fetch_player does
                let props = zbus::Proxy::new(
                    &conn,
                    name.clone(),
                    MPRIS_PATH,
                    "org.freedesktop.DBus.Properties",
                )
                .await;
                let get = match props {
                    Ok(p) => p.receive_signal("PropertiesChanged").await.is_ok(),
                    Err(_) => false,
                };
                let root_res = zbus::proxy::Builder::new(&conn)
                    .destination(name.clone())
                    .and_then(|b| b.path(MPRIS_PATH))
                    .and_then(|b| b.interface("org.mpris.MediaPlayer2"))
                    .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No));
                let root = match root_res {
                    Ok(b) => {
                        let built: Result<zbus::Proxy<'_>, zbus::Error> = b.build().await;
                        match built {
                            Ok(p) => p,
                            Err(e) => return format!("root build: {e}"),
                        }
                    }
                    Err(e) => return format!("root builder: {e}"),
                };
                let identity = match root.get_property::<String>("Identity").await {
                    Ok(v) => v,
                    Err(e) => return format!("get Identity FAILED: {e}"),
                };
                return format!("tracked: identity={identity} props-subscribed={get}");
            }
        }
        "stream ended".to_string()
    });
    log(&log_path, &result);
    eprintln!("mpris_probe: {result}");
    let ok = result.starts_with("tracked");
    std::process::exit(if ok { 0 } else { 1 });
}
