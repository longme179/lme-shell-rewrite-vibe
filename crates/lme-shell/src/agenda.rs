// SPDX-License-Identifier: GPL-3.0-or-later
//! Agenda tab (Tier 2.7 — Agenda.qml + CalendarWidget.qml + TasksWidget.qml
//! + TimerWidget.qml + the FocusTimer singleton port). Three sub-tabs:
//! - the Calendar month grid,
//!   the To-do list (the frozen `tasks` preference, full CRUD),
//!   and the Timer (focus/stopwatch; daemon state so it keeps running
//!   while the panel closes, and resets on shell restart like the
//!   legacy singleton).
//!
//! UI state that must survive the 5 s in-place rebuilds lives in the SHELL
//! (`AgendaState`), like `control_tab`; the task entry text is re-read from
//! the live panel by every refresh (the rebuild itself syncs the draft
//! through `ControlCenterPanel::agenda_entry_text`).

use lme_ui::draw::DrawCmd;
use lme_ui::font_weight::{BOLD, MEDIUM, NORMAL};
use lme_ui::geom::{Point, Rect, Size};
use lme_ui::theme::Theme;
use lme_ui::widgets::{
    Button, Ctx, Fill, HBox, InputEvent, Label, Spacer, TextAlign, TextInput, VBox, Widget,
};
use serde_json::Value;

/// All agenda ids live in click_ids::AGENDA_PANEL [0x8200, 0x8500).
pub mod ids {
    /// Sub-tab row: +0 Calendar, +1 To do, +2 Timer.
    pub const SUBTAB_BASE: u64 = 0x8201;
    pub const CAL_PREV: u64 = 0x8210;
    pub const CAL_NEXT: u64 = 0x8211;
    pub const CAL_TODAY: u64 = 0x8212;
    /// The task entry input (Focus/Submit events; keyboard focus target).
    pub const TASK_ENTRY: u64 = 0x8220;
    pub const TASK_ADD: u64 = 0x8221;
    pub const TASK_CLEAR: u64 = 0x8222;
    /// Timer mode: +0 Focus 25 min, +1 Short 5 min, +2 Long 15 min.
    pub const TIMER_MODE_BASE: u64 = 0x8230;
    pub const TIMER_TOGGLE: u64 = 0x8240;
    pub const TIMER_RESET: u64 = 0x8241;
    /// Stopwatch switch row: +0 Focus timer, +1 Stopwatch.
    pub const TIMER_SW_BASE: u64 = 0x8242;
    /// Per-task buttons: + i*4 + {0 check, 1 delete}; i < 100 (the frozen
    /// `tasks` cap) — 0x8300..0x8490, inside the range with headroom.
    pub const TASK_ROW_BASE: u64 = 0x8300;

    pub fn subtab(index: usize) -> u64 {
        SUBTAB_BASE + index as u64
    }
    pub fn timer_mode(index: usize) -> u64 {
        TIMER_MODE_BASE + index as u64
    }
    pub fn task_check(index: usize) -> u64 {
        TASK_ROW_BASE + index as u64 * 4
    }
    pub fn task_delete(index: usize) -> u64 {
        TASK_ROW_BASE + index as u64 * 4 + 1
    }
}

// ---------------------------------------------------------------------------
// Pure date math (unit-tested; mirrors the QML Date calls)
// ---------------------------------------------------------------------------

/// Days in `month` of `year` (leap aware).
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

pub fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// days_from_civil (Hinnant) for the (year, month, day); 1970-01-01 = 0.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 {
        year as i64 - 1
    } else {
        year as i64
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if month > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0-based Monday-first weekday (legacy `(getDay() + 6) % 7`): day 0 of
/// the epoch was a Thursday = Mon-index 3.
pub fn weekday_mon(year: i32, month: u32, day: u32) -> usize {
    (days_from_civil(year, month, day) + 3).rem_euclid(7) as usize
}

/// Monday-first weekday of the 1st of the month.
pub fn first_weekday_mon(year: i32, month: u32) -> u32 {
    weekday_mon(year, month, 1) as u32
}

pub fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("")
}

const WEEKDAYS_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const WEEKDAYS_FULL: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// "dddd, MMMM d, yyyy" for the today line.
pub fn full_date_line(year: i32, month: u32, day: u32) -> String {
    format!(
        "{}, {} {}, {}",
        WEEKDAYS_FULL[weekday_mon(year, month, day)],
        month_name(month),
        day,
        year
    )
}

// ---------------------------------------------------------------------------
// Daemon-side state
// ---------------------------------------------------------------------------

