// SPDX-License-Identifier: GPL-3.0-or-later
//! Notification daemon (Tier 1.9): an org.freedesktop.Notifications server
//! on the session bus, an in-memory hub (live + history + DND + muted apps,
//! port of legacy qml/services/Notifications.qml) and a toast popup surface
//! (legacy shell.qml "lme-notifications" window: top-right, Overlay, up to
//! three stacked cards).
//!
//! Threading (ADR 0002 / D4): the D-Bus object server runs on its own
//! blocking thread; Notify/CloseNotification are forwarded to the daemon's
//! calloop loop through a channel, and the daemon sends signal-emission
//! commands (NotificationClosed, ActionInvoked) back through another
//! channel. D-Bus being unavailable is non-fatal — the shell runs without
//! external notifications.

use crate::daemon::Shell;
use lme_ui::color::Color;
use lme_ui::font_weight::{MEDIUM, NORMAL};
use lme_ui::geom::{Point, Rect, Size};
use lme_ui::wayland::{KeyboardInteractivity, Layer, UiState, WindowSpec};
use lme_ui::widgets::{Button, Ctx, InputEvent, UiEvent, Widget};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IFACE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Toast column width (legacy implicitWidth: 360).
pub const TOAST_WIDTH: u32 = 360;
/// Toast card spacing (legacy Column spacing: 10).
const TOAST_GAP: f32 = 10.0;
/// Maximum simultaneously visible toasts (legacy popups.slice(0, 3)).
pub const MAX_POPUPS: usize = 3;
/// History cap (legacy .slice(0, 200)).
pub const HISTORY_CAP: usize = 200;
/// Default expire timeout for notifications that pass -1 (legacy rule).
const DEFAULT_EXPIRE_MS: u64 = 7_000;

/// Close reasons (spec):
/// 1 expired, 2 dismissed by the user, 3 closed by a CloseNotification call.
pub const REASON_EXPIRED: u8 = 1;
pub const REASON_DISMISSED: u8 = 2;
pub const REASON_CLOSED: u8 = 3;

/// One notification snapshot (immutable once created; replacements make a
/// new snapshot, like the legacy plain-text entries).
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    /// Icon name for the Tier-2 notification center (not drawn on toasts,
    /// matching legacy popup design).
    #[allow(dead_code)]
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub urgency: u8, // 0 low, 1 normal, 2 critical
    /// (key, label) pairs; "default" is the click-anywhere action.
    pub actions: Vec<(String, String)>,
    /// Arrival time — the Tier-2 center renders "x min ago" from it.
    #[allow(dead_code)]
    pub timestamp: Instant,
    /// When the popup should auto-expire; None = stays until dismissed.
    pub expire_at: Option<Instant>,
    pub transient: bool,
    pub local: bool,
    /// Whether this notification currently shows a toast (set at receive
    /// time from the DND/muted checks; cleared when an action hides it).
    pub popup: bool,
}

impl Notification {
    /// Resolve the freedesktop expire timeout (legacy resetTimeout rules):
    /// 0 = never; -1 (client default) = 7 s unless critical (never);
    /// > 0 = that many milliseconds.
    pub fn resolve_expiry(expire_timeout: i32, urgency: u8) -> Option<Instant> {
        match expire_timeout {
            0 => None,
            t if t < 0 => {
                if urgency == 2 {
                    None
                } else {
                    Some(Instant::now() + Duration::from_millis(DEFAULT_EXPIRE_MS))
                }
            }
            t => Some(Instant::now() + Duration::from_millis(t.max(1) as u64)),
        }
    }

    fn card_height(&self) -> f32 {
        let mut h = 12.0 + 20.0 + 6.0 + 22.0; // pad + app row + gap + summary
        if !self.body.trim().is_empty() {
            h += 6.0 + 18.0;
        }
        if !self.actions.is_empty() {
            h += 10.0 + 32.0;
        }
        h + 12.0 // bottom pad
    }
}

/// Events from the D-Bus thread to the daemon loop.
pub enum DbusEvent {
    Notify(Box<Notification>),
    CloseRequested(u32),
}

