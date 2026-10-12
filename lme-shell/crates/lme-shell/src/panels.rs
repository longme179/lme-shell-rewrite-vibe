// SPDX-License-Identifier: GPL-3.0-or-later
//! Panel surfaces: power menu (REBUILD_SPEC §5, PowerMenu.qml port) and the
//! multi-mode launcher (REBUILD_SPEC §6.1). Panels are created lazily on
//! toggle and destroyed on close (ADR 0002).

use crate::launcher_data::{self, LauncherResult, Mode, ResultKind};
use lme_ui::color::Color;
use lme_ui::font_weight::{BOLD, MEDIUM, NORMAL};
use lme_ui::geom::{Point, Rect, Size};
use lme_ui::widgets::{
    Ctx, FocusScope, HBox, InputEvent, Label, Spacer, TextInput, UiEvent, VBox, Widget,
};
use serde_json::Value;

/// Data bundle for launcher result computation (avoids borrowing Shell).
pub struct LauncherData<'a> {
    pub apps: &'a [Value],
    pub favorites: &'a [String],
    pub windows: Option<&'a Value>,
    pub emoji: &'a launcher_data::EmojiIndex,
}

/// Centers a fixed-size panel inside the (full-output) window and applies
/// the reveal animation (alpha + slide-up) from an animator track.
pub struct CenteredPanel {
    panel: Box<dyn Widget>,
    w: f32,
    h: f32,
    reveal_track: u64,
    rect: Rect,
}

impl CenteredPanel {
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

impl Widget for CenteredPanel {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(self.w, self.h)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        let x = bounds.x + (bounds.w - self.w).max(0.0) / 2.0;
        let y = bounds.y + (bounds.h - self.h).max(0.0) / 2.0;
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
        let eased = reveal;
        let mut panel_cmds = Vec::new();
        self.panel.paint(ctx, &mut panel_cmds);
        let dy = (1.0 - eased) * 14.0;
        let alpha = eased;
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
        // translate pointer coordinates into panel space
        let inner = self.panel.rect();
        let translated = match ev {
            InputEvent::Move(p) => InputEvent::Move(Point::new(p.x - inner.x, p.y - inner.y)),
            InputEvent::Down(p) => InputEvent::Down(Point::new(p.x - inner.x, p.y - inner.y)),
            InputEvent::Up(p) => InputEvent::Up(Point::new(p.x - inner.x, p.y - inner.y)),
            InputEvent::Scroll(p, d) => {
                InputEvent::Scroll(Point::new(p.x - inner.x, p.y - inner.y), *d)
            }
            other => other.clone(),
        };
        self.panel.deliver(ctx, &translated, target)
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

pub mod ids {
    pub const POWER_LOCK: u64 = 0x5001;
    pub const POWER_SUSPEND: u64 = 0x5002;
    pub const POWER_LOGOUT: u64 = 0x5003;
    pub const POWER_REBOOT: u64 = 0x5004;
    pub const POWER_OFF: u64 = 0x5005;
    pub const POWER_CLOSE: u64 = 0x5006;
    pub const POWER_CONFIRM: u64 = 0x5007;
    pub const POWER_BACK: u64 = 0x5008;
    pub const LAUNCHER_INPUT: u64 = 0x6001;
    pub const LAUNCHER_LIST: u64 = 0x6002;
}

// ---------------------------------------------------------------------------
// Power menu
// ---------------------------------------------------------------------------

pub struct PowerMenuPanel {
    pub pending: Option<(&'static str, &'static str)>, // (action, label)
    rect: Rect,
    content: FocusScope,
}

impl PowerMenuPanel {
    pub fn new(theme: &lme_ui::theme::Theme) -> Self {
        Self {
            pending: None,
            rect: Rect::default(),
            content: build_power_grid(theme, None),
        }
    }

    pub fn request_confirmation(
        &mut self,
        theme: &lme_ui::theme::Theme,
        action: &'static str,
        label: &'static str,
    ) {
        self.pending = Some((action, label));
        self.content = build_power_grid(theme, self.pending);
    }

    #[allow(dead_code)]
    pub fn clear_confirmation(&mut self, theme: &lme_ui::theme::Theme) {
        self.pending = None;
        self.content = build_power_grid(theme, None);
    }