/// Agenda UI state (survives the 5 s refreshes; lives in the Shell like
/// `control_tab`). The calendar defaults to the current month at startup.
#[derive(Clone)]
pub struct AgendaState {
    pub subtab: usize,
    pub cal_year: i32,
    pub cal_month: u32, // 1..=12
    /// Task entry draft — re-read from the live panel by every refresh.
    pub draft: String,
    /// Timer tab: focus (false) or stopwatch (true) view.
    pub sw_view: bool,
}

impl AgendaState {
    pub fn new(now: (i32, u32, u32)) -> Self {
        Self {
            subtab: 0,
            cal_year: now.0,
            cal_month: now.1.clamp(1, 12),
            draft: String::new(),
            sw_view: false,
        }
    }

    pub fn cal_prev_month(&mut self) {
        self.cal_month -= 1;
        if self.cal_month == 0 {
            self.cal_month = 12;
            self.cal_year -= 1;
        }
    }

    pub fn cal_next_month(&mut self) {
        self.cal_month += 1;
        if self.cal_month > 12 {
            self.cal_month = 1;
            self.cal_year += 1;
        }
    }
}

/// One frozen `tasks` entry (validated by the backend prefs on write).
#[derive(Clone)]
pub struct TaskItem {
    pub id: String,
    pub text: String,
    pub done: bool,
}

pub fn tasks_from_settings(values: &serde_json::Map<String, Value>) -> Vec<TaskItem> {
    values
        .get("tasks")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(TaskItem {
                        id: row.get("id")?.as_str()?.to_string(),
                        text: row.get("text")?.as_str()?.to_string(),
                        done: row.get("done")?.as_bool()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Serialize the task list back into the preference value shape.
pub fn tasks_to_value(tasks: &[TaskItem]) -> Value {
    Value::Array(
        tasks
            .iter()
            .map(|task| {
                serde_json::json!({
                    "id": task.id,
                    "text": task.text,
                    "done": task.done,
                })
            })
            .collect(),
    )
}

/// New unique task id (ascii + '-' only, ≤64 chars — the validator rules).
pub fn new_task_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("t{now}")
}

// ---------------------------------------------------------------------------
// FocusTimer (the QML singleton port; daemon state so it keeps running
// while the panel closes — resets on shell restart)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FocusTimer {
    pub mode: usize, // 0 focus 25 min, 1 short 5 min, 2 long 15 min
    pub remaining: u64,
    pub running: bool,
    pub deadline: Option<std::time::Instant>,
    pub sw_running: bool,
    pub sw_saved: u64,
    pub sw_started: Option<std::time::Instant>,
    pub sw_elapsed: u64,
}

impl Default for FocusTimer {
    fn default() -> Self {
        Self {
            mode: 0,
            remaining: Self::durations()[0],
            running: false,
            deadline: None,
            sw_running: false,
            sw_saved: 0,
            sw_started: None,
            sw_elapsed: 0,
        }
    }
}

impl FocusTimer {
    pub const fn durations() -> [u64; 3] {
        [25 * 60, 5 * 60, 15 * 60]
    }

    pub fn select_mode(&mut self, mode: usize) {
        self.running = false;
        self.deadline = None;
        self.mode = mode.min(2);
        self.remaining = Self::durations()[self.mode];
    }

    pub fn start(&mut self) {
        if self.remaining == 0 {
            self.remaining = Self::durations()[self.mode];
        }
        self.deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(self.remaining));
        self.running = true;
    }

    pub fn pause(&mut self) {
        if let Some(deadline) = self.deadline {
            // legacy parity: Math.ceil((deadline - now) / 1000) — a pause
            // 1.2 s in shows 1499, not 1498
            self.remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_secs_f64()
                .ceil() as u64;
        }
        self.running = false;
        self.deadline = None;
    }

    pub fn reset(&mut self) {
        self.running = false;
        self.deadline = None;
        self.remaining = Self::durations()[self.mode];
    }

    pub fn sw_toggle(&mut self) {
        if self.sw_running {
            self.sw_saved = self.sw_elapsed;
            self.sw_running = false;
            self.sw_started = None;
        } else {
            self.sw_started = Some(std::time::Instant::now());
            self.sw_running = true;
        }
    }

    pub fn sw_reset(&mut self) {
        self.sw_running = false;
        self.sw_started = None;
        self.sw_saved = 0;
        self.sw_elapsed = 0;
    }

    pub fn sw_display(&self) -> u64 {
        self.sw_saved
            + self
                .sw_started
                .map(|started| started.elapsed().as_secs())
                .unwrap_or(0)
    }

    pub fn is_active(&self) -> bool {
        self.running || self.sw_running
    }

    /// Recompute the displays. Returns true exactly once when the focus
    /// timer hits zero (the caller fires the local notification).
    pub fn tick(&mut self) -> bool {
        let mut completed = false;
        if self.running {
            if let Some(deadline) = self.deadline {
                let left = deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_secs_f64()
                    .ceil() as u64;
                self.remaining = left;
                if left == 0 {
                    self.running = false;
                    self.deadline = None;
                    completed = true;
                }
            }
        }
        if self.sw_running {
            self.sw_elapsed = self.sw_display();
        }
        completed
    }
}