/// Commands from the daemon loop to the D-Bus thread (signal emission).
pub enum DbusCommand {
    Closed(u32, u8),
    ActionInvoked(u32, String),
}

// ---------------------------------------------------------------------------
// Hub (shell-side state, driven on the calloop thread)
// ---------------------------------------------------------------------------

/// Live notification + its history flag. `NotificationHub` keeps the live
/// list (what can still be actioned or closed by the client) and the recent
/// history of expired/dismissed entries for the Tier-2 notification center.
#[derive(Default)]
pub struct NotificationHub {
    /// Newest first; entries with `popup: true` are rendered as toasts.
    pub live: Vec<Notification>,
    /// Expired/dismissed snapshots, newest first (transient ones removed).
    pub history: Vec<Notification>,
    pub unread: usize,
}

impl NotificationHub {
    pub fn popup_list(&self, max: usize) -> Vec<&Notification> {
        self.live.iter().filter(|n| n.popup).take(max).collect()
    }

    pub fn total_height(popups: &[&Notification]) -> f32 {
        popups.iter().map(|n| n.card_height()).sum::<f32>()
            + TOAST_GAP * popups.len().saturating_sub(1) as f32
    }

    pub fn push_history(&mut self, n: Notification) {
        if n.transient {
            return; // transient notifications never persist (legacy rule)
        }
        self.history.insert(0, n);
        self.history.truncate(HISTORY_CAP);
    }
}

/// Is DND active right now? (settings dndUntil: 0 off, -1 forever, else
/// epoch-ms deadline — the same encoding legacy uses.)
pub fn dnd_active(settings: &serde_json::Map<String, Value>) -> bool {
    let until = settings
        .get("dndUntil")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    match until {
        -1 => true,
        t if t > 0 => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            t > now
        }
        _ => false,
    }
}

/// Apps with popups suppressed (settings notificationMutedApps).
fn muted_apps(settings: &serde_json::Map<String, Value>) -> Vec<String> {
    settings
        .get("notificationMutedApps")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// D-Bus server thread
// ---------------------------------------------------------------------------

struct NotificationsIface {
    tx: calloop::channel::Sender<DbusEvent>,
    next_id: AtomicU32,
}

/// The #[zbus::interface] macro requires the inner methods to be inside an
/// `impl NotificationsIface` block it can rewrite.
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationsIface {
    /// The full Notify call. Hints carry `urgency` (u8/y) and possibly
    /// `transient` (b); we ignore everything else (no image/markup support,
    /// matching the legacy server's advertised capabilities).
    #[allow(clippy::too_many_arguments)] // the freedesktop Notify signature
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|v| <u8 as TryFrom<&zbus::zvariant::OwnedValue>>::try_from(v).ok())
            .unwrap_or(1);
        let transient = hints
            .get("transient")
            .and_then(|v| <bool as TryFrom<&zbus::zvariant::OwnedValue>>::try_from(v).ok())
            .unwrap_or(false);
        let action_pairs: Vec<(String, String)> = actions
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect();
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id.fetch_add(1, Ordering::SeqCst).max(1)
        };
        let notification = Notification {
            id,
            app_name: if app_name.is_empty() {
                "Application".to_string()
            } else {
                app_name.to_string()
            },
            app_icon: app_icon.to_string(),
            summary: if summary.is_empty() {
                "Notification".to_string()
            } else {
                summary.to_string()
            },
            body: body.to_string(),
            urgency,
            actions: action_pairs,
            timestamp: Instant::now(),
            expire_at: Notification::resolve_expiry(expire_timeout, urgency),
            transient,
            local: false,
            popup: false, // the shell sets it from DND/mute state
        };
        let _ = self.tx.send(DbusEvent::Notify(Box::new(notification)));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.tx.send(DbusEvent::CloseRequested(id));
    }

    fn get_capabilities(&self) -> Vec<&str> {
        // exactly what the legacy Quickshell server advertised
        vec!["body", "actions"]
    }

    fn get_server_information(&self) -> zbus::fdo::Result<(String, String, String, String)> {
        Ok((
            "lme-shell".into(),
            "lme".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        ))
    }
}