    pub fn size() -> (u32, u32) {
        (440, 430)
    }
}

fn power_button(
    theme: &lme_ui::theme::Theme,
    id: u64,
    label: &str,
    symbol: &str,
    accent: Color,
) -> Button {
    let mut b = Button::new(id, label);
    b.label_size = 14.0;
    b.label_weight = MEDIUM;
    b.fill = theme.surface.alpha(0.55);
    b.hover_fill = Some(theme.surface);
    b.text_color = theme.text;
    b.radius = theme.small_radius;
    b.border = Some((theme.border, 1.0));
    b.min_h = 58.0;
    let _ = (symbol, accent);
    b
}

use lme_ui::widgets::Button;

fn build_power_grid(
    theme: &lme_ui::theme::Theme,
    pending: Option<(&'static str, &'static str)>,
) -> FocusScope {
    let t = theme;
    let mut column = VBox::new(14.0).padded(24.0);
    // header
    let (title, subtitle) = match pending {
        Some((_, label)) => (format!("{label}?"), "Confirm to continue.".to_string()),
        None => (
            "Take a break".to_string(),
            "Your session, your pace.".to_string(),
        ),
    };
    column = column.push(
        HBox::new(0.0)
            .push(
                VBox::new(3.0)
                    .push(Label::styled(title, 24.0, BOLD, t.text))
                    .push(Label::styled(subtitle, 13.0, NORMAL, t.muted)),
            )
            .push(Spacer::fill())
            .push(power_button(t, ids::POWER_CLOSE, "×", "", t.text)),
    );
    match pending {
        None => {
            let mut grid = VBox::new(12.0);
            grid = grid.push(
                HBox::new(12.0)
                    .push(power_button(t, ids::POWER_LOCK, "Lock", "◈", t.cyan))
                    .push(power_button(
                        t,
                        ids::POWER_SUSPEND,
                        "Suspend",
                        "☾",
                        t.purple,
                    )),
            );
            grid = grid.push(
                HBox::new(12.0)
                    .push(power_button(t, ids::POWER_LOGOUT, "Log out", "↗", t.yellow))
                    .push(power_button(t, ids::POWER_REBOOT, "Restart", "↻", t.pink)),
            );
            grid = grid.push(power_button(t, ids::POWER_OFF, "Shut down", "⏻", t.red));
            column = column.push(grid);
            column = column.push(Spacer::fill());
        }
        Some((action, _)) => {
            let message = if action == "suspend" {
                "Your session will stay open while the laptop sleeps."
            } else {
                "Save your work before continuing. Open applications may close."
            };
            let mut confirm = VBox::new(16.0).padded(18.0);
            confirm = confirm.push(Label::styled(message, 14.0, NORMAL, t.text));
            confirm = confirm.push(Spacer::fill());
            confirm = confirm.push(
                HBox::new(12.0)
                    .push(power_button(t, ids::POWER_BACK, "Go back", "", t.text))
                    .push({
                        let mut b = power_button(
                            t,
                            ids::POWER_CONFIRM,
                            &format!("Confirm {}", pending_label(pending).to_lowercase()),
                            "",
                            t.red,
                        );
                        b.fill = t.red;
                        b.text_color = t.background;
                        b
                    }),
            );
            column = column.push(confirm);
        }
    }
    column = column.push(Label::styled(
        "Session actions run through Fedora's user session and logind.",
        12.0,
        NORMAL,
        t.muted,
    ));
    FocusScope::new(Box::new(column), false)
}

fn pending_label(pending: Option<(&'static str, &'static str)>) -> &'static str {
    pending.map(|(_, l)| l).unwrap_or("")
}

impl Widget for PowerMenuPanel {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.content.measure(ctx)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        self.content.layout(ctx, bounds);
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        // glass panel background
        out.push(lme_ui::draw::DrawCmd::RoundedRect {
            rect: self.rect,
            radius: ctx.theme.radius,
            fill: Some(ctx.theme.panel_fill.alpha(ctx.theme.panel_opacity)),
            border: Some((ctx.theme.outline_variant, 1.0)),
            clip: None,
        });
        self.content.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        self.content.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.content.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Launcher
// ---------------------------------------------------------------------------

pub struct LauncherPanel {
    pub query: String,
    pub results: Vec<LauncherResult>,
    pub selected: usize,
    pub calculation_error: String,
    pub favorites_only: bool,
    input: TextInput,
    rect: Rect,
}

impl LauncherPanel {
    pub fn new(theme: &lme_ui::theme::Theme) -> Self {
        let mut input = TextInput::themed(
            ids::LAUNCHER_INPUT,
            "Apps · = calculate · : emoji · @ windows",
            theme,
        );
        input.size = 16.0;
        Self {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            calculation_error: String::new(),
            favorites_only: false,
            input,
            rect: Rect::default(),
        }
    }

    pub fn size() -> (u32, u32) {
        (520, 560)
    }

