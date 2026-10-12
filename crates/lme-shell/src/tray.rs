// SPDX-License-Identifier: GPL-3.0-or-later
//! System tray (Tier 1.11): a `org.kde.StatusNotifierWatcher` host, live
//! `org.kde.StatusNotifierItem` tracking and a `com.canonical.dbusmenu`
//! client, plus the bar tray panel (port of legacy qml/panels/TrayMenu.qml:
//! 268px list of items, left-click activate, right-click menu, wheel
//! scrolls the item, "No tray applications" empty state).
//!
//! Threading (ADR 0002 / D4, the notifications.rs pattern): the D-Bus
//! object server runs on its own blocking connection; interface calls and
//! per-item signal listeners forward `ManagerMsg`s to a manager thread that
//! owns the proxies, and the manager forwards `TrayEvent`s to the daemon's
//! calloop loop. The daemon sends `TrayCommand`s back through the same
//! manager. A missing session bus is non-fatal — the bar hides the tray
//! button (items == 0, exactly like legacy `SystemTray.items.length > 0`).
//!
//! Deltas vs legacy (documented in README + DECISIONS D22): the DBusMenu
//! renders as an in-panel page (flat, indented submenus) instead of a
//! floating platform menu; the panel anchors at the right edge (legacy
//! computes trayAnchorX from the bar button, we lack that layout info).

use crate::daemon::Shell;
use lme_ui::color::Color;
use lme_ui::font_weight::NORMAL;
use lme_ui::geom::{Point, Rect, Size};
use lme_ui::icons::IconCache;
use lme_ui::wayland::{KeyboardInteractivity, UiState, WindowSpec};
use lme_ui::widgets::{Ctx, InputEvent, PointerButton, UiEvent, Widget};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const WATCHER_IFACE: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const SNI_IFACE: &str = "org.kde.StatusNotifierItem";
/// Path items live at when they register a bus NAME (the common form).
const SNI_DEFAULT_PATH: &str = "/StatusNotifierItem";
const DBUSMENU_IFACE: &str = "com.canonical.dbusmenu";
/// Right-edge margin of non-centered legacy panels (shell.qml x fallback).
pub const PANEL_RIGHT_MARGIN: f32 = 14.0;
/// Tray list width (legacy TrayMenu implicitWidth).
pub const PANEL_WIDTH: f32 = 268.0;
/// Tray panel max height (legacy Math.min(content + 24, 360)).
pub const PANEL_MAX_H: f32 = 360.0;
/// Row height (legacy Layout.preferredHeight 34).
pub const ROW_H: f32 = 34.0;

// ---------------------------------------------------------------------------
// Item model
// ---------------------------------------------------------------------------

/// One StatusNotifierItem snapshot. Rebuilt wholesale on any New* signal
/// (items are tiny; partial patching is not worth the state machine).
#[derive(Clone)]
pub struct TrayItem {
    /// Watcher-list encoding: bus name, or "bus/path" for the path form.
    pub key: String,
    pub bus: String,
    pub path: String,
    pub id: String,
    pub title: String,
    pub category: String,
    /// Active | Passive | NeedsAttention
    pub status: String,
    pub icon_name: String,
    /// Largest IconPixmap, decoded ARGB32 → RGBA.
    pub icon_pixmap: Option<Arc<tiny_skia::Pixmap>>,
    pub attention_icon_name: String,
    pub attention_pixmap: Option<Arc<tiny_skia::Pixmap>>,
    pub tooltip_title: String,
    pub tooltip_desc: String,
    /// ItemIsMenu: primary click should open the menu, not Activate.
    pub only_menu: bool,
    /// Menu object path (com.canonical.dbusmenu), if the item has one.
    pub menu_path: Option<String>,
}

impl TrayItem {
    /// Legacy row title: tooltipTitle || title || id || "Tray application".
    pub fn display_title(&self) -> &str {
        let first = if self.tooltip_title.is_empty() {
            &self.title
        } else {
            &self.tooltip_title
        };
        if !first.is_empty() {
            return first;
        }
        if !self.id.is_empty() {
            return &self.id;
        }
        "Tray application"
    }

    /// Attention icon wins while the item asks for attention (Quickshell
    /// StatusNotifier behavior).
    pub fn display_icon(&self) -> Option<Arc<tiny_skia::Pixmap>> {
        if self.status == "NeedsAttention" {
            self.attention_pixmap
                .clone()
                .or_else(|| self.icon_pixmap.clone())
        } else {
            self.icon_pixmap.clone()
        }
    }
}

/// Shell-side registry (registration order, like the legacy Repeater).
#[derive(Default)]
pub struct TrayHub {
    pub items: Vec<TrayItem>,
}