/// Handle to the running D-Bus server (None when the session bus is absent).
/// The server threads live exactly as long as the command sender stored in
/// `Shell`: dropping it closes the channel, the emitter thread exits, the
/// connection is released and the bus name becomes available again.
pub struct DbusHandle {
    pub cmd_tx: std::sync::mpsc::Sender<DbusCommand>,
}

/// Start the notification server on the session bus. Fails softly: any
/// error (no session bus, name taken) is returned to the caller, which
/// logs and continues without external notifications. The object server
/// lives on a dedicated blocking connection owned by the signal-emitter
/// thread; it exits (releasing the bus name) when the daemon drops the
/// command sender.
pub fn spawn(tx: calloop::channel::Sender<DbusEvent>) -> Result<DbusHandle, String> {
    let conn = zbus::blocking::connection::Builder::session()
        .map_err(|e| format!("session bus: {e}"))?
        .serve_at(
            PATH,
            NotificationsIface {
                tx,
                next_id: AtomicU32::new(1),
            },
        )
        .map_err(|e| format!("serve: {e}"))?
        .name(IFACE)
        .map_err(|e| format!("name {IFACE}: {e}"))?
        .build()
        .map_err(|e| format!("connect: {e}"))?;

    // The connection must stay alive; the emitter thread owns it and exits
    // when the daemon drops the command sender (Shell.dbus_cmd), releasing
    // the bus name.
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DbusCommand>();
    let emit_conn = conn.clone();
    std::thread::spawn(move || {
        // hold `conn` so the object server stays served for our lifetime
        let _keepalive = &conn;
        for cmd in cmd_rx {
            let result = match cmd {
                DbusCommand::Closed(id, reason) => emit_conn.emit_signal(
                    None::<&str>,
                    PATH,
                    IFACE,
                    "NotificationClosed",
                    &(id, reason),
                ),
                DbusCommand::ActionInvoked(id, key) => {
                    emit_conn.emit_signal(None::<&str>, PATH, IFACE, "ActionInvoked", &(id, key))
                }
            };
            if let Err(e) = result {
                eprintln!("lme-shell: notification signal: {e:?}");
            }
        }
    });
    Ok(DbusHandle { cmd_tx })
}

// ---------------------------------------------------------------------------
// Toast widgets (legacy shell.qml popup column)
// ---------------------------------------------------------------------------

/// Widget ids for toast interactions (u64 space; notif ids are u32 with
/// D-Bus ids < 0x4000_0000 and local ids >= 0x4000_0000 — both encodings
/// stay disjoint from each other and from the bar/panel id ranges).
pub fn toast_dismiss_id(notif_id: u32) -> u64 {
    0x7000_0000_0000 + notif_id as u64
}
pub fn toast_action_id(notif_id: u32, index: usize) -> u64 {
    0x7400_0000_0000 + (notif_id as u64) * 16 + index as u64
}

/// One toast card: app name + dismiss, summary, body line, action buttons.
pub struct ToastCard {
    pub notif: Notification,
    rect: Rect,
}

impl ToastCard {
    pub fn new(notif: Notification) -> Self {
        Self {
            notif,
            rect: Rect::default(),
        }
    }
}

