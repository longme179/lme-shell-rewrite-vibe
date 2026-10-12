//! Shared D-Bus plumbing helpers (see docs/ZBUS_NOTES.md — read it before
//! writing new D-Bus code).
//!
//! [`no_cache_proxy`] exists because zbus 5's default proxy cache mode is
//! `CacheProperties::Lazily`: the first `get_property` spawns a background
//! GetAll cache task on the connection's internal executor. On a BLOCKING
//! connection shared by several threads (the D4/D20 pattern: manager +
//! watcher + listener threads) that background task intermittently
//! deadlocks — found as a 1-in-3 flake in the MPRIS e2e (D25): the monitor
//! showed the GetAll going out and never being processed while the peer
//! stayed alive. Raw `call()`s and `CacheProperties::No` proxies (a plain
//! `Get` per read) never deadlock. Use this helper for every proxy that
//! reads properties.

/// Build a blocking proxy with the property cache DISABLED: every
/// `get_property` becomes a plain `Get` method call, avoiding the
/// background-cache GetAll deadlock (D25 / ZBUS_NOTES pitfall 6).
pub fn no_cache_proxy<'p>(
    conn: &zbus::blocking::Connection,
    destination: &'p str,
    path: &'p str,
    interface: &'p str,
) -> Result<zbus::blocking::Proxy<'p>, String> {
    zbus::blocking::proxy::Builder::new(conn)
        .destination(destination)
        .map_err(|e| format!("destination {destination}: {e}"))?
        .path(path)
        .map_err(|e| format!("path {path}: {e}"))?
        .interface(interface)
        .map_err(|e| format!("interface {interface}: {e}"))?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .map_err(|e| format!("proxy {destination}: {e}"))
}

/// Drive the connection's internal executor forever, on our own thread.
///
/// zbus's built-in driver thread runs `while !executor.is_empty() {
/// tick() }` — it exits permanently the first time the executor drains,
/// and every later task (socket reads, method-call reply dispatch) never
/// runs again. A client-only blocking connection (no object server)
/// drains as soon as its init calls resolve, so a method call made later
/// intermittently hangs forever — found as the 1-in-2 MPRIS e2e flake
/// (D25): the Get went out on the wire, the (healthy) peer replied, and
/// the reply was never dispatched. The zbus-recommended pattern (their
/// `Connection::executor` docs: "You're responsible to continuously tick
/// the executor. Failure to do so will result in hangs") is a tick loop
/// that NEVER exits on empty — `Executor::tick` parks until a task is
/// spawned, so this thread idles at zero CPU. Call this right after
/// building every blocking connection.
pub fn drive_executor_forever(conn: &zbus::blocking::Connection) {
    let executor = conn.inner().executor().clone();
    std::thread::Builder::new()
        .name("lme-zbus-executor".into())
        .spawn(move || {
            async_io::block_on(async move {
                loop {
                    executor.tick().await;
                }
            })
        })
        .expect("spawn zbus executor driver thread");
}