impl TrayHub {
    /// Insert or replace by key; true when the item count changed.
    pub fn upsert(&mut self, item: TrayItem) -> bool {
        match self.items.iter().position(|i| i.key == item.key) {
            Some(i) => {
                self.items[i] = item;
                false
            }
            None => {
                self.items.push(item);
                true
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<TrayItem> {
        let idx = self.items.iter().position(|i| i.key == key)?;
        Some(self.items.remove(idx))
    }

    pub fn get(&self, key: &str) -> Option<&TrayItem> {
        self.items.iter().find(|i| i.key == key)
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.items.len(),
            "items": self.items.iter().map(|i| serde_json::json!({
                "key": i.key,
                "bus": i.bus,
                "path": i.path,
                "id": i.id,
                "title": i.title,
                "displayTitle": i.display_title(),
                "tooltipDesc": i.tooltip_desc,
                "status": i.status,
                "category": i.category,
                "hasMenu": i.menu_path.is_some(),
                "onlyMenu": i.only_menu,
                "iconName": i.icon_name,
                "attentionIconName": i.attention_icon_name,
            })).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------------------
// DBusMenu model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleKind {
    Check,
    Radio,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuNode {
    pub id: i32,
    /// Mnemonic underscores stripped.
    pub label: String,
    pub separator: bool,
    pub enabled: bool,
    pub visible: bool,
    /// (kind, checked)
    pub toggle: Option<(ToggleKind, bool)>,
    pub has_children: bool,
    pub children: Vec<MenuNode>,
}

/// One rendered row (submenus flattened, invisible entries dropped).
#[derive(Debug, Clone)]
pub struct FlatEntry {
    pub node_id: i32,
    pub label: String,
    pub depth: u32,
    pub enabled: bool,
    pub separator: bool,
    pub toggle: Option<(ToggleKind, bool)>,
    pub has_children: bool,
}

pub fn flatten_menu(nodes: &[MenuNode], depth: u32, out: &mut Vec<FlatEntry>) {
    for n in nodes {
        if !n.visible {
            continue;
        }
        out.push(FlatEntry {
            node_id: n.id,
            label: n.label.clone(),
            depth,
            enabled: n.enabled,
            separator: n.separator,
            toggle: n.toggle,
            has_children: n.has_children,
        });
        if !n.children.is_empty() {
            flatten_menu(&n.children, depth + 1, out);
        }
    }
}

/// Strip DBusMenu mnemonic underscores ("_Quit" → "Quit").
pub fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '_' {
            // doubled underscores are escaped
            if let Some('_') = chars.clone().next() {
                chars.next();
                out.push('_');
                continue;
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Parse one `(ia{sv}av)` layout item from a Value tree. Children elements
/// may arrive wrapped in a Variant (the wire form) or as bare structures
/// (hand-built test trees) — both are accepted.
fn parse_menu_node(v: &zbus::zvariant::Value<'_>) -> Option<MenuNode> {
    match v {
        zbus::zvariant::Value::Structure(s) => {
            let fields = s.fields();
            if fields.len() != 3 {
                return None;
            }
            let id = i32::try_from(&fields[0]).ok()?;
            let props = match &fields[1] {
                zbus::zvariant::Value::Dict(d) => dict_map(d),
                _ => return None,
            };
            let children = match &fields[2] {
                zbus::zvariant::Value::Array(a) => a
                    .inner()
                    .iter()
                    .filter_map(parse_menu_node)
                    .collect::<Vec<_>>(),
                _ => return None,
            };
            let entry_type = prop_str(&props, "type").unwrap_or_else(|| "standard".to_string());
            let toggle = match prop_str(&props, "toggle-type").as_deref() {
                Some("checkmark") => {
                    Some((ToggleKind::Check, prop_i32(&props, "toggle-state") == 1))
                }
                Some("radio") => Some((ToggleKind::Radio, prop_i32(&props, "toggle-state") == 1)),
                _ => None,
            };
            Some(MenuNode {
                id,
                label: strip_mnemonic(&prop_str(&props, "label").unwrap_or_default()),
                separator: entry_type == "separator",
                enabled: prop_bool(&props, "enabled").unwrap_or(true),
                visible: prop_bool(&props, "visible").unwrap_or(true),
                toggle,
                has_children: prop_str(&props, "children-display").as_deref() == Some("submenu"),
                children,
            })
        }
        zbus::zvariant::Value::Value(inner) => parse_menu_node(inner),
        _ => None,
    }
}

fn dict_map<'v>(d: &zbus::zvariant::Dict<'_, 'v>) -> HashMap<String, zbus::zvariant::Value<'v>> {
    let mut out = HashMap::new();
    for (k, v) in d.iter() {
        if let zbus::zvariant::Value::Str(s) = k {
            // a{sv} wire form: values arrive wrapped in a variant — unwrap
            // so prop_str/prop_i32 see the inner type directly.
            let v = match v {
                zbus::zvariant::Value::Value(inner) => (**inner).try_clone(),
                other => other.try_clone(),
            }
            .unwrap_or(zbus::zvariant::Value::U32(0));
            out.insert(s.to_string(), v);
        }
    }
    out
}

fn prop_str(m: &HashMap<String, zbus::zvariant::Value<'_>>, k: &str) -> Option<String> {
    match m.get(k)? {
        zbus::zvariant::Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn prop_bool(m: &HashMap<String, zbus::zvariant::Value<'_>>, k: &str) -> Option<bool> {
    bool::try_from(m.get(k)?).ok()
}

fn prop_i32(m: &HashMap<String, zbus::zvariant::Value<'_>>, k: &str) -> i32 {
    m.get(k).and_then(|v| i32::try_from(v).ok()).unwrap_or(0)
}

/// Parse the GetLayout reply body (`(u(ia{sv}av))` → layout tree).
pub fn parse_menu_layout(reply: &zbus::zvariant::Value<'_>) -> Option<MenuNode> {
    match reply {
        zbus::zvariant::Value::Structure(s) => {
            let fields = s.fields();
            if fields.len() != 2 {
                return None;
            }
            parse_menu_node(&fields[1])
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Pixmap decode
// ---------------------------------------------------------------------------

/// SNI IconPixmap entries are (w, h, ARGB32 big-endian, NOT premultiplied).
/// tiny-skia wants premultiplied RGBA; `PremultipliedColorU8::from_rgba`
/// does that conversion per pixel.
pub fn decode_argb_pixmap(w: i32, h: i32, data: &[u8]) -> Option<Arc<tiny_skia::Pixmap>> {
    if w <= 0 || h <= 0 || w > 512 || h > 512 {
        return None;
    }
    let (w, h) = (w as u32, h as u32);
    if data.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    let pixels = pm.pixels_mut();
    for (dst, px) in pixels.iter_mut().zip(data.as_chunks::<4>().0) {
        // ARGB32 network order: A R G B. Some apps ship non-premultiplied
        // data with a channel above alpha (invalid); clamp instead of
        // dropping the whole icon (Quickshell tolerates it too).
        let (r, g, b, a) = (px[1], px[2], px[3], px[0]);
        *dst = tiny_skia::PremultipliedColorU8::from_rgba(r.min(a), g.min(a), b.min(a), a)?;
    }
    Some(Arc::new(pm))
}

/// SNI pixmap list wire type: (width, height, ARGB32 network-order bytes).
type PixmapList = Vec<(i32, i32, Vec<u8>)>;

/// SNI ToolTip wire type: (iconName, (w, h, ARGB32 bytes), title, description).
type SniToolTip = (String, (i32, i32, Vec<u8>), String, String);

/// Pick the largest pixmap in a (iiay) list (best quality when scaled).
pub fn best_pixmap(entries: &PixmapList) -> Option<Arc<tiny_skia::Pixmap>> {
    entries
        .iter()
        .filter(|(w, h, _)| *w > 0 && *h > 0)
        .max_by_key(|(w, h, _)| w * h)
        .and_then(|(w, h, data)| decode_argb_pixmap(*w, *h, data))
}

// ---------------------------------------------------------------------------
// Events / commands
// ---------------------------------------------------------------------------

/// Manager → daemon (calloop channel).
pub enum TrayEvent {
    Registered(Box<TrayItem>),
    Updated(Box<TrayItem>),
    Unregistered {
        key: String,
    },
    /// GetLayout reply for an OpenMenu command.
    MenuReady {
        key: String,
        nodes: Vec<MenuNode>,
    },
    MenuFailed {
        key: String,
        error: String,
    },
}

/// Daemon → manager (mpsc, forwarded to the bus thread).
pub enum TrayCommand {
    Activate {
        key: String,
    },
    SecondaryActivate {
        key: String,
    },
    /// delta in SNI units (one wheel notch = ±120, like angleDelta.y).
    Scroll {
        key: String,
        delta: i32,
    },
    OpenMenu {
        key: String,
    },
    /// Clicked menu entry node id.
    MenuEvent {
        key: String,
        id: i32,
    },
}

/// Everything the manager thread consumes.
enum ManagerMsg {
    Register {
        sender: String,
        service: String,
    },
    FromShell(TrayCommand),
    /// Any signal on an item's object (New* / PropertiesChanged / …).
    ItemSignal {
        key: String,
    },
    /// NameOwnerChanged: an item's bus name lost its owner.
    OwnerLost {
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Watcher interface (object server)
// ---------------------------------------------------------------------------

struct WatcherIface {
    tx: std::sync::mpsc::Sender<ManagerMsg>,
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl WatcherIface {
    /// `service` is either the item's bus NAME (item at
    /// /StatusNotifierItem — the common form) or an object PATH on the
    /// caller (the legacy ayatana form, item on the caller's unique name).
    fn register_status_notifier_item(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        service: &str,
    ) -> zbus::fdo::Result<()> {
        let Some(sender) = hdr.sender() else {
            return Err(zbus::fdo::Error::Failed("anonymous sender".into()));
        };
        self.tx
            .send(ManagerMsg::Register {
                sender: sender.to_string(),
                service: service.to_string(),
            })
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// We are the (sole) host; registering another host is a no-op.
    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().expect("tray items mutex").clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// Handle to the running watcher. Dropping it stops the manager thread
/// (its channel closes), which drops the connection and releases the bus
/// name.
#[derive(Clone)]
pub struct TrayHandle {
    tx: Arc<std::sync::mpsc::Sender<ManagerMsg>>,
}

impl TrayHandle {
    pub fn send(&self, cmd: TrayCommand) {
        let _ = self.tx.send(ManagerMsg::FromShell(cmd));
    }
}

/// Item bookkeeping on the manager thread.
struct ItemRecord {
    bus: String,
    path: String,
    menu_path: Option<String>,
    /// Stops the per-item listener when the item goes away.
    alive: Arc<AtomicBool>,
}

/// Start the watcher. Fails softly (no session bus / name taken) exactly
/// like notifications::spawn.
pub fn spawn(tx: calloop::channel::Sender<TrayEvent>) -> Result<TrayHandle, String> {
    let (mgr_tx, mgr_rx) = std::sync::mpsc::channel::<ManagerMsg>();
    let items_list = Arc::new(Mutex::new(Vec::<String>::new()));
    let conn = zbus::blocking::connection::Builder::session()
        .map_err(|e| format!("session bus: {e}"))?
        .serve_at(
            WATCHER_PATH,
            WatcherIface {
                tx: mgr_tx.clone(),
                items: Arc::clone(&items_list),
            },
        )
        .map_err(|e| format!("serve: {e}"))?
        .name(WATCHER_IFACE)
        .map_err(|e| format!("name {WATCHER_IFACE}: {e}"))?
        .build()
        .map_err(|e| format!("connect: {e}"))?;

    let mgr_tx = Arc::new(mgr_tx);
    let handle_tx = Arc::clone(&mgr_tx);
    std::thread::spawn(move || {
        let mut records: HashMap<String, ItemRecord> = HashMap::new();
        // Watch NameOwnerChanged for item apps that quit without
        // unregistering (the common case: crash / SIGKILL).
        spawn_owner_watcher(&conn, &mgr_tx);
        for msg in mgr_rx {
            match msg {
                ManagerMsg::Register { sender, service } => {
                    register_item(
                        &conn,
                        &mgr_tx,
                        &mut records,
                        &items_list,
                        &tx,
                        sender,
                        service,
                    );
                }
                ManagerMsg::FromShell(cmd) => {
                    run_command(&conn, &records, &tx, cmd);
                }
                ManagerMsg::ItemSignal { key } => {
                    if let Some(rec) = records.get(&key) {
                        match fetch_item(&conn, &rec.bus, &rec.path) {
                            Ok(item) => {
                                let _ = tx.send(TrayEvent::Updated(Box::new(item)));
                            }
                            Err(e) => eprintln!("lme-shell: tray refresh {key}: {e}"),
                        }
                    }
                }
                ManagerMsg::OwnerLost { name } => {
                    // drop every item registered on that bus name
                    let lost: Vec<String> = records
                        .iter()
                        .filter(|(_, r)| r.bus == name)
                        .map(|(k, _)| k.clone())
                        .collect();
                    for key in lost {
                        remove_item(&conn, &mut records, &items_list, &tx, &key);
                    }
                }
            }
        }
        // mgr_rx closed: daemon is shutting down. Flag listeners and let
        // the connection drop with this thread.
        for r in records.values() {
            r.alive.store(false, Ordering::Relaxed);
        }
    });
    Ok(TrayHandle { tx: handle_tx })
}

#[allow(clippy::too_many_arguments)]
fn register_item(
    conn: &zbus::blocking::Connection,
    mgr_tx: &Arc<std::sync::mpsc::Sender<ManagerMsg>>,
    records: &mut HashMap<String, ItemRecord>,
    items_list: &Arc<Mutex<Vec<String>>>,
    tx: &calloop::channel::Sender<TrayEvent>,
    sender: String,
    service: String,
) {
    let (bus, path) = if service.starts_with('/') {
        (sender, service)
    } else {
        (service, SNI_DEFAULT_PATH.to_string())
    };
    let key = if path == SNI_DEFAULT_PATH {
        bus.clone()
    } else {
        format!("{bus}{path}")
    };
    match fetch_item(conn, &bus, &path) {
        Ok(mut item) => {
            item.key = key.clone();
            let rec = ItemRecord {
                bus: bus.clone(),
                path: path.clone(),
                menu_path: item.menu_path.clone(),
                alive: Arc::new(AtomicBool::new(true)),
            };
            spawn_item_listener(conn, mgr_tx, &key, &rec);
            records.insert(key.clone(), rec);
            {
                let mut list = items_list.lock().expect("tray items mutex");
                if !list.contains(&key) {
                    list.push(key.clone());
                }
            }
            let _ = conn.emit_signal(
                None::<&str>,
                WATCHER_PATH,
                WATCHER_IFACE,
                "StatusNotifierItemRegistered",
                &key,
            );
            let _ = tx.send(TrayEvent::Registered(Box::new(item)));
        }
        Err(e) => eprintln!("lme-shell: tray register {key}: {e}"),
    }
}

fn remove_item(
    conn: &zbus::blocking::Connection,
    records: &mut HashMap<String, ItemRecord>,
    items_list: &Arc<Mutex<Vec<String>>>,
    tx: &calloop::channel::Sender<TrayEvent>,
    key: &str,
) {
    let Some(rec) = records.remove(key) else {
        return;
    };
    rec.alive.store(false, Ordering::Relaxed);
    items_list
        .lock()
        .expect("tray items mutex")
        .retain(|k| k != key);
    let _ = conn.emit_signal(
        None::<&str>,
        WATCHER_PATH,
        WATCHER_IFACE,
        "StatusNotifierItemUnregistered",
        &key.to_string(),
    );
    let _ = tx.send(TrayEvent::Unregistered {
        key: key.to_string(),
    });
}

/// One listener thread per item: all signals on the item's object (SNI
/// New*, org.freedesktop.DBus.Properties.PropertiesChanged — and even
/// dbusmenu LayoutUpdated for apps that host the menu on the item path).
fn spawn_item_listener(
    conn: &zbus::blocking::Connection,
    mgr_tx: &Arc<std::sync::mpsc::Sender<ManagerMsg>>,
    key: &str,
    rec: &ItemRecord,
) {
    let Ok(proxy) =
        zbus::blocking::Proxy::new(conn, rec.bus.as_str(), rec.path.as_str(), SNI_IFACE)
    else {
        return;
    };
    let Ok(signals) = proxy.receive_all_signals() else {
        return;
    };
    let key = key.to_string();
    let alive = Arc::clone(&rec.alive);
    let tx = Arc::clone(mgr_tx);
    std::thread::spawn(move || {
        for msg in signals {
            if !alive.load(Ordering::Relaxed) {
                break;
            }
            let refresh = matches!(
                msg.header().member().map(|m| m.as_str()),
                Some(
                    "NewTitle"
                        | "NewIcon"
                        | "NewAttentionIcon"
                        | "NewOverlayIcon"
                        | "NewToolTip"
                        | "NewStatus"
                        | "PropertiesChanged"
                        | "LayoutUpdated"
                )
            );
            if refresh
                && tx
                    .send(ManagerMsg::ItemSignal { key: key.clone() })
                    .is_err()
            {
                break;
            }
        }
    });
}

/// One thread watching NameOwnerChanged (old owner gone → item list check).
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
            if !old.is_empty() && new.is_empty() && tx.send(ManagerMsg::OwnerLost { name }).is_err()
            {
                break;
            }
        }
    });
}

/// Read every property we care about into a snapshot. Missing properties
/// fall back to spec defaults.
fn fetch_item(
    conn: &zbus::blocking::Connection,
    bus: &str,
    path: &str,
) -> Result<TrayItem, String> {
    let proxy = zbus::blocking::Proxy::new(conn, bus, path, SNI_IFACE)
        .map_err(|e| format!("proxy: {e}"))?;
    let str_prop = |name: &str, default: &str| -> String {
        proxy
            .get_property::<String>(name)
            .unwrap_or_else(|_| default.to_string())
    };
    let pixmaps: PixmapList = proxy.get_property("IconPixmap").unwrap_or_default();
    let attention_pixmaps: PixmapList = proxy
        .get_property("AttentionIconPixmap")
        .unwrap_or_default();
    let tooltip: Option<SniToolTip> = proxy.get_property("ToolTip").ok();
    let menu: Option<zbus::zvariant::OwnedObjectPath> = proxy.get_property("Menu").ok();
    let menu_path = menu
        .filter(|p| p.as_str() != "/" && !p.as_str().is_empty())
        .map(|p| p.to_string());
    let (tooltip_title, tooltip_desc) = tooltip
        .map(|(_, _, title, desc)| (title, desc))
        .unwrap_or_default();
    Ok(TrayItem {
        key: String::new(), // filled by the caller
        bus: bus.to_string(),
        path: path.to_string(),
        id: str_prop("Id", ""),
        title: str_prop("Title", ""),
        category: str_prop("Category", "ApplicationStatus"),
        status: str_prop("Status", "Active"),
        icon_name: str_prop("IconName", ""),
        icon_pixmap: best_pixmap(&pixmaps),
        attention_icon_name: str_prop("AttentionIconName", ""),
        attention_pixmap: best_pixmap(&attention_pixmaps),
        tooltip_title,
        tooltip_desc,
        only_menu: proxy.get_property("ItemIsMenu").unwrap_or(false),
        menu_path,
    })
}

/// Daemon commands → proxy calls on the manager thread.
fn run_command(
    conn: &zbus::blocking::Connection,
    records: &HashMap<String, ItemRecord>,
    tx: &calloop::channel::Sender<TrayEvent>,
    cmd: TrayCommand,
) {
    let key = key_of(&cmd);
    let Some(rec) = records.get(&key) else {
        return;
    };
    let proxy = zbus::blocking::Proxy::new(conn, rec.bus.as_str(), rec.path.as_str(), SNI_IFACE);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0);
    match cmd {
        TrayCommand::Activate { .. } => {
            if let Ok(p) = &proxy {
                let _: zbus::Result<()> = p.call("Activate", &(0i32, 0i32));
            }
        }
        TrayCommand::SecondaryActivate { .. } => {
            if let Ok(p) = &proxy {
                let _: zbus::Result<()> = p.call("SecondaryActivate", &(0i32, 0i32));
            }
        }
        TrayCommand::Scroll { delta, .. } => {
            if let Ok(p) = &proxy {
                let _: zbus::Result<()> = p.call("Scroll", &(delta, "vertical"));
            }
        }
        TrayCommand::OpenMenu { .. } => {
            let Some(menu_path) = &rec.menu_path else {
                let _ = tx.send(TrayEvent::MenuFailed {
                    key,
                    error: "no menu".into(),
                });
                return;
            };
            match fetch_menu(conn, &rec.bus, menu_path) {
                Ok(nodes) => {
                    let _ = tx.send(TrayEvent::MenuReady { key, nodes });
                }
                Err(e) => {
                    let _ = tx.send(TrayEvent::MenuFailed { key, error: e });
                }
            }
        }
        TrayCommand::MenuEvent { id, .. } => {
            if let Some(menu_path) = &rec.menu_path {
                if let Ok(p) = zbus::blocking::Proxy::new(
                    conn,
                    rec.bus.as_str(),
                    menu_path.as_str(),
                    DBUSMENU_IFACE,
                ) {
                    let _: zbus::Result<()> = p.call(
                        "Event",
                        &(id, "clicked", zbus::zvariant::Value::I32(0), now_ms),
                    );
                }
            }
        }
    }
}

fn key_of(cmd: &TrayCommand) -> String {
    match cmd {
        TrayCommand::Activate { key }
        | TrayCommand::SecondaryActivate { key }
        | TrayCommand::Scroll { key, .. }
        | TrayCommand::OpenMenu { key }
        | TrayCommand::MenuEvent { key, .. } => key.clone(),
    }
}

/// GetLayout via AboutToShow + GetLayout (some apps only fill the layout
/// after AboutToShow).
fn fetch_menu(
    conn: &zbus::blocking::Connection,
    bus: &str,
    menu_path: &str,
) -> Result<Vec<MenuNode>, String> {
    let proxy = zbus::blocking::Proxy::new(conn, bus, menu_path, DBUSMENU_IFACE)
        .map_err(|e| format!("menu proxy: {e}"))?;
    let _: zbus::Result<bool> = proxy.call("AboutToShow", &0i32);
    let props = vec![
        "label",
        "enabled",
        "visible",
        "type",
        "toggle-type",
        "toggle-state",
        "children-display",
    ];
    let reply: zbus::Result<(u32, zbus::zvariant::OwnedValue)> =
        proxy.call("GetLayout", &(0i32, -1i32, props));
    match reply {
        Ok((_, layout)) => parse_menu_layout(&layout)
            .map(|root| root.children)
            .ok_or_else(|| "unparsable layout".to_string()),
        Err(e) => Err(format!("GetLayout: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

pub mod ids {
    /// Primary-click row i (activate, or menu when only_menu).
    pub const TRAY_ROW_ACT: u64 = 0x7100_0000_0000;
    /// Secondary-click row i (context menu).
    pub const TRAY_ROW_MENU: u64 = 0x7150_0000_0000;
    /// Middle-click row i (SecondaryActivate).
    pub const TRAY_ROW_AUX: u64 = 0x7160_0000_0000;
    /// Wheel over row i.
    pub const TRAY_ROW_WHEEL: u64 = 0x7170_0000_0000;
    pub const TRAY_MENU_BACK: u64 = 0x7200_0000_0000;
    /// Flat menu entry i.
    pub const TRAY_MENU_ENTRY: u64 = 0x7300_0000_0000;
}

/// Full-overlay wrapper anchoring the tray panel top-right (legacy
/// shell.qml: x = overlay.width - width - 14, y = panelTopMargin; the tray
/// backdrop is fully transparent, unlike the launcher's 0.40 dim).
pub struct TrayPopup {
    panel: Box<dyn Widget>,
    w: f32,
    h: f32,
    reveal_track: u64,
    rect: Rect,
}

impl TrayPopup {
    pub fn new(panel: Box<dyn Widget>, w: f32, h: f32, reveal_track: u64) -> Self {
        Self {
            panel,
            w,
            h,
            reveal_track,
            rect: Rect::default(),
        }
    }
    pub fn panel_mut(&mut self) -> &mut Box<dyn Widget> {
        &mut self.panel
    }
}

impl Widget for TrayPopup {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(self.w, self.h)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        let x = (bounds.x + bounds.w - self.w - PANEL_RIGHT_MARGIN).max(bounds.x + 8.0);
        let y = bounds.y + ctx.theme.panel_top_margin;
        self.panel.layout(
            ctx,
            Rect::new(x, y, self.w.min(bounds.w), self.h.min(bounds.h)),
        );
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        let reveal = ctx.anims.value(self.reveal_track).clamp(0.0, 1.0);
        if reveal <= 0.0 {
            return;
        }
        let mut panel_cmds = Vec::new();
        self.panel.paint(ctx, &mut panel_cmds);
        // legacy slides the tray panel DOWN from the bar
        let dy = (1.0 - reveal) * -14.0;
        let alpha = reveal;
        for mut cmd in panel_cmds {
            match &mut cmd {
                lme_ui::draw::DrawCmd::RoundedRect { rect, fill, .. } => {
                    rect.y += dy;
                    if let Some(c) = fill {
                        *c = c.alpha(c.a * alpha);
                    }
                }
                lme_ui::draw::DrawCmd::Text { pos, .. } => {
                    pos.y += dy;
                }
                lme_ui::draw::DrawCmd::Circle { cy, .. } => {
                    *cy += dy;
                }
                lme_ui::draw::DrawCmd::Shadow { rect, .. } => {
                    rect.y += dy;
                }
                lme_ui::draw::DrawCmd::Image { rect, .. } => {
                    rect.y += dy;
                }
            }
            out.push(cmd);
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        // children are laid out in SURFACE coordinates (see CenteredPanel's
        // coordinate note — the regression test there covers this class of
        // bug; the tray panel sits at a non-zero offset by construction).
        if let InputEvent::Down { pos: p, .. } = ev {
            if !self.panel.rect().contains(*p) {
                // the legacy overlay window closes on any outside click
                ctx.queue.push(UiEvent::Dismiss);
                return true;
            }
        }
        self.panel.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.panel.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// One resolved row (icon looked up once per item change, not per frame).
pub struct TrayRow {
    pub item: TrayItem,
    pub icon: Option<Arc<tiny_skia::Pixmap>>,
}

/// The tray list page (legacy TrayMenu.qml).
pub struct TrayPanel {
    pub rows: Vec<TrayRow>,
    icons: IconCache,
    hovered: Option<usize>,
    scroll: f32,
    rect: Rect,
}

impl TrayPanel {
    pub fn new(items: &[TrayItem]) -> Self {
        let mut p = Self {
            rows: Vec::new(),
            icons: IconCache::new(),
            hovered: None,
            scroll: 0.0,
            rect: Rect::default(),
        };
        p.set_items(items);
        p
    }

    /// Refresh the item list (theme-icon lookup happens here, once).
    pub fn set_items(&mut self, items: &[TrayItem]) {
        self.rows = items
            .iter()
            .map(|item| TrayRow {
                item: item.clone(),
                icon: item.display_icon().or_else(|| {
                    // No pixmap: fall back to a theme icon, preferring the
                    // attention name while the item asks for attention.
                    let name = if item.status == "NeedsAttention"
                        && !item.attention_icon_name.is_empty()
                    {
                        &item.attention_icon_name
                    } else {
                        &item.icon_name
                    };
                    self.icons.get(name)
                }),
            })
            .collect();
        if let Some(h) = self.hovered {
            if h >= self.rows.len() {
                self.hovered = None;
            }
        }
        self.clamp_scroll();
    }

    /// Panel height: min(content + 24, 360) — legacy implicitHeight.
    pub fn height_for(items: usize) -> f32 {
        let content = if items == 0 {
            16.0 + 4.0 + ROW_H
        } else {
            16.0 + 4.0 + items as f32 * ROW_H
        };
        (content + 24.0).min(PANEL_MAX_H)
    }

    fn list_area(&self) -> Rect {
        Rect::new(
            self.rect.x + 12.0,
            self.rect.y + 12.0 + 20.0 + 4.0,
            self.rect.w - 24.0,
            (self.rect.h - 24.0 - 24.0).max(0.0),
        )
    }

    fn max_scroll(&self) -> f32 {
        (self.rows.len() as f32 * ROW_H - self.list_area().h).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// Row index at surface point p (accounts for scroll).
    fn row_at(&self, p: Point) -> Option<usize> {
        let area = self.list_area();
        if !area.contains(p) {
            return None;
        }
        let idx = ((p.y - area.y + self.scroll) / ROW_H).floor() as usize;
        (idx < self.rows.len()).then_some(idx)
    }
}

impl Widget for TrayPanel {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(PANEL_WIDTH, Self::height_for(self.rows.len()))
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        use lme_ui::draw::DrawCmd;
        let t = ctx.theme;
        // glass panel card
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: t.small_radius,
            fill: Some(t.panel_fill.alpha(t.panel_opacity)),
            border: Some((t.outline_variant, 1.0)),
            clip: None,
        });
        // header
        out.push(DrawCmd::Text {
            pos: Point::new(self.rect.x + 12.0, self.rect.y + 12.0),
            line: ctx
                .fonts
                .line_truncated("System tray", 12.0, NORMAL, self.rect.w - 24.0),
            color: t.muted,
            clip: Some(self.rect),
        });
        let area = self.list_area();
        if self.rows.is_empty() {
            out.push(DrawCmd::Text {
                pos: Point::new(area.x, area.y + (ROW_H - 16.0) / 2.0),
                line: ctx
                    .fonts
                    .line_truncated("No tray applications", 12.0, NORMAL, area.w),
                color: t.muted,
                clip: Some(area),
            });
            return;
        }
        let visible = ((area.h / ROW_H).ceil() as usize).max(1);
        let first = (self.scroll / ROW_H).floor() as usize;
        for i in 0..visible {
            let index = first + i;
            let Some(row) = self.rows.get(index) else {
                break;
            };
            let y = area.y + i as f32 * ROW_H - (self.scroll - first as f32 * ROW_H);
            let row_rect = Rect::new(area.x, y, area.w, ROW_H - 2.0);
            if Some(index) == self.hovered {
                out.push(DrawCmd::RoundedRect {
                    rect: row_rect,
                    radius: 8.0,
                    fill: Some(t.accent.alpha(0.18)),
                    border: None,
                    clip: Some(area),
                });
            }
            // icon: pixmap → theme icon → first-letter placeholder
            let icon_rect = Rect::new(area.x + 9.0, y + (ROW_H - 2.0 - 18.0) / 2.0, 18.0, 18.0);
            if let Some(pm) = &row.icon {
                out.push(DrawCmd::Image {
                    rect: icon_rect,
                    radius: 3.0,
                    image: Arc::clone(pm),
                    clip: Some(area),
                });
            } else {
                let ch = row.item.display_title().chars().next().unwrap_or('?');
                out.push(DrawCmd::Circle {
                    cx: icon_rect.x + 9.0,
                    cy: icon_rect.y + 9.0,
                    r: 9.0,
                    fill: t.surface_container,
                    clip: Some(area),
                });
                out.push(DrawCmd::Text {
                    pos: Point::new(icon_rect.x + 4.0, icon_rect.y + 3.0),
                    line: ctx
                        .fonts
                        .line_truncated(&ch.to_string(), 11.0, NORMAL, 12.0),
                    color: t.muted,
                    clip: Some(area),
                });
            }
            out.push(DrawCmd::Text {
                pos: Point::new(area.x + 9.0 + 18.0 + 10.0, y + (ROW_H - 2.0 - 16.0) / 2.0),
                line: ctx.fonts.line_truncated(
                    row.item.display_title(),
                    12.0,
                    NORMAL,
                    area.w - 47.0,
                ),
                color: t.text,
                clip: Some(area),
            });
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, _target: Option<u64>) -> bool {
        match ev {
            InputEvent::Move(p) => {
                let hov = self.row_at(*p);
                if hov != self.hovered {
                    self.hovered = hov;
                    ctx.request_repaint();
                }
                hov.is_some()
            }
            InputEvent::Down { .. } => self.row_at(ev.pos().unwrap_or_default()).is_some(),
            InputEvent::Up { pos: p, button } => {
                let Some(index) = self.row_at(*p) else {
                    return false;
                };
                match button {
                    PointerButton::Primary => ctx
                        .queue
                        .push(UiEvent::Click(ids::TRAY_ROW_ACT + index as u64)),
                    PointerButton::Secondary => ctx
                        .queue
                        .push(UiEvent::Click(ids::TRAY_ROW_MENU + index as u64)),
                    PointerButton::Middle => ctx
                        .queue
                        .push(UiEvent::Click(ids::TRAY_ROW_AUX + index as u64)),
                    PointerButton::Other => {}
                }
                ctx.request_repaint();
                true
            }
            InputEvent::Scroll(p, dy) => {
                // wheel over a row scrolls the ITEM (legacy WheelHandler);
                // elsewhere it scrolls the list (legacy Flickable)
                if let Some(index) = self.row_at(*p) {
                    let notches = if *dy >= 0.0 { dy.ceil() } else { dy.floor() };
                    if notches != 0.0 {
                        ctx.queue
                            .push(UiEvent::Wheel(ids::TRAY_ROW_WHEEL + index as u64, notches));
                    }
                    return true;
                }
                self.scroll = (self.scroll - dy * ROW_H).clamp(0.0, self.max_scroll());
                ctx.request_repaint();
                true
            }
            InputEvent::Leave => {
                let was = self.hovered.take().is_some();
                if was {
                    ctx.request_repaint();
                }
                was
            }
            _ => false,
        }
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The DBusMenu page (replaces the list while a menu is open).
pub struct TrayMenuPage {
    pub title: String,
    pub entries: Vec<FlatEntry>,
    hovered: Option<usize>,
    scroll: f32,
    rect: Rect,
}

impl TrayMenuPage {
    pub fn new(title: &str, nodes: &[MenuNode]) -> Self {
        let mut entries = Vec::new();
        flatten_menu(nodes, 0, &mut entries);
        Self {
            title: title.to_string(),
            entries,
            hovered: None,
            scroll: 0.0,
            rect: Rect::default(),
        }
    }

    pub fn height_for(entries: usize) -> f32 {
        (24.0 + 20.0 + 4.0 + entries as f32 * 30.0 + 12.0).min(PANEL_MAX_H)
    }

    fn list_area(&self) -> Rect {
        Rect::new(
            self.rect.x + 12.0,
            self.rect.y + 12.0 + 20.0 + 4.0,
            self.rect.w - 24.0,
            (self.rect.h - 24.0 - 12.0).max(0.0),
        )
    }

    fn max_scroll(&self) -> f32 {
        (self.entries.len() as f32 * 30.0 - self.list_area().h).max(0.0)
    }

    /// Entry index at surface point p (separators are not clickable).
    fn entry_at(&self, p: Point) -> Option<usize> {
        let area = self.list_area();
        if !area.contains(p) {
            return None;
        }
        let idx = ((p.y - area.y + self.scroll) / 30.0).floor() as usize;
        match self.entries.get(idx) {
            Some(e) if !e.separator => Some(idx),
            _ => None,
        }
    }
}

impl Widget for TrayMenuPage {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(PANEL_WIDTH, Self::height_for(self.entries.len()))
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        use lme_ui::draw::DrawCmd;
        let t = ctx.theme;
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: t.small_radius,
            fill: Some(t.panel_fill.alpha(t.panel_opacity)),
            border: Some((t.outline_variant, 1.0)),
            clip: None,
        });
        // header: back button + item title
        out.push(DrawCmd::Text {
            pos: Point::new(self.rect.x + 12.0, self.rect.y + 12.0),
            line: ctx.fonts.line_truncated(
                &format!("‹  {}", self.title),
                12.0,
                NORMAL,
                self.rect.w - 24.0,
            ),
            color: t.muted,
            clip: Some(self.rect),
        });
        let area = self.list_area();
        let visible = ((area.h / 30.0).ceil() as usize).max(1);
        let first = (self.scroll / 30.0).floor() as usize;
        for i in 0..visible {
            let index = first + i;
            let Some(entry) = self.entries.get(index) else {
                break;
            };
            let y = area.y + i as f32 * 30.0 - (self.scroll - first as f32 * 30.0);
            if entry.separator {
                out.push(DrawCmd::RoundedRect {
                    rect: Rect::new(area.x + 6.0, y + 14.0, area.w - 12.0, 1.0),
                    radius: 0.0,
                    fill: Some(t.outline_variant),
                    border: None,
                    clip: Some(area),
                });
                continue;
            }
            let row_rect = Rect::new(area.x, y, area.w, 28.0);
            if Some(index) == self.hovered && entry.enabled {
                out.push(DrawCmd::RoundedRect {
                    rect: row_rect,
                    radius: 8.0,
                    fill: Some(t.accent.alpha(0.18)),
                    border: None,
                    clip: Some(area),
                });
            }
            let text_color = if entry.enabled { t.text } else { t.muted };
            let x = area.x + 10.0 + entry.depth as f32 * 16.0;
            // state glyph
            let glyph = match entry.toggle {
                Some((ToggleKind::Check, true)) => Some("✓"),
                Some((ToggleKind::Radio, true)) => Some("●"),
                Some((ToggleKind::Radio, false)) => Some("○"),
                _ => None,
            };
            let label_x = match glyph {
                Some(g) => {
                    out.push(DrawCmd::Text {
                        pos: Point::new(x, y + 7.0),
                        line: ctx.fonts.line_truncated(g, 13.0, NORMAL, 14.0),
                        color: text_color,
                        clip: Some(area),
                    });
                    x + 18.0
                }
                None => x,
            };
            let suffix = if entry.has_children { "  ▸" } else { "" };
            out.push(DrawCmd::Text {
                pos: Point::new(label_x, y + 7.0),
                line: ctx.fonts.line_truncated(
                    &format!("{}{suffix}", entry.label),
                    13.0,
                    NORMAL,
                    area.w - (label_x - area.x) - 8.0,
                ),
                color: text_color,
                clip: Some(area),
            });
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, _target: Option<u64>) -> bool {
        match ev {
            InputEvent::Move(p) => {
                let hov = self.entry_at(*p);
                if hov != self.hovered {
                    self.hovered = hov;
                    ctx.request_repaint();
                }
                hov.is_some()
            }
            InputEvent::Down { .. } => self.entry_at(ev.pos().unwrap_or_default()).is_some(),
            InputEvent::Up {
                pos: p,
                button: PointerButton::Primary,
            } => {
                // the header line doubles as the back button
                if p.y < self.list_area().y {
                    ctx.queue.push(UiEvent::Click(ids::TRAY_MENU_BACK));
                    ctx.request_repaint();
                    return true;
                }
                let Some(index) = self.entry_at(*p) else {
                    return false;
                };
                if self.entries[index].enabled {
                    ctx.queue
                        .push(UiEvent::Click(ids::TRAY_MENU_ENTRY + index as u64));
                    ctx.request_repaint();
                }
                true
            }
            InputEvent::Scroll(_, dy) => {
                self.scroll = (self.scroll - dy * 30.0).clamp(0.0, self.max_scroll());
                ctx.request_repaint();
                true
            }
            InputEvent::Leave => {
                let was = self.hovered.take().is_some();
                if was {
                    ctx.request_repaint();
                }
                was
            }
            _ => false,
        }
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Shell integration
// ---------------------------------------------------------------------------

impl Shell {
    /// Open (or close) the tray panel window. The window is a transparent
    /// full-overlay like the legacy lme-overlay (tray mode: backdrop alpha
    /// 0, exclusive keyboard, outside click closes).
    pub fn open_tray_panel(&mut self, state: &mut UiState) {
        if self.tray_panel.is_some() {
            self.close_tray_panel(state);
            return;
        }
        let items: Vec<TrayItem> = self.tray.items.clone();
        let h = TrayPanel::height_for(items.len());
        let mut spec = WindowSpec::full_overlay("lme-tray", KeyboardInteractivity::Exclusive);
        spec.background = Color::rgba(0.0, 0.0, 0.0, 0.0);
        let id = state.create_window(spec);
        let panel = TrayPanel::new(&items);
        let track = crate::daemon::reveal_track(id);
        state.set_root(
            id,
            Box::new(TrayPopup::new(Box::new(panel), PANEL_WIDTH, h, track)),
        );
        self.tray_panel = Some(id);
        self.start_reveal(state, id);
    }

    pub fn close_tray_panel(&mut self, state: &mut UiState) {
        if let Some(id) = self.tray_panel.take() {
            state.close_window(id);
        }
        self.tray_menu_open = None;
    }

    /// Swap the open tray window's content to the menu page.
    pub fn show_tray_menu(&mut self, state: &mut UiState, key: &str, nodes: &[MenuNode]) {
        let Some(id) = self.tray_panel else {
            return;
        };
        let Some(item) = self.tray.get(key) else {
            return;
        };
        let page = TrayMenuPage::new(item.display_title(), nodes);
        let h = TrayMenuPage::height_for(page.entries.len());
        let track = crate::daemon::reveal_track(id);
        self.tray_menu_open = Some(key.to_string());
        state.set_root(
            id,
            Box::new(TrayPopup::new(Box::new(page), PANEL_WIDTH, h, track)),
        );
        state.request_render(id);
    }

    /// Swap back from the menu page to the item list.
    pub fn show_tray_list(&mut self, state: &mut UiState) {
        let Some(id) = self.tray_panel else {
            return;
        };
        self.tray_menu_open = None;
        let items: Vec<TrayItem> = self.tray.items.clone();
        let h = TrayPanel::height_for(items.len());
        let track = crate::daemon::reveal_track(id);
        state.set_root(
            id,
            Box::new(TrayPopup::new(
                Box::new(TrayPanel::new(&items)),
                PANEL_WIDTH,
                h,
                track,
            )),
        );
        state.request_render(id);
    }

    /// TrayEvent handler (called from the calloop channel source).
    pub fn tray_event(&mut self, state: &mut UiState, ev: TrayEvent) {
        match ev {
            TrayEvent::Registered(item) | TrayEvent::Updated(item) => {
                self.tray.upsert(*item);
                self.tray_items = self.tray.items.len();
                self.rebuild_bars(state);
                self.refresh_tray_panel(state);
            }
            TrayEvent::Unregistered { key } => {
                self.tray.remove(&key);
                self.tray_items = self.tray.items.len();
                self.rebuild_bars(state);
                if self.tray_menu_open.as_deref() == Some(key.as_str()) {
                    self.show_tray_list(state);
                } else {
                    self.refresh_tray_panel(state);
                }
                if self.tray.items.is_empty() {
                    // legacy hides the module (and closes the panel)
                    self.close_tray_panel(state);
                }
            }
            TrayEvent::MenuReady { key, nodes } => {
                if self.tray_panel.is_some() && self.tray.get(&key).is_some() {
                    self.show_tray_menu(state, &key, &nodes);
                }
            }
            TrayEvent::MenuFailed { key, error } => {
                if self.tray.get(&key).is_some() {
                    self.show_osd(state, format!("Tray menu unavailable: {error}"), -1.0);
                }
            }
        }
    }

    fn refresh_tray_panel(&mut self, state: &mut UiState) {
        let Some(id) = self.tray_panel else {
            return;
        };
        if self.tray_menu_open.is_some() {
            return; // menu page owns the window; item updates wait
        }
        let items: Vec<TrayItem> = self.tray.items.clone();
        let h = TrayPanel::height_for(items.len());
        state.with_window(id, |window, _ctx| {
            let Some(popup) = window.root().as_any().downcast_mut::<TrayPopup>() else {
                return;
            };
            let Some(panel) = popup.panel_mut().as_any().downcast_mut::<TrayPanel>() else {
                return;
            };
            panel.set_items(&items);
        });
        // the panel keeps its open size; item counts usually change slowly
        let _ = h;
        state.request_render(id);
    }

    /// UiEvent router for tray widget ids (see tray::ids).
    pub fn handle_tray_click(&mut self, state: &mut UiState, id: u64) {
        use crate::tray::ids as tid;
        // menu page first (its ids sit above the row ids)
        if id >= tid::TRAY_MENU_BACK {
            if id == tid::TRAY_MENU_BACK {
                self.show_tray_list(state);
                return;
            }
            let index = (id - tid::TRAY_MENU_ENTRY) as usize;
            let Some(wid) = self.tray_panel else { return };
            let node_id = state.with_window(wid, |window, _ctx| {
                window
                    .root()
                    .as_any()
                    .downcast_mut::<TrayPopup>()
                    .and_then(|p| {
                        p.panel_mut()
                            .as_any()
                            .downcast_mut::<TrayMenuPage>()
                            .map(|page| page.entries.clone())
                    })
                    .and_then(|entries| entries.get(index).map(|e| e.node_id))
            });
            if let Some(node_id) = node_id.flatten() {
                if let Some(key) = self.tray_menu_open.clone() {
                    if let Some(h) = &self.tray_handle {
                        h.send(crate::tray::TrayCommand::MenuEvent { key, id: node_id });
                    }
                }
                self.show_tray_list(state);
            }
            return;
        }
        if id >= tid::TRAY_ROW_AUX {
            // middle: SecondaryActivate (legacy has no binding on the row,
            // but SNI defines it; kept as a natural extension)
            let index = (id - tid::TRAY_ROW_AUX) as usize;
            let Some(item) = self.tray.items.get(index).cloned() else {
                return;
            };
            if let Some(h) = &self.tray_handle {
                h.send(crate::tray::TrayCommand::SecondaryActivate { key: item.key });
            }
            self.close_tray_panel(state);
        } else if id >= tid::TRAY_ROW_MENU {
            // right: context menu (legacy displayMenu; needs hasMenu)
            let index = (id - tid::TRAY_ROW_MENU) as usize;
            let Some(item) = self.tray.items.get(index).cloned() else {
                return;
            };
            if item.menu_path.is_some() {
                if let Some(h) = &self.tray_handle {
                    h.send(crate::tray::TrayCommand::OpenMenu { key: item.key });
                }
            }
        } else {
            // primary: legacy `onlyMenu ? displayMenu() : (activate, close)`
            let index = (id - tid::TRAY_ROW_ACT) as usize;
            let Some(item) = self.tray.items.get(index).cloned() else {
                return;
            };
            if item.only_menu && item.menu_path.is_some() {
                if let Some(h) = &self.tray_handle {
                    h.send(crate::tray::TrayCommand::OpenMenu { key: item.key });
                }
            } else {
                if let Some(h) = &self.tray_handle {
                    h.send(crate::tray::TrayCommand::Activate { key: item.key });
                }
                self.close_tray_panel(state);
            }
        }
    }

    /// Wheel over tray rows: one notch = one SNI delta unit of ±120 (the
    /// angleDelta.y value legacy passed through).
    pub fn handle_tray_wheel(&mut self, id: u64, dy: f32) {
        use crate::tray::ids as tid;
        let index = (id - tid::TRAY_ROW_WHEEL) as usize;
        let Some(item) = self.tray.items.get(index) else {
            return;
        };
        let Some(h) = &self.tray_handle else {
            return;
        };
        h.send(crate::tray::TrayCommand::Scroll {
            key: item.key.clone(),
            delta: (dy * 120.0) as i32,
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn argb_solid(w: i32, h: i32, argb: [u8; 4]) -> (i32, i32, Vec<u8>) {
        (w, h, argb.repeat((w * h) as usize))
    }

    #[test]
    fn decodes_argb_pixmap_to_premultiplied_rgba() {
        let (w, h, data) = argb_solid(2, 2, [128, 255, 0, 0]);
        let pm = decode_argb_pixmap(w, h, &data).expect("decode");
        assert_eq!(pm.width(), 2);
        // red premultiplied by alpha 128/255 ≈ 128
        let p = pm.pixel(0, 0).expect("pixel");
        assert_eq!(p.red(), 128);
        assert_eq!(p.green(), 0);
        assert_eq!(p.alpha(), 128);
    }

    #[test]
    fn rejects_malformed_pixmaps() {
        assert!(decode_argb_pixmap(0, 4, &[0; 16]).is_none());
        assert!(decode_argb_pixmap(2, 2, &[0; 8]).is_none()); // short data
        assert!(decode_argb_pixmap(1024, 1024, &[0; 1024 * 1024 * 4]).is_none());
        // too big
    }

    #[test]
    fn picks_largest_pixmap() {
        let entries = vec![
            argb_solid(8, 8, [255, 0, 255, 0]),
            argb_solid(18, 18, [255, 0, 0, 255]),
        ];
        let pm = best_pixmap(&entries).expect("some");
        assert_eq!(pm.width(), 18);
    }

    #[test]
    fn display_title_fallback_chain() {
        let mut item = TrayItem {
            key: "k".into(),
            bus: "b".into(),
            path: "/p".into(),
            id: "org.x".into(),
            title: String::new(),
            category: "ApplicationStatus".into(),
            status: "Active".into(),
            icon_name: String::new(),
            icon_pixmap: None,
            attention_icon_name: String::new(),
            attention_pixmap: None,
            tooltip_title: String::new(),
            tooltip_desc: String::new(),
            only_menu: false,
            menu_path: None,
        };
        assert_eq!(item.display_title(), "org.x");
        item.title = "Title".into();
        assert_eq!(item.display_title(), "Title");
        item.tooltip_title = "Tooltip".into();
        assert_eq!(item.display_title(), "Tooltip");
        item.tooltip_title.clear();
        item.title.clear();
        item.id.clear();
        assert_eq!(item.display_title(), "Tray application");
    }

    #[test]
    fn strips_mnemonic_underscores() {
        assert_eq!(strip_mnemonic("_Quit"), "Quit");
        assert_eq!(strip_mnemonic("Open _File"), "Open File");
        assert_eq!(strip_mnemonic("A__B"), "A_B"); // escaped underscore
        assert_eq!(strip_mnemonic("No Marker"), "No Marker");
    }

    /// Build an (ia{sv}av) tree the way it arrives from the bus.
    /// `Structure::from` variant-wraps Value fields (Value's Type
    /// signature is "v"); the real wire form carries them bare, so
    /// build with `append_field`.
    fn layout_value(
        id: i32,
        label: &'static str,
        props: &[(&'static str, zbus::zvariant::Value<'static>)],
        children: Vec<zbus::zvariant::Value<'static>>,
    ) -> zbus::zvariant::Value<'static> {
        use zbus::zvariant::{Array, Dict, StructureBuilder, Value};
        let ks = zbus::zvariant::Signature::try_from("s").unwrap();
        let vs = zbus::zvariant::Signature::try_from("v").unwrap();
        let mut dict = Dict::new(&ks, &vs);
        // true a{sv} wire form: every value wrapped in a variant
        dict.append(
            Value::from("label"),
            Value::Value(Box::new(Value::from(label))),
        )
        .unwrap();
        for (k, v) in props {
            dict.append(Value::from(*k), Value::Value(Box::new(v.clone())))
                .unwrap();
        }
        let mut kids = Array::new(&zbus::zvariant::Signature::try_from("v").unwrap());
        for c in children {
            kids.append(Value::Value(Box::new(c))).unwrap();
        }
        Value::Structure(
            StructureBuilder::new()
                .append_field(Value::I32(id))
                .append_field(Value::Dict(dict))
                .append_field(Value::Array(kids))
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn parses_dbusmenu_layout_with_variants() {
        let child = layout_value(
            21,
            "_About",
            &[
                ("toggle-type", zbus::zvariant::Value::from("checkmark")),
                ("toggle-state", zbus::zvariant::Value::from(1)),
            ],
            vec![],
        );
        let sep = layout_value(
            30,
            "",
            &[("type", zbus::zvariant::Value::from("separator"))],
            vec![],
        );
        let sub = layout_value(
            40,
            "Sub",
            &[("children-display", zbus::zvariant::Value::from("submenu"))],
            vec![],
        );
        let root = layout_value(0, "Root", &[], vec![child, sep, sub]);
        // wire form: (revision, root) — again bare fields, no variant wrap
        let reply = zbus::zvariant::Value::Structure(
            zbus::zvariant::StructureBuilder::new()
                .append_field(zbus::zvariant::Value::U32(7))
                .append_field(root)
                .build()
                .unwrap(),
        );
        let node = parse_menu_layout(&reply).expect("root parses");
        assert_eq!(node.id, 0);
        assert_eq!(node.children.len(), 3);
        assert_eq!(node.children[0].label, "About");
        assert_eq!(node.children[0].toggle, Some((ToggleKind::Check, true)));
        assert!(node.children[1].separator);
        assert!(node.children[2].has_children);
    }

    #[test]
    fn flatten_drops_invisible_and_keeps_order() {
        let nodes = vec![MenuNode {
            id: 1,
            label: "One".into(),
            separator: false,
            enabled: true,
            visible: true,
            toggle: None,
            has_children: false,
            children: vec![MenuNode {
                id: 2,
                label: "Hidden".into(),
                separator: false,
                enabled: true,
                visible: false,
                toggle: None,
                has_children: false,
                children: vec![],
            }],
        }];
        let mut flat = Vec::new();
        flatten_menu(&nodes, 0, &mut flat);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].node_id, 1);

        let nested = vec![MenuNode {
            id: 10,
            label: "Parent".into(),
            separator: false,
            enabled: true,
            visible: true,
            toggle: None,
            has_children: true,
            children: vec![MenuNode {
                id: 11,
                label: "Child".into(),
                separator: false,
                enabled: false,
                visible: true,
                toggle: None,
                has_children: false,
                children: vec![],
            }],
        }];
        let mut flat = Vec::new();
        flatten_menu(&nested, 0, &mut flat);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[1].depth, 1);
        assert!(!flat[1].enabled);
    }

    #[test]
    fn hub_upsert_remove() {
        let mut hub = TrayHub::default();
        let item = |key: &str| TrayItem {
            key: key.into(),
            bus: key.into(),
            path: SNI_DEFAULT_PATH.into(),
            id: key.into(),
            title: String::new(),
            category: String::new(),
            status: "Active".into(),
            icon_name: String::new(),
            icon_pixmap: None,
            attention_icon_name: String::new(),
            attention_pixmap: None,
            tooltip_title: String::new(),
            tooltip_desc: String::new(),
            only_menu: false,
            menu_path: None,
        };
        assert!(hub.upsert(item("a")));
        assert!(!hub.upsert(item("a"))); // replace, no count change
        assert!(hub.upsert(item("b")));
        assert_eq!(hub.items.len(), 2);
        assert!(hub.remove("a").is_some());
        assert!(hub.remove("zz").is_none());
        assert_eq!(hub.items.len(), 1);
        let json = hub.json();
        assert_eq!(json["count"], 1);
        assert_eq!(json["items"][0]["key"], "b");
    }
}