impl Widget for ToastCard {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(TOAST_WIDTH as f32, self.notif.card_height())
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        use lme_ui::draw::DrawCmd;
        let t = ctx.theme;
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: t.radius,
            fill: Some(t.panel_fill.alpha(t.panel_opacity)),
            border: Some((t.outline_variant, 1.0)),
            clip: None,
        });
        let pad = 16.0;
        let inner_w = self.rect.w - pad * 2.0;
        // app name (muted, small)
        let app_line = ctx
            .fonts
            .line_truncated(&self.notif.app_name, 11.0, NORMAL, inner_w - 30.0);
        out.push(DrawCmd::Text {
            pos: Point::new(self.rect.x + pad, self.rect.y + 12.0),
            line: app_line,
            color: t.muted,
            clip: Some(self.rect),
        });
        // × glyph as a plain text draw (hit-tested via the id below)
        let close = ctx.fonts.line("×", 14.0, NORMAL);
        out.push(DrawCmd::Text {
            pos: Point::new(self.rect.x + self.rect.w - pad - 12.0, self.rect.y + 10.0),
            line: close,
            color: t.muted,
            clip: None,
        });
        // summary
        let summary = ctx
            .fonts
            .line_truncated(&self.notif.summary, 15.0, MEDIUM, inner_w);
        out.push(DrawCmd::Text {
            pos: Point::new(self.rect.x + pad, self.rect.y + 32.0),
            line: summary,
            color: t.text,
            clip: Some(self.rect),
        });
        // body (single truncated line; multi-line wrap is a Tier-2 polish)
        let mut y = self.rect.y + 58.0;
        if !self.notif.body.trim().is_empty() {
            let body = ctx
                .fonts
                .line_truncated(&self.notif.body, 13.0, NORMAL, inner_w);
            out.push(DrawCmd::Text {
                pos: Point::new(self.rect.x + pad, y),
                line: body,
                color: t.muted,
                clip: Some(self.rect),
            });
            y += 24.0;
        }
        // action buttons (small pills)
        if !self.notif.actions.is_empty() {
            let mut x = self.rect.x + pad;
            for (index, (_, label)) in self.notif.actions.iter().enumerate().take(3) {
                let mut b = Button::new(toast_action_id(self.notif.id, index), label.clone());
                b.label_size = 12.0;
                b.fill = t.surface.alpha(0.6);
                b.hover_fill = Some(t.surface);
                b.text_color = t.text;
                b.radius = t.small_radius;
                b.min_h = 28.0;
                b.layout(
                    ctx,
                    Rect::new(x, y, (label.len() as f32 * 7.0 + 24.0).min(110.0), 28.0),
                );
                b.paint(ctx, out);
                x += (label.len() as f32 * 7.0 + 24.0).min(110.0) + 8.0;
            }
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, _target: Option<u64>) -> bool {
        // The × area and the action buttons are the interactive zones.
        match ev {
            InputEvent::Move(p)
            | InputEvent::Down { pos: p, .. }
            | InputEvent::Up { pos: p, .. } => {
                let close_rect = Rect::new(
                    self.rect.x + self.rect.w - 46.0,
                    self.rect.y + 6.0,
                    40.0,
                    28.0,
                );
                if close_rect.contains(*p) {
                    if matches!(ev, InputEvent::Up { .. }) {
                        ctx.queue
                            .push(UiEvent::Click(toast_dismiss_id(self.notif.id)));
                        ctx.request_repaint();
                    }
                    return true;
                }
                if let InputEvent::Up { pos: p, .. } = ev {
                    // action pill hit-test (they were laid out left to right)
                    let mut x = self.rect.x + 16.0;
                    let mut y = self.rect.y + 58.0;
                    if !self.notif.body.trim().is_empty() {
                        y += 24.0;
                    }
                    for (index, (_, label)) in self.notif.actions.iter().enumerate().take(3) {
                        let w = (label.len() as f32 * 7.0 + 24.0).min(110.0);
                        if Rect::new(x, y, w, 28.0).contains(*p) {
                            ctx.queue
                                .push(UiEvent::Click(toast_action_id(self.notif.id, index)));
                            ctx.request_repaint();
                            return true;
                        }
                        x += w + 8.0;
                    }
                }
                matches!(ev, InputEvent::Down { .. } | InputEvent::Up { .. })
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

/// The stacked toast column (root widget of the lme-notifications window).
pub struct ToastColumn {
    cards: Vec<ToastCard>,
    rect: Rect,
}

impl ToastColumn {
    pub fn new(cards: Vec<ToastCard>) -> Self {
        Self {
            cards,
            rect: Rect::default(),
        }
    }
}

impl Widget for ToastColumn {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        let h: f32 = self
            .cards
            .iter()
            .map(|c| c.notif.card_height())
            .sum::<f32>()
            + TOAST_GAP * self.cards.len().saturating_sub(1) as f32;
        Size::new(TOAST_WIDTH as f32, h)
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        let mut y = bounds.y;
        for card in &mut self.cards {
            let h = card.notif.card_height();
            card.layout(_ctx, Rect::new(bounds.x, y, bounds.w, h));
            y += h + TOAST_GAP;
        }
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        for card in &mut self.cards {
            card.paint(ctx, out);
        }
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        let mut consumed = false;
        for card in &mut self.cards {
            if !consumed {
                consumed = card.deliver(ctx, ev, target);
            }
        }
        consumed
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Shell-side glue (called from the daemon's calloop thread)
// ---------------------------------------------------------------------------

impl Shell {
    /// Handle one event from the D-Bus thread.
    pub fn notification_event(&mut self, state: &mut UiState, ev: DbusEvent) {
        match ev {
            DbusEvent::Notify(boxed) => {
                let mut n = *boxed;
                let is_new = !self.notifications.live.iter().any(|e| e.id == n.id);
                n.popup = !dnd_active(&self.settings.values)
                    && !muted_apps(&self.settings.values).contains(&n.app_name);
                let id = n.id;
                let expire_at = n.expire_at;
                // replace-or-insert, newest first (legacy receive/refresh)
                self.notifications.live.retain(|e| e.id != n.id);
                self.notifications.live.insert(0, n);
                if is_new {
                    self.notifications.unread += 1;
                }
                self.rebuild_toasts(state);
                self.rebuild_bars(state);
                self.schedule_toast_expiry(state, id, expire_at);
            }
            DbusEvent::CloseRequested(id) => {
                // the client withdrew it: drop from live AND history
                if let Some(i) = self.notifications.live.iter().position(|n| n.id == id) {
                    self.notifications.live.remove(i);
                    self.notifications.history.retain(|h| h.id != id);
                    self.emit_closed(id, REASON_CLOSED);
                    self.rebuild_toasts(state);
                    self.rebuild_bars(state);
                }
            }
        }
    }

    fn emit_closed(&mut self, id: u32, reason: u8) {
        if let Some(tx) = &self.dbus_cmd {
            let _ = tx.send(DbusCommand::Closed(id, reason));
        }
    }

    fn emit_action(&mut self, id: u32, action: String) {
        if let Some(tx) = &self.dbus_cmd {
            let _ = tx.send(DbusCommand::ActionInvoked(id, action));
        }
    }

    /// Expiry timer fired (or an outdated timer fired): close the toast if
    /// the deadline really passed.
    pub fn expire_notification(&mut self, state: &mut UiState, id: u32) {
        let Some(n) = self
            .notifications
            .live
            .iter()
            .position(|n| n.id == id)
            .map(|i| self.notifications.live.remove(i))
        else {
            return;
        };
        if let Some(at) = n.expire_at {
            if at > Instant::now() {
                // replaced since this timer was scheduled — put it back
                self.notifications.live.insert(0, n);
                return;
            }
        }
        self.notifications.push_history(n);
        self.emit_closed(id, REASON_EXPIRED);
        self.rebuild_toasts(state);
        self.rebuild_bars(state);
    }

    /// The user clicked × on a toast.
    pub fn dismiss_notification(&mut self, state: &mut UiState, id: u32) {
        if let Some(i) = self.notifications.live.iter().position(|n| n.id == id) {
            let n = self.notifications.live.remove(i);
            self.notifications.push_history(n);
            self.emit_closed(id, REASON_DISMISSED);
            self.rebuild_toasts(state);
        }
    }

    /// The user clicked an action pill.
    pub fn invoke_notification_action(&mut self, state: &mut UiState, id: u32, action: String) {
        self.emit_action(id, action);
        // hide the popup but keep it live (legacy hidePopup)
        if let Some(n) = self.notifications.live.iter_mut().find(|n| n.id == id) {
            n.popup = false;
        }
        self.rebuild_toasts(state);
    }

    /// Local notification (msg notify / internal messages): app "LME Shell",
    /// 7 s expiry (legacy local()).
    pub fn local_notification(&mut self, state: &mut UiState, summary: String, body: String) {
        let id = self.next_local_id;
        self.next_local_id += 1;
        let n = Notification {
            id,
            app_name: "LME Shell".into(),
            app_icon: String::new(),
            summary,
            body,
            urgency: 1,
            actions: Vec::new(),
            timestamp: Instant::now(),
            expire_at: Notification::resolve_expiry(-1, 1),
            transient: false,
            local: true,
            popup: !dnd_active(&self.settings.values),
        };
        let expire_at = n.expire_at;
        self.notifications.live.insert(0, n);
        self.notifications.unread += 1;
        self.rebuild_toasts(state);
        self.rebuild_bars(state);
        self.schedule_toast_expiry(state, id, expire_at);
    }

    /// Bell clicked: opening the (Tier-2) center marks notifications read
    /// (legacy NotificationCenter markRead-on-open).
    pub fn mark_notifications_read(&mut self, state: &mut UiState) {
        if self.notifications.unread != 0 {
            self.notifications.unread = 0;
            self.rebuild_bars(state);
        }
    }

    fn schedule_toast_expiry(&mut self, state: &mut UiState, id: u32, expire_at: Option<Instant>) {
        let Some(at) = expire_at else { return };
        let Some(handle) = state.handle.clone() else {
            return;
        };
        let timer = calloop::timer::Timer::from_deadline(at);
        let _ = handle.insert_source(timer, move |_, _, state| {
            crate::daemon::with_shell(state, |shell, state| {
                shell.expire_notification(state, id);
            });
            calloop::timer::TimeoutAction::Drop
        });
    }

    /// Recreate the lme-notifications window to match the popup list
    /// (legacy: implicitHeight auto-resizes; we rebuild the surface —
    /// toasts change rarely).
    pub fn rebuild_toasts(&mut self, state: &mut UiState) {
        if let Some(id) = self.toasts.take() {
            state.close_window(id);
        }
        let popups = self.notifications.popup_list(MAX_POPUPS);
        if popups.is_empty() {
            return;
        }
        let height = NotificationHub::total_height(&popups);
        let spec = WindowSpec {
            namespace: "lme-notifications".into(),
            layer: Layer::Overlay,
            anchor: (true, true, false, false), // top + right
            exclusive_zone: -1,
            size: Some((TOAST_WIDTH, height.ceil() as u32)),
            keyboard: KeyboardInteractivity::None,
            margins: (self.theme.panel_top_margin as i32, 14, 0, 0),
            output: state.focused_output().map(|o| o.output),
            background: Color::rgba(0.0, 0.0, 0.0, 0.0),
        };
        let id = state.create_window(spec);
        let cards = popups
            .into_iter()
            .map(|n| ToastCard::new(n.clone()))
            .collect();
        state.set_root(id, Box::new(ToastColumn::new(cards)));
        state.request_render(id);
        self.toasts = Some(id);
    }
    /// Toast interaction clicks (routed from handle_click).
    pub fn handle_toast_click(&mut self, state: &mut UiState, id: u64) -> bool {
        if id >= 0x7400_0000_0000 {
            let notif_id = ((id - 0x7400_0000_0000) / 16) as u32;
            let index = ((id - 0x7400_0000_0000) % 16) as usize;
            if let Some((key, _)) = self
                .notifications
                .live
                .iter()
                .find(|n| n.id == notif_id)
                .and_then(|n| n.actions.get(index))
                .cloned()
            {
                self.invoke_notification_action(state, notif_id, key);
                return true;
            }
            return true;
        }
        // Bounded: the tray id space starts at 0x7100_0000_0000 (tray.rs
        // ids) — the old open-ended `>= 0x7000…` swallowed every tray row
        // click. Toast ids are 0x7000_0000_0000 + u32 (≤ 0x7000_FFFF_FFFF).
        if (0x7000_0000_0000..0x7100_0000_0000).contains(&id) {
            let notif_id = (id - 0x7000_0000_0000) as u32;
            self.dismiss_notification(state, notif_id);
            return true;
        }
        false
    }
}

/// History serialization for the IPC surface (notification center payload).
pub fn history_json(hub: &NotificationHub) -> Value {
    let live: Vec<Value> = hub
        .live
        .iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id, "appName": n.app_name, "summary": n.summary,
                "body": n.body, "urgency": n.urgency, "active": true,
                "local": n.local,
            })
        })
        .collect();
    let past: Vec<Value> = hub
        .history
        .iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id, "appName": n.app_name, "summary": n.summary,
                "body": n.body, "urgency": n.urgency, "active": false,
                "local": n.local,
            })
        })
        .collect();
    serde_json::json!({ "live": live, "history": past, "unread": hub.unread })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notif(id: u32, app: &str, urgency: u8, expire: i32) -> Notification {
        Notification {
            id,
            app_name: app.into(),
            app_icon: String::new(),
            summary: format!("S{id}"),
            body: String::new(),
            urgency,
            actions: Vec::new(),
            timestamp: Instant::now(),
            expire_at: Notification::resolve_expiry(expire, urgency),
            transient: false,
            local: false,
            popup: true,
        }
    }

    #[test]
    fn expiry_rules_match_legacy() {
        // 0 = never
        assert!(Notification::resolve_expiry(0, 1).is_none());
        // default (-1): 7 s unless critical
        let d = Notification::resolve_expiry(-1, 1).unwrap() - Instant::now();
        assert!(d >= Duration::from_millis(6_900) && d <= Duration::from_millis(7_000));
        assert!(Notification::resolve_expiry(-1, 2).is_none());
        // explicit timeout
        let d = Notification::resolve_expiry(2500, 0).unwrap() - Instant::now();
        assert!(d >= Duration::from_millis(2400) && d <= Duration::from_millis(2600));
    }

    #[test]
    fn popup_list_takes_three_newest() {
        let mut hub = NotificationHub::default();
        for i in 1..=5 {
            hub.live.insert(0, notif(i, "app", 1, 5000));
        }
        let ids: Vec<u32> = hub.popup_list(3).iter().map(|n| n.id).collect();
        assert_eq!(ids, [5, 4, 3]);
        // non-popup entries are skipped
        hub.live[1].popup = false;
        let ids: Vec<u32> = hub.popup_list(3).iter().map(|n| n.id).collect();
        assert_eq!(ids, [5, 3, 2]);
    }

    #[test]
    fn history_receives_retired_and_drops_transient() {
        let mut hub = NotificationHub::default();
        hub.live.insert(0, notif(1, "app", 1, 0));
        let mut t = notif(2, "app", 1, 0);
        t.transient = true;
        hub.live.insert(0, t);
        let n = hub.live.remove(1);
        hub.push_history(n);
        let n = hub.live.remove(0);
        hub.push_history(n);
        assert_eq!(hub.history.len(), 1);
        assert_eq!(hub.history[0].id, 1);
        assert!(hub.live.is_empty());
    }

    #[test]
    fn dnd_window_semantics() {
        let mut s = serde_json::Map::new();
        s.insert("dndUntil".into(), Value::from(0));
        assert!(!dnd_active(&s));
        s.insert("dndUntil".into(), Value::from(-1));
        assert!(dnd_active(&s));
        // deadline far in the future (year 2100)
        s.insert("dndUntil".into(), Value::from(4_102_444_800_000i64));
        assert!(dnd_active(&s));
        // deadline in the past
        s.insert("dndUntil".into(), Value::from(1000));
        assert!(!dnd_active(&s));
    }

    #[test]
    fn toast_widget_ids_roundtrip() {
        for id in [1u32, 42, 999_999, 0x4000_0000] {
            assert_eq!((toast_dismiss_id(id) - 0x7000_0000_0000) as u32, id);
        }
        let w = toast_action_id(7, 2);
        assert_eq!((w - 0x7400_0000_0000) / 16, 7);
        assert_eq!((w - 0x7400_0000_0000) % 16, 2);
        // ranges never overlap, even for the largest possible ids
        assert!(toast_dismiss_id(u32::MAX) < 0x7400_0000_0000);
        assert!(toast_action_id(u32::MAX, 15) > toast_dismiss_id(u32::MAX));
        // and they sit far above every bar/panel/workspace id range
        assert!(toast_dismiss_id(0) > 0x3000_0000);
    }
}