/// "mm:ss" (the QML format: minutes zero-padded, not capped at 99).
pub fn format_mmss(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

// ---------------------------------------------------------------------------
// The tab widget
// ---------------------------------------------------------------------------

/// Everything the Agenda tab renders (owned snapshot, rebuilt from
/// `ControlData` like the Controls tab).
pub struct AgendaData {
    pub today: (i32, u32, u32),
    pub subtab: usize,
    pub cal: (i32, u32),
    pub tasks: Vec<TaskItem>,
    pub draft: String,
    pub sw_view: bool,
    pub timer: FocusTimer,
}

/// The Agenda tab content: sub-tab row + the active sub-tab. A custom
/// widget so the task entry text can be read back by the daemon (the
/// refreshes sync the draft through `entry_text` — the path walk is
/// unit-locked by entry_text_follows_typing).
pub struct AgendaTab {
    subtabs: Box<dyn Widget>,
    content: Box<dyn Widget>,
    rect: Rect,
}

impl AgendaTab {
    pub fn new(theme: &Theme, d: &AgendaData) -> Self {
        let t = theme;
        let mut subtabs = HBox::new(8.0);
        for (index, label) in ["Calendar", "To do", "Timer"].iter().enumerate() {
            let active = index == d.subtab;
            let mut b = Button::new(ids::subtab(index), *label);
            b.label_size = 13.0;
            b.label_weight = if active { BOLD } else { MEDIUM };
            b.fill = if active { t.purple } else { t.surface };
            b.hover_fill = Some(if active {
                t.purple
            } else {
                t.surface_container_high
            });
            b.text_color = if active { t.background } else { t.text };
            b.radius = t.small_radius;
            b.border = Some((t.border, 1.0));
            b.min_h = 32.0;
            subtabs = subtabs.push(Fill::new(Box::new(b)));
        }
        let content: Box<dyn Widget> = match d.subtab {
            0 => Box::new(calendar_tab(t, d)),
            1 => tasks_tab(t, d),
            _ => Box::new(timer_tab(t, d)),
        };
        Self {
            subtabs: Box::new(subtabs),
            content,
            rect: Rect::default(),
        }
    }

    /// The live task entry text ("" off the tasks sub-tab). Path walk into
    /// the composite — the levels are fixed by this module's own builder:
    /// content VBox -> children[1] (entry row HBox) -> children[0] (Fill) ->
    /// the TextInput. as_any needs &mut, so the walk is &mut.
    pub fn entry_text(&mut self) -> String {
        self.content
            .as_any()
            .downcast_mut::<VBox>()
            .and_then(|tasks| tasks.children.get_mut(1))
            .and_then(|row| row.as_any().downcast_mut::<HBox>())
            .and_then(|row| row.children.first_mut())
            .and_then(|fill| fill.as_any().downcast_mut::<Fill>())
            .and_then(|fill| fill.child.as_any().downcast_mut::<TextInput>())
            .map(|input| input.text.clone())
            .unwrap_or_default()
    }
}

impl Widget for AgendaTab {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        let sub = self.subtabs.measure(ctx);
        let body = self.content.measure(ctx);
        Size::new(sub.w.max(body.w), sub.h + 14.0 + body.h)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        let top_h = self.subtabs.measure(ctx).h.min(bounds.h);
        self.subtabs
            .layout(ctx, Rect::new(bounds.x, bounds.y, bounds.w, top_h));
        self.content.layout(
            ctx,
            Rect::new(
                bounds.x,
                bounds.y + top_h + 14.0,
                bounds.w,
                (bounds.h - top_h - 14.0).max(0.0),
            ),
        );
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        self.subtabs.paint(ctx, out);
        self.content.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        let hit_sub = self.subtabs.deliver(ctx, ev, target);
        let hit_body = self.content.deliver(ctx, ev, target);
        hit_sub || hit_body
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.subtabs.focusables(out);
        self.content.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Small secondary-button (shared look with the control-center buttons).
fn subtab_button(t: &Theme, id: u64, label: &str, active: bool) -> Button {
    let mut b = Button::new(id, label);
    b.label_size = 12.0;
    b.label_weight = MEDIUM;
    b.fill = if active {
        t.purple
    } else {
        t.panel_fill.alpha(0.55)
    };
    b.hover_fill = Some(if active { t.purple } else { t.panel_fill });
    b.text_color = if active { t.background } else { t.text };
    b.radius = t.small_radius;
    b.border = Some((t.border, 1.0));
    b.min_h = 30.0;
    b
}

/// One calendar day cell (rounded rect + centered number; today = accent).
struct DayCell {
    day: u32,
    today: bool,
    valid: bool,
    rect: Rect,
}

impl Widget for DayCell {
    fn measure(&mut self, _ctx: &mut Ctx) -> Size {
        Size::new(48.0, 48.0)
    }
    fn layout(&mut self, _ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        if !self.valid {
            return; // transparent placeholder (legacy: color "transparent")
        }
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: 14.0,
            fill: Some(if self.today {
                ctx.theme.purple
            } else {
                ctx.theme.surface
            }),
            border: None,
            clip: Some(self.rect),
        });
        let text = self.day.to_string();
        let line = ctx.fonts.line(&text, 14.0, BOLD);
        out.push(DrawCmd::Text {
            pos: Point::new(
                self.rect.x + (self.rect.w - line.width) / 2.0,
                self.rect.y + (self.rect.h - line.height()) / 2.0,
            ),
            line,
            color: if self.today {
                ctx.theme.background
            } else {
                ctx.theme.text
            },
            clip: Some(self.rect),
        });
    }
    fn deliver(&mut self, _ctx: &mut Ctx, _ev: &InputEvent, _target: Option<u64>) -> bool {
        false
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn calendar_tab(t: &Theme, d: &AgendaData) -> VBox {
    let mut body = VBox::new(14.0).padded(8.0);
    // month nav row: ‹ | centered "October 2026" | › (CalendarWidget.qml)
    let mut title = Label::styled(
        format!("{} {}", month_name(d.cal.1), d.cal.0),
        21.0,
        BOLD,
        t.text,
    );
    title.align = TextAlign::Center;
    body = body.push(
        HBox::new(8.0)
            .push(subtab_button(t, ids::CAL_PREV, "‹", false))
            .push(Fill::new(Box::new(title)))
            .push(subtab_button(t, ids::CAL_NEXT, "›", false)),
    );
    // weekday header, Monday-first
    let mut header = HBox::new(6.0);
    for name in WEEKDAYS_SHORT {
        let mut l = Label::styled(name, 12.0, NORMAL, t.muted);
        l.align = TextAlign::Center;
        header = header.push(Fill::new(Box::new(l)));
    }
    body = body.push(header);
    // 6 rows x 7 cells (42 slots; day = index - first + 1)
    let first = first_weekday_mon(d.cal.0, d.cal.1);
    let dim = days_in_month(d.cal.0, d.cal.1);
    let is_this_month = d.cal.0 == d.today.0 && d.cal.1 == d.today.1;
    for row in 0..6 {
        let mut days = HBox::new(6.0);
        for col in 0..7 {
            let index = row * 7 + col;
            let day = index as i64 - first as i64 + 1;
            let valid = (1..=dim as i64).contains(&day);
            let today = valid && is_this_month && day == d.today.2 as i64;
            days = days.push(Fill::new(Box::new(DayCell {
                day: day.max(0) as u32,
                today,
                valid,
                rect: Rect::default(),
            })));
        }
        body = body.push(days);
    }
    // Today button + full date line
    body = body.push(
        HBox::new(8.0)
            .push(Spacer::fill())
            .push(subtab_button(t, ids::CAL_TODAY, "Today", false))
            .push(Spacer::fill()),
    );
    let mut line = Label::styled(
        full_date_line(d.today.0, d.today.1, d.today.2),
        12.0,
        NORMAL,
        t.muted,
    );
    line.align = TextAlign::Center;
    body = body.push(line);
    body
}

/// Tasks tab. The entry input sits at children[1].children[0] of the
/// returned VBox — `AgendaTab::entry_text` walks that exact path.
fn tasks_tab(t: &Theme, d: &AgendaData) -> Box<dyn Widget> {
    let remaining = d.tasks.iter().filter(|task| !task.done).count();
    let mut body = VBox::new(12.0).padded(8.0);
    body = body.push(Label::styled(
        "A little progress every day.",
        19.0,
        BOLD,
        t.text,
    ));
    // entry row: input (Fill) + Add — children[1] of the body
    let mut input = TextInput::new(ids::TASK_ENTRY, "Add a task…");
    input.text = d.draft.clone();
    input.cursor = d.draft.chars().count();
    input.size = 13.0;
    input.radius = 12.0;
    body = body.push(
        HBox::new(8.0)
            .push(Fill::new(Box::new(input)))
            .push(subtab_button(t, ids::TASK_ADD, "Add", false)),
    );
    body = body.push(Label::styled(
        format!("{remaining} remaining"),
        12.0,
        NORMAL,
        t.muted,
    ));
    // task rows: check + text + delete (strikeout approximated by the
    // muted color — the renderer has no strikeout; noted with the styling)
    for (index, task) in d.tasks.iter().enumerate() {
        let mut row = HBox::new(10.0);
        let mut check = subtab_button(
            t,
            ids::task_check(index),
            if task.done { "✓" } else { "·" },
            task.done,
        );
        check.min_w = 34.0;
        row = row.push(check);
        row = row.push(Fill::new(Box::new(Label::styled(
            &task.text,
            13.0,
            if task.done { NORMAL } else { MEDIUM },
            if task.done { t.muted } else { t.text },
        ))));
        row = row.push(subtab_button(t, ids::task_delete(index), "×", false));
        body = body.push(Fill::new(Box::new(Card::new(Box::new(row.padded(10.0))))));
    }
    // footer: Clear completed (right-aligned like the QML row)
    body = body.push(HBox::new(8.0).push(Spacer::fill()).push(subtab_button(
        t,
        ids::TASK_CLEAR,
        "Clear completed",
        d.tasks.iter().any(|task| task.done),
    )));
    body = body.push(Label::styled(
        "Tasks are saved with your shell preferences.",
        12.0,
        NORMAL,
        t.muted,
    ));
    Box::new(body)
}

/// The local Card wrapper (same look as control_center::Card; kept local
/// so this module stays self-contained).
struct Card {
    body: Box<dyn Widget>,
    rect: Rect,
}

impl Card {
    fn new(body: Box<dyn Widget>) -> Self {
        Self {
            body,
            rect: Rect::default(),
        }
    }
}

impl Widget for Card {
    fn measure(&mut self, ctx: &mut Ctx) -> Size {
        self.body.measure(ctx)
    }
    fn layout(&mut self, ctx: &mut Ctx, bounds: Rect) {
        self.rect = bounds;
        self.body.layout(ctx, bounds);
    }
    fn paint(&mut self, ctx: &mut Ctx, out: &mut Vec<DrawCmd>) {
        out.push(DrawCmd::RoundedRect {
            rect: self.rect,
            radius: ctx.theme.small_radius,
            fill: Some(ctx.theme.surface),
            border: Some((ctx.theme.border, 1.0)),
            clip: Some(self.rect),
        });
        self.body.paint(ctx, out);
    }
    fn deliver(&mut self, ctx: &mut Ctx, ev: &InputEvent, target: Option<u64>) -> bool {
        self.body.deliver(ctx, ev, target)
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn focusables(&self, out: &mut Vec<u64>) {
        self.body.focusables(out);
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn timer_tab(t: &Theme, d: &AgendaData) -> VBox {
    let mut body = VBox::new(18.0).padded(8.0);
    // focus/stopwatch switch row
    let mut switch = HBox::new(8.0);
    for (index, label) in ["Focus timer", "Stopwatch"].iter().enumerate() {
        switch = switch.push(Fill::new(Box::new(subtab_button(
            t,
            ids::TIMER_SW_BASE + index as u64,
            label,
            (index == 1) == d.sw_view,
        ))));
    }
    body = body.push(switch);
    if !d.sw_view {
        // mode row (legacy combo → three buttons)
        let modes = ["Focus · 25 min", "Short · 5 min", "Long · 15 min"];
        let mut mode_row = HBox::new(6.0);
        for (index, label) in modes.iter().enumerate() {
            mode_row = mode_row.push(Fill::new(Box::new(subtab_button(
                t,
                ids::timer_mode(index),
                label,
                d.timer.mode == index,
            ))));
        }
        body = body.push(mode_row);
    }
    // the big readout (64 px; the family is proportional — the legacy used
    // a mono family, noted with the styling pass)
    let seconds = if d.sw_view {
        d.timer.sw_display()
    } else {
        d.timer.remaining
    };
    let mut readout = Label::styled(
        format_mmss(seconds),
        64.0,
        BOLD,
        if d.sw_view { t.cyan } else { t.purple },
    );
    readout.align = TextAlign::Center;
    body = body.push(readout);
    let caption = if d.sw_view {
        "Measure your time."
    } else if d.timer.running {
        "One thing at a time."
    } else {
        "Make room for focused work."
    };
    let mut cap = Label::styled(caption, 12.0, NORMAL, t.muted);
    cap.align = TextAlign::Center;
    body = body.push(cap);
    // controls
    let running = if d.sw_view {
        d.timer.sw_running
    } else {
        d.timer.running
    };
    body = body.push(
        HBox::new(8.0)
            .push(Spacer::fill())
            .push(subtab_button(
                t,
                ids::TIMER_TOGGLE,
                if running { "Pause" } else { "Start" },
                true,
            ))
            .push(subtab_button(t, ids::TIMER_RESET, "Reset", false))
            .push(Spacer::fill()),
    );
    body = body.push(Label::styled(
        "The timer keeps running when the panel closes. It resets when the shell restarts.",
        12.0,
        NORMAL,
        t.muted,
    ));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_center::{ControlCenterPanel, ControlData};
    use lme_ui::widgets::UiEvent;
    use lme_ui::widgets::XKB_BACKSPACE;

    /// The shared panel test kit lives in control_center::tests; a minimal
    /// local copy keeps this module's tests self-contained.
    struct Kit {
        theme: Theme,
        fonts: lme_ui::text::FontBook,
        anims: lme_ui::anim::Animator,
        shadows: lme_ui::shadow::ShadowCache,
        queue: Vec<UiEvent>,
    }

    impl Kit {
        fn new() -> Self {
            Self {
                theme: Theme::default_with(&lme_ui::theme::Palette::default()),
                fonts: lme_ui::text::FontBook::new(),
                anims: lme_ui::anim::Animator::new(),
                shadows: lme_ui::shadow::ShadowCache::new(),
                queue: Vec::new(),
            }
        }
        fn ctx(&mut self) -> Ctx<'_> {
            Ctx {
                theme: &mut self.theme,
                fonts: &mut self.fonts,
                anims: &mut self.anims,
                shadows: &mut self.shadows,
                queue: &mut self.queue,
                now: std::time::Instant::now(),
                focused: None,
                repaint: false,
            }
        }
        fn layout_panel(&mut self, panel: &mut ControlCenterPanel) {
            let mut c = self.ctx();
            panel.layout(
                &mut c,
                lme_ui::geom::Rect::new(0.0, 0.0, crate::control_center::PANEL_W, 740.0),
            );
        }
    }

    fn agenda_data() -> AgendaData {
        AgendaData {
            today: (2026, 10, 13),
            subtab: 1,
            cal: (2026, 10),
            tasks: vec![TaskItem {
                id: "t1".into(),
                text: "Ship the agenda tab".into(),
                done: false,
            }],
            draft: String::new(),
            sw_view: false,
            timer: FocusTimer::default(),
        }
    }

    #[test]
    fn calendar_math_matches_the_calendar() {
        // October 2026: Oct 1 2026 is a Thursday → Mon-index 3
        assert_eq!(first_weekday_mon(2026, 10), 3);
        assert_eq!(days_in_month(2026, 10), 31);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29); // leap
                                                // 2026-10-13 is a Tuesday (verified against `date`)
        assert_eq!(full_date_line(2026, 10, 13), "Tuesday, October 13, 2026");
        // Jan 1 1970 was a Thursday
        assert_eq!(first_weekday_mon(1970, 1), 3);
        assert_eq!(full_date_line(1970, 1, 1), "Thursday, January 1, 1970");
        // month rollover helpers (verified against the calendar)
        let mut a = AgendaState::new((2026, 10, 13));
        a.cal_prev_month();
        assert_eq!((a.cal_year, a.cal_month), (2026, 9));
        a.cal_prev_month(); // Aug
        a.cal_prev_month(); // Jul
        assert_eq!((a.cal_year, a.cal_month), (2026, 7));
        // cross a year boundary: Jan 2026 → Dec 2025
        let mut b = AgendaState::new((2026, 1, 1));
        b.cal_prev_month();
        assert_eq!((b.cal_year, b.cal_month), (2025, 12));
        b.cal_next_month();
        b.cal_next_month();
        assert_eq!((b.cal_year, b.cal_month), (2026, 2));
    }

    #[test]
    fn focus_timer_transitions() {
        let mut ft = FocusTimer::default();
        assert_eq!(ft.remaining, 1500);
        ft.select_mode(1);
        assert_eq!(ft.remaining, 300);
        assert!(!ft.running);
        ft.start();
        assert!(ft.running);
        ft.pause();
        assert!(!ft.running);
        assert!((298..=300).contains(&ft.remaining));
        ft.select_mode(0);
        assert_eq!(ft.remaining, 1500);
        // tick with an already-past deadline completes exactly once
        ft.start();
        ft.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(ft.tick());
        assert!(!ft.running);
        assert_eq!(ft.remaining, 0);
        assert!(!ft.tick(), "completion fires exactly once");
        // stopwatch
        ft.sw_toggle();
        assert!(ft.sw_running);
        ft.sw_toggle();
        assert!(!ft.sw_running);
        assert_eq!(ft.sw_display(), 0);
        ft.sw_reset();
        assert_eq!((ft.sw_saved, ft.sw_elapsed), (0, 0));
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(65), "01:05");
        assert_eq!(format_mmss(1500), "25:00");
        assert_eq!(format_mmss(3661), "61:01"); // minutes not capped, like the QML
    }

    #[test]
    fn task_ids_stay_inside_the_range_and_stride() {
        for i in 0..100 {
            assert!(crate::click_ids::AGENDA_PANEL.contains(ids::task_check(i)));
            assert!(crate::click_ids::AGENDA_PANEL.contains(ids::task_delete(i)));
            assert_eq!(ids::task_delete(i) - ids::task_check(i), 1);
        }
        // adjacent rows never alias
        assert_eq!(ids::task_check(1) - ids::task_check(0), 4);
        for id in [
            ids::subtab(0),
            ids::subtab(2),
            ids::CAL_PREV,
            ids::CAL_NEXT,
            ids::CAL_TODAY,
            ids::TASK_ENTRY,
            ids::TASK_ADD,
            ids::TASK_CLEAR,
            ids::timer_mode(0),
            ids::timer_mode(2),
            ids::TIMER_TOGGLE,
            ids::TIMER_RESET,
            ids::TIMER_SW_BASE,
            ids::TIMER_SW_BASE + 1,
        ] {
            assert!(
                crate::click_ids::AGENDA_PANEL.contains(id),
                "id {id:#x} outside AGENDA_PANEL"
            );
        }
    }

    #[test]
    fn task_id_shape_passes_the_validator() {
        let id = new_task_id();
        assert!(!id.is_empty() && id.len() <= 64);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    /// E2E coordinate locks (panel-local; the e2e specs add the (666,10)
    /// window offset). Scans by real hit-testing like the control-center
    /// position tests.
    fn scan_find_id(
        kit: &mut Kit,
        panel: &mut ControlCenterPanel,
        want: u64,
    ) -> Option<(f32, f32)> {
        let mut y = 4.0f32;
        while y < 736.0 {
            let mut x = 4.0f32;
            while x < 596.0 {
                let p = lme_ui::geom::Point::new(x, y);
                let before = kit.queue.len();
                {
                    let mut c = kit.ctx();
                    panel.deliver(&mut c, &InputEvent::down(p), None);
                    panel.deliver(&mut c, &InputEvent::up(p), None);
                }
                if kit.queue[before..]
                    .iter()
                    .any(|e| matches!(e, UiEvent::Click(id) if *id == want))
                {
                    return Some((x, y));
                }
                x += 4.0;
            }
            y += 4.0;
        }
        None
    }

    #[test]
    fn agenda_positions_for_e2e() {
        let mut kit = Kit::new();
        let mut panel =
            ControlCenterPanel::new(&kit.theme, &agenda_control_data(1, "ship", true), 3);
        kit.layout_panel(&mut panel);
        // first: the Agenda MAIN tab button lives in the top grid — a
        // second panel instance is irrelevant; scan this one (tab grid is
        // always rendered by ControlCenterPanel::rebuild)
        let (t3_x, t3_y) = scan_find_id(&mut kit, &mut panel, crate::control_center::ids::tab(3))
            .expect("agenda main tab never hit");
        eprintln!("E2E_AGENDA_MAIN_TAB_PANEL_POS={t3_x},{t3_y}");
        // tab 3 = the agenda; subtab 1 = tasks (with one task + draft)
        // entry input: text clicks are not Click events (Focus only) — lock
        // by rect scan for TASK_ADD instead and place the click INSIDE the
        // input using the layout before it
        let (add_x, add_y) =
            scan_find_id(&mut kit, &mut panel, ids::TASK_ADD).expect("Add button never hit");
        eprintln!("E2E_AGENDA_ADD_PANEL_POS={add_x},{add_y}");
        // the entry input sits left of Add in the same row
        eprintln!("E2E_AGENDA_ENTRY_ROW={}", add_y);
        // task check + delete for row 0
        let (chk_x, chk_y) =
            scan_find_id(&mut kit, &mut panel, ids::task_check(0)).expect("task check never hit");
        eprintln!("E2E_AGENDA_TASK0_CHECK_PANEL_POS={chk_x},{chk_y}");
        let (del_x, del_y) =
            scan_find_id(&mut kit, &mut panel, ids::task_delete(0)).expect("task delete never hit");
        eprintln!("E2E_AGENDA_TASK0_DELETE_PANEL_POS={del_x},{del_y}");
        // sub-tab + timer buttons on the timer subtab
        let mut panel2 = ControlCenterPanel::new(&kit.theme, &agenda_control_data(2, "", false), 3);
        kit.layout_panel(&mut panel2);
        let (start_x, start_y) =
            scan_find_id(&mut kit, &mut panel2, ids::TIMER_TOGGLE).expect("timer toggle never hit");
        eprintln!("E2E_AGENDA_TIMER_TOGGLE_PANEL_POS={start_x},{start_y}");
        let (cal_x, cal_y) =
            scan_find_id(&mut kit, &mut panel2, ids::subtab(0)).expect("calendar subtab never hit");
        eprintln!("E2E_AGENDA_SUBTAB_CALENDAR_PANEL_POS={cal_x},{cal_y}");
    }

    /// ControlData for the agenda e2e coordinate test (tab 3).
    fn agenda_control_data(subtab: usize, draft: &str, with_task: bool) -> ControlData {
        let mut d = crate::control_center::tests::data();
        d.now_ymd = (2026, 10, 13);
        d.agenda_subtab = subtab;
        d.cal = (2026, 10);
        d.agenda_draft = draft.to_string();
        if with_task {
            d.tasks = vec![TaskItem {
                id: "t1".into(),
                text: "e2e task".into(),
                done: false,
            }];
        }
        d
    }

    /// The entry-text walk: type through the REAL delivery path (Key events
    /// targeted at TASK_ENTRY), then read the text back through
    /// ControlCenterPanel::agenda_entry_text — the daemon's draft sync.
    #[test]
    fn entry_text_follows_typing() {
        let mut kit = Kit::new();
        let mut d = agenda_data();
        d.subtab = 1;
        let mut panel = ControlCenterPanel::new(
            &kit.theme,
            &ControlData {
                now_hm: "12:34".into(),
                now_date: "Tue, Oct 13".into(),
                dnd: false,
                animations: true,
                stay_awake: false,
                night_light: false,
                night_light_temp: 4500,
                wifi: None,
                bluetooth: None,
                audio: None,
                brightness: None,
                power_profile: None,
                media: None,
                cpu: None,
                ram: None,
                battery_percent: None,
                battery_charging: false,
                weather: None,
                now_ymd: (2026, 10, 13),
                agenda_subtab: 1,
                cal: (2026, 10),
                tasks: d.tasks.clone(),
                agenda_draft: String::new(),
                sw_view: false,
                timer: FocusTimer::default(),
            },
            3,
        );
        kit.layout_panel(&mut panel);
        // focus the entry (the window layer would set this on the Focus
        // event; the test sets it directly like the launcher tests do)
        // type "hi" through the targeted delivery path
        let key = |utf8: &str| lme_ui::widgets::KeyInput {
            sym: 0,
            utf8: utf8.to_string(),
            shift: false,
            ctrl: false,
        };
        {
            let mut c = kit.ctx();
            c.focused = Some(ids::TASK_ENTRY);
            panel.deliver(&mut c, &InputEvent::Key(key("h")), Some(ids::TASK_ENTRY));
        }
        {
            let mut c = kit.ctx();
            c.focused = Some(ids::TASK_ENTRY);
            panel.deliver(&mut c, &InputEvent::Key(key("i")), Some(ids::TASK_ENTRY));
        }
        // backspace once, then read back
        {
            let mut c = kit.ctx();
            c.focused = Some(ids::TASK_ENTRY);
            panel.deliver(
                &mut c,
                &InputEvent::Key(lme_ui::widgets::KeyInput {
                    sym: XKB_BACKSPACE,
                    utf8: String::new(),
                    shift: false,
                    ctrl: false,
                }),
                Some(ids::TASK_ENTRY),
            );
        }
        assert_eq!(
            panel.agenda_entry_text(),
            Some("h".to_string()),
            "the draft must read back through the walk"
        );
    }
}
