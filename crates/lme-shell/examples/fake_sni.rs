// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake `org.kde.StatusNotifierItem` + `com.canonical.dbusmenu` server
//! for the tray e2e test (tests/golden.rs::tray_sni_lifecycle).
//!
//! Usage: `fake_sni <log-file>` — connects to the session bus from
//! `DBUS_SESSION_BUS_ADDRESS` (the e2e private bus), owns
//! `org.lme.FakeSni`, serves the item at `/StatusNotifierItem` and a menu
//! at `/MenuBar`, registers with the shell's StatusNotifierWatcher
//! (retrying until the watcher appears) and appends every received call
//! (`registered`, `activate`, `secondary`, `scroll …`, `abouttoshow …`,
//! `getlayout …`, `event …`) to the log file so the test can assert the
//! shell actually talked to the item. Exits when killed (the bus name
//! loss exercises the shell's NameOwnerChanged path).

use std::io::Write as _;
use std::sync::LazyLock;
use std::time::Duration;

use zbus::zvariant::{Array, Dict, Signature, StructureBuilder, Value};

const SERVICE: &str = "org.lme.FakeSni";
const MENU_PATH: &str = "/MenuBar";

static STR_SIG: LazyLock<Signature> = LazyLock::new(|| Signature::try_from("s").expect("s sig"));
static VAR_SIG: LazyLock<Signature> = LazyLock::new(|| Signature::try_from("v").expect("v sig"));

fn log(path: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

struct Sni {
    log_path: String,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Sni {
    #[zbus(property)]
    fn id(&self) -> &str {
        "org.lme.FakeSni"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Fake Tray App"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }
    /// 16×16 opaque teal ARGB32 (channels below alpha: valid premultiply).
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        vec![(16, 16, [255u8, 32, 160, 160].repeat(16 * 16))]
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        Vec::new()
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, (i32, i32, Vec<u8>), String, String) {
        (
            String::new(),
            (0, 0, Vec::new()),
            "Fake tooltip".to_string(),
            "e2e item".to_string(),
        )
    }
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::OwnedObjectPath::try_from(MENU_PATH).expect("menu path")
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    fn activate(&self, _x: i32, _y: i32) {
        log(&self.log_path, "activate");
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        log(&self.log_path, "secondary");
    }
    fn scroll(&self, delta: i32, orientation: &str) {
        log(&self.log_path, &format!("scroll {delta} {orientation}"));
    }
}

struct Menu {
    log_path: String,
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn about_to_show(&self, id: i32) -> zbus::fdo::Result<bool> {
        log(&self.log_path, &format!("abouttoshow {id}"));
        Ok(true)
    }

    /// Reply body: `(u, layout)` — the layout Value rides as a variant
    /// member; the shell's `(u32, OwnedValue)` deserialization peels the
    /// variant wrapper (zvariant Values are signature-driven), and the
    /// node internals below match the `(ia{sv}av)` wire form exactly.
    fn get_layout(
        &self,
        parent: i32,
        _depth: i32,
        _props: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Value<'static>)> {
        log(&self.log_path, &format!("getlayout {parent}"));
        Ok((3, menu_root()))
    }

    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        log(&self.log_path, &format!("event {id} {event_id}"));
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
}

/// One `(ia{sv}av)` node with bare fields (a{sv} values variant-wrapped,
/// children as variant-wrapped structures — the wire form).
fn menu_node(
    id: i32,
    label: &'static str,
    extra: &[(&'static str, Value<'static>)],
    children: Vec<Value<'static>>,
) -> Value<'static> {
    let mut dict = Dict::new(&STR_SIG, &VAR_SIG);
    dict.append(
        Value::from("label"),
        Value::Value(Box::new(Value::from(label))),
    )
    .expect("append label");
    for (k, v) in extra {
        dict.append(Value::from(*k), Value::Value(Box::new(v.clone())))
            .expect("append prop");
    }
    let mut kids = Array::new(&VAR_SIG);
    for c in children {
        kids.append(Value::Value(Box::new(c)))
            .expect("append child");
    }
    Value::Structure(
        StructureBuilder::new()
            .append_field(Value::I32(id))
            .append_field(Value::Dict(dict))
            .append_field(Value::Array(kids))
            .build()
            .expect("node structure"),
    )
}

/// Root(0) → About(1), separator(2), Quit(3).
fn menu_root() -> Value<'static> {
    let about = menu_node(1, "_About", &[], Vec::new());
    let sep = menu_node(2, "", &[("type", Value::from("separator"))], Vec::new());
    let quit = menu_node(3, "_Quit", &[], Vec::new());
    menu_node(
        0,
        "Root",
        &[("children-display", Value::from("submenu"))],
        vec![about, sep, quit],
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: fake_sni <log-file>");
        std::process::exit(2);
    }
    let log_path = args[1].clone();

    let conn = zbus::blocking::connection::Builder::session()?
        .serve_at(
            "/StatusNotifierItem",
            Sni {
                log_path: log_path.clone(),
            },
        )?
        .serve_at(
            MENU_PATH,
            Menu {
                log_path: log_path.clone(),
            },
        )?
        .name(SERVICE)?
        .build()?;

    // Register with the shell's watcher; retry while the shell starts.
    let watcher = zbus::blocking::Proxy::new(
        &conn,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )?;
    let mut registered = false;
    for _ in 0..200 {
        match watcher.call::<_, _, ()>("RegisterStatusNotifierItem", &(SERVICE)) {
            Ok(()) => {
                registered = true;
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    if !registered {
        eprintln!("fake_sni: watcher never appeared");
        std::process::exit(3);
    }
    log(&log_path, "registered");

    // Serve until killed (NameOwnerChanged then exercises the shell path).
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
