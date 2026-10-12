// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake `org.bluez` service for the control-center bluetooth e2e
//! (tests/golden.rs::control_bluetooth_tile_toggles).
//!
//! Usage: `fake_bluez <log-file>` — connects to the bus from
//! `DBUS_SESSION_BUS_ADDRESS` (the e2e private bus, which the harness also
//! exports to the daemon as DBUS_SYSTEM_BUS_ADDRESS so `busctl --system`
//! reaches this fake), owns `org.bluez` and serves an ObjectManager at `/`
//! reporting one powered adapter at /org/bluez/hci0. The daemon's probe
//! (`SystemProbes::bluetooth` — busctl GetManagedObjects) then reports
//! available + powered. Exits when killed.

use std::collections::HashMap;
use std::io::Write as _;

use zbus::zvariant::{ObjectPath, OwnedValue};

fn log(path: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

struct BluezM {
    log_path: String,
    /// Flipped when the shell sets Powered (the e2e asserts the log).
    powered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl BluezM {
    /// a{oa{sa{sv}}} — the subset of BlueZ's object tree the daemon probe
    /// reads (adapter paths + org.bluez.Adapter1 Powered/Alias).
    fn get_managed_objects(
        &self,
    ) -> HashMap<ObjectPath<'static>, HashMap<String, HashMap<String, OwnedValue>>> {
        log(&self.log_path, "get_managed_objects");
        let powered = self.powered.load(std::sync::atomic::Ordering::SeqCst);
        let mut adapter1: HashMap<String, OwnedValue> = HashMap::new();
        adapter1.insert(
            "Powered".into(),
            OwnedValue::try_from(zbus::zvariant::Value::Bool(powered)).expect("owned"),
        );
        adapter1.insert(
            "Alias".into(),
            OwnedValue::try_from(zbus::zvariant::Value::Str("Golden Buddy".into())).expect("owned"),
        );
        let mut interfaces: HashMap<String, HashMap<String, OwnedValue>> = HashMap::new();
        interfaces.insert("org.bluez.Adapter1".to_string(), adapter1);
        let mut objects: HashMap<
            ObjectPath<'static>,
            HashMap<String, HashMap<String, OwnedValue>>,
        > = HashMap::new();
        objects.insert(
            ObjectPath::try_from("/org/bluez/hci0").expect("valid path"),
            interfaces,
        );
        objects
    }
}

/// `org.bluez.Adapter1` property setter so the real (non-dry-run) toggle
/// path has something to talk to; the fake records every set.
struct Adapter {
    log_path: String,
    powered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[zbus(property)]
    fn set_powered(&self, value: bool) {
        self.powered
            .store(value, std::sync::atomic::Ordering::SeqCst);
        log(&self.log_path, &format!("set_powered {value}"));
    }

    fn alias(&self) -> &str {
        "Golden Buddy"
    }
}

fn main() {
    let log_path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: fake_bluez <log-file>");
        std::process::exit(2);
    });
    log(&log_path, "starting");
    let powered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let conn = zbus::blocking::Connection::session()
        .expect("session bus (DBUS_SESSION_BUS_ADDRESS from the e2e)");
    conn.object_server()
        .at(
            "/",
            BluezM {
                log_path: log_path.clone(),
                powered: std::sync::Arc::clone(&powered),
            },
        )
        .expect("serve ObjectManager at /");
    conn.object_server()
        .at(
            "/org/bluez/hci0",
            Adapter {
                log_path: log_path.clone(),
                powered: std::sync::Arc::clone(&powered),
            },
        )
        .expect("serve Adapter1");
    conn.request_name("org.bluez")
        .expect("own org.bluez on the private bus");
    log(&log_path, "ready");
    // serve until killed
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