    /// Recompute results for the current query (called on text change and
    /// when the shell's app/niri state changes).
    pub fn update_results(&mut self, data: &LauncherData) {
        self.query = self.input.text.clone();
        self.calculation_error.clear();
        let text = self
            .query
            .trim_start_matches([':', '@'])
            .trim()
            .to_lowercase();
        let mode = launcher_data::detect_mode(&self.query);
        let mut results = Vec::new();
        match mode {
            Mode::Emoji => {
                let mut hits = Vec::new();
                data.emoji.search(&text, &mut hits);
                for (emoji, name) in hits {
                    results.push(LauncherResult {
                        kind: ResultKind::Copy {
                            value: emoji.clone(),
                        },
                        name: format!("{emoji}  {name}"),
                        detail: "Copy emoji".into(),
                        icon: None,
                        favorite: false,
                    });
                }
            }
            Mode::Calculator => {
                let expression = self.query.trim_start_matches('=').trim().to_string();
                if !expression.is_empty() {
                    match launcher_data::calculate(&expression) {
                        Ok(value) => {
                            let formatted = launcher_data::format_result(value);
                            results.push(LauncherResult {
                                kind: ResultKind::Copy {
                                    value: formatted.clone(),
                                },
                                name: formatted,
                                detail: "Enter to copy result".into(),
                                icon: None,
                                favorite: false,
                            });
                        }
                        Err(e) => self.calculation_error = e,
                    }
                }
            }
            Mode::Windows => {
                self.push_windows(data.windows, &text, &mut results);
            }
            Mode::Apps => {
                let favorites: &[String] = data.favorites;
                let ranked =
                    launcher_data::rank_apps(&text, data.apps, favorites, self.favorites_only);
                for app in ranked {
                    results.push(LauncherResult {
                        kind: ResultKind::App {
                            id: app
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        },
                        name: app
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        detail: app
                            .get("comment")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Application")
                            .to_string(),
                        icon: app
                            .get("icon")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        favorite: false,
                    });
                }
                if !text.is_empty() && !self.favorites_only {
                    self.push_windows(data.windows, &text, &mut results);
                }
            }
        }
        self.selected = 0;
        self.results = results;
    }

    fn push_windows(&self, windows: Option<&Value>, text: &str, out: &mut Vec<LauncherResult>) {
        let Some(windows) = windows.and_then(|v| v.as_array()) else {
            return;
        };
        for w in windows {
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let app_id = w.get("app_id").and_then(|v| v.as_str()).unwrap_or("");
            let haystack = format!("{title} {app_id}").to_lowercase();
            if !haystack.contains(text) {
                continue;
            }
            out.push(LauncherResult {
                kind: ResultKind::Window {
                    window_id: w.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                },
                name: if title.is_empty() {
                    app_id.to_string()
                } else {
                    title.to_string()
                },
                detail: "Open window".into(),
                icon: None,
                favorite: false,
            });
        }
    }

    pub fn selected_result(&self) -> Option<&LauncherResult> {
        self.results.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: i32) -> bool {
        let max = self.results.len().saturating_sub(1);
        let new = (self.selected as i32 + delta).clamp(0, max as i32) as usize;
        if new != self.selected {
            self.selected = new;
            return true;
        }
        false
    }
}

impl Widget for LauncherPanel {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        let (w, h) = Self::size();
        Size::new(w as f32, h as f32)
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
            fill: Some(t.launcher_fill.alpha(t.panel_opacity)),
            border: Some((t.outline_variant, 1.0)),
            clip: None,
        });
        // search field
        let field = Rect::new(
            self.rect.x + 16.0,
            self.rect.y + 16.0,
            self.rect.w - 32.0,
            46.0,
        );
        self.input.layout(ctx, field);
        self.input.paint(ctx, out);
        // error line
        if !self.calculation_error.is_empty() {
            out.push(DrawCmd::Text {
                pos: Point::new(self.rect.x + 18.0, self.rect.y + 70.0),
                line: ctx.fonts.line_truncated(
                    &self.calculation_error,
                    13.0,
                    NORMAL,
                    self.rect.w - 36.0,
                ),
                color: t.red,
                clip: Some(self.rect),
            });
        }
        self.paint_list(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        // Panel-level keys first: arrows move the selection, Enter activates.
        if let InputEvent::Key(k) = ev {
            match k.sym {
                lme_ui::widgets::XKB_UP => {
                    if self.move_selection(-1) {
                        ctx.request_repaint();
                    }
                    return true;
                }
                lme_ui::widgets::XKB_DOWN => {
                    if self.move_selection(1) {
                        ctx.request_repaint();
                    }
                    return true;
                }
                _ if k.is_enter() => {
                    if self.selected_result().is_some() {
                        ctx.queue.push(UiEvent::Click(ids::LAUNCHER_LIST));
                    }
                    return true;
                }
                _ => {}
            }
        }
        // Everything else goes to the text input.
        let consumed = self
            .input
            .deliver(ctx, ev, target.or(Some(ids::LAUNCHER_INPUT)));
        if consumed {
            if let InputEvent::Key(k) = ev {
                if !k.utf8.is_empty() || k.sym == lme_ui::widgets::XKB_BACKSPACE {
                    ctx.queue.push(UiEvent::Slide(ids::LAUNCHER_INPUT, 0.0)); // signal: query changed
                }
            }
        }
        consumed
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        out.push(ids::LAUNCHER_INPUT);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl LauncherPanel {
    fn paint_list(&mut self, ctx: &mut Ctx, out: &mut Vec<lme_ui::draw::DrawCmd>) {
        use lme_ui::draw::DrawCmd;
        let list_area = Rect::new(
            self.rect.x + 16.0,
            self.rect.y + 74.0,
            self.rect.w - 32.0,
            (self.rect.h - 90.0).max(0.0),
        );
        let row_h = 52.0f32;
        let visible = ((list_area.h / row_h).floor() as usize).max(1);
        let first = if self.selected >= visible {
            self.selected + 1 - visible
        } else {
            0
        };
        for i in 0..visible {
            let index = first + i;
            let Some(result) = self.results.get(index) else {
                break;
            };
            let y = list_area.y + i as f32 * row_h;
            if index == self.selected {
                out.push(DrawCmd::RoundedRect {
                    rect: Rect::new(list_area.x, y, list_area.w, row_h - 6.0),
                    radius: ctx.theme.small_radius,
                    fill: Some(ctx.theme.primary_container.alpha(0.55)),
                    border: None,
                    clip: Some(list_area),
                });
            }
            out.push(DrawCmd::Text {
                pos: Point::new(list_area.x + 14.0, y + 8.0),
                line: ctx
                    .fonts
                    .line_truncated(&result.name, 15.0, MEDIUM, list_area.w - 28.0),
                color: ctx.theme.text,
                clip: Some(list_area),
            });
            out.push(DrawCmd::Text {
                pos: Point::new(list_area.x + 14.0, y + 30.0),
                line: ctx
                    .fonts
                    .line_truncated(&result.detail, 12.0, NORMAL, list_area.w - 28.0),
                color: ctx.theme.muted,
                clip: Some(list_area),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// OSD / status toast
// ---------------------------------------------------------------------------

pub struct OsdPanel {
    pub label: String,
    pub value: f32, // 0..=100; negative = no bar (status message)
    rect: Rect,
}

impl OsdPanel {
    pub fn new(label: String, value: f32) -> Self {
        Self {
            label,
            value,
            rect: Rect::default(),
        }
    }
    pub fn size(&self) -> (u32, u32) {
        if self.value < 0.0 {
            (460, 76)
        } else {
            (240, 64)
        }
    }
}

impl Widget for OsdPanel {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        let (w, h) = self.size();
        Size::new(w as f32, h as f32)
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
        if self.value < 0.0 {
            out.push(DrawCmd::Text {
                pos: Point::new(self.rect.x + 20.0, self.rect.y + (self.rect.h - 18.0) / 2.0),
                line: ctx
                    .fonts
                    .line_truncated(&self.label, 14.0, NORMAL, self.rect.w - 40.0),
                color: t.text,
                clip: Some(self.rect),
            });
        } else {
            out.push(DrawCmd::Text {
                pos: Point::new(self.rect.x + 20.0, self.rect.y + 14.0),
                line: {
                    let caption = format!("{}  {}%", self.label, self.value.round() as i64);
                    ctx.fonts.line(&caption, 14.0, MEDIUM)
                },
                color: t.text,
                clip: None,
            });
            let bar = Rect::new(
                self.rect.x + 20.0,
                self.rect.y + self.rect.h - 20.0,
                self.rect.w - 40.0,
                4.0,
            );
            out.push(DrawCmd::RoundedRect {
                rect: bar,
                radius: 2.0,
                fill: Some(t.surface),
                border: None,
                clip: None,
            });
            out.push(DrawCmd::RoundedRect {
                rect: Rect::new(
                    bar.x,
                    bar.y,
                    bar.w * (self.value.clamp(0.0, 100.0) / 100.0),
                    bar.h,
                ),
                radius: 2.0,
                fill: Some(t.accent),
                border: None,
                clip: None,
            });
        }
    }
    fn deliver(&mut self, _ctx: &mut Ctx, _ev: &InputEvent, _t: Option<u64>) -> bool {
        false
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
