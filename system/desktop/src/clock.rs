//! Clock: the time (analog and digital), a stopwatch with laps, and a
//! countdown timer that notifies when it finishes.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{self, InputEvent, key};

use crate::apps::{Action, App, AppKind, Ctx};
use crate::font::Weight;
use crate::gfx::{Canvas, Rect, with_alpha};
use crate::ui;

pub const MONTHS: [&str; 12] = [
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
pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// Unit vector for a clock position (`step` out of 60, 0 at 12 o'clock),
/// from a table of sines for 0..=15 steps of 6 degrees.
fn dial(step: u32) -> (f32, f32) {
    const SIN: [f32; 16] = [
        0.0, 0.104_528, 0.207_912, 0.309_017, 0.406_737, 0.5, 0.587_785, 0.669_131, 0.743_145, 0.809_017, 0.866_025,
        0.913_545, 0.951_057, 0.978_148, 0.994_522, 1.0,
    ];
    let s = step % 60;
    let (q, r) = (s / 15, (s % 15) as usize);
    let (sin, cos) = (SIN[r], SIN[15 - r]);
    match q {
        0 => (sin, -cos),
        1 => (cos, sin),
        2 => (-sin, cos),
        _ => (-cos, -sin),
    }
}

fn now_ms() -> u64 {
    meridian_rt::uptime_us() / 1000
}

fn fmt_duration(ms: u64, hundredths: bool) -> String {
    let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    let base = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") };
    if hundredths { format!("{base}.{:02}", ms / 10 % 100) } else { base }
}

pub struct Clock {
    tab: usize,
    // Stopwatch.
    sw_start: Option<u64>,
    sw_banked: u64,
    laps: Vec<u64>,
    // Timer.
    timer_set: u64,
    timer_end: Option<u64>,
    timer_left: u64,
    timer_done: bool,
    notify: Vec<Action>,
    w: i32,
    h: i32,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            tab: 0,
            sw_start: None,
            sw_banked: 0,
            laps: Vec::new(),
            timer_set: 5 * 60_000,
            timer_end: None,
            timer_left: 5 * 60_000,
            timer_done: false,
            notify: Vec::new(),
            w: 560,
            h: 470,
        }
    }

    fn stopwatch(&self) -> u64 {
        self.sw_banked + self.sw_start.map_or(0, |s| now_ms() - s)
    }

    fn timer_remaining(&self) -> u64 {
        match self.timer_end {
            Some(end) => end.saturating_sub(now_ms()),
            None => self.timer_left,
        }
    }

    fn buttons(&self) -> Vec<(Rect, &'static str)> {
        let y = self.h - 64;
        let cx = self.w / 2;
        match self.tab {
            1 => vec![
                (Rect::new(cx - 160, y, 100, 40), if self.sw_start.is_some() { "Stop" } else { "Start" }),
                (Rect::new(cx - 50, y, 100, 40), "Lap"),
                (Rect::new(cx + 60, y, 100, 40), "Reset"),
            ],
            2 => vec![
                (Rect::new(cx - 160, y, 100, 40), if self.timer_end.is_some() { "Pause" } else { "Start" }),
                (Rect::new(cx - 50, y, 100, 40), "Reset"),
                (Rect::new(cx + 60, y, 46, 40), "-1"),
                (Rect::new(cx + 114, y, 46, 40), "+1"),
            ],
            _ => Vec::new(),
        }
    }

    fn press(&mut self, label: &str) {
        let now = now_ms();
        match (self.tab, label) {
            (1, "Start") => self.sw_start = Some(now),
            (1, "Stop") => {
                self.sw_banked = self.stopwatch();
                self.sw_start = None;
            }
            (1, "Lap") if self.sw_start.is_some() => {
                self.laps.insert(0, self.stopwatch());
                self.laps.truncate(50);
            }
            (1, "Reset") => {
                self.sw_start = None;
                self.sw_banked = 0;
                self.laps.clear();
            }
            (2, "Start") if self.timer_left > 0 => {
                self.timer_end = Some(now + self.timer_left);
                self.timer_done = false;
            }
            (2, "Pause") => {
                self.timer_left = self.timer_remaining();
                self.timer_end = None;
            }
            (2, "Reset") => {
                self.timer_end = None;
                self.timer_left = self.timer_set;
                self.timer_done = false;
            }
            (2, "-1" | "+1") if self.timer_end.is_none() => {
                let m = self.timer_set / 60_000;
                let m = if label == "+1" { (m + 1).min(180) } else { m.saturating_sub(1).max(1) };
                self.timer_set = m * 60_000;
                self.timer_left = self.timer_set;
                self.timer_done = false;
            }
            _ => {}
        }
    }
}

impl App for Clock {
    fn kind(&self) -> AppKind {
        AppKind::Clock
    }
    fn title(&self) -> String {
        "Clock".into()
    }
    fn size(&self) -> (i32, i32) {
        (560, 470)
    }
    fn min_size(&self) -> (i32, i32) {
        (440, 420)
    }
    fn interval(&self) -> u64 {
        if self.tab == 1 && self.sw_start.is_some() {
            50
        } else if self.timer_end.is_some() {
            250
        } else {
            1000
        }
    }
    fn tick(&mut self) -> bool {
        if let Some(end) = self.timer_end {
            if now_ms() >= end {
                self.timer_end = None;
                self.timer_left = 0;
                self.timer_done = true;
                let mins = self.timer_set / 60_000;
                self.notify.push(Action::Notify(format!("Timer finished: {mins} minute(s) are up")));
            }
        }
        true
    }
    fn pending(&mut self) -> Vec<Action> {
        core::mem::take(&mut self.notify)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        cv.fill(r, t.window());
        for (i, name) in ["Clock", "Stopwatch", "Timer"].iter().enumerate() {
            let tab = Rect::new(r.x + 12 + i as i32 * 116, r.y + 8, 108, 32);
            if self.tab == i {
                cv.fill_rounded(tab, 5, t.surface());
                cv.fill_rounded(Rect::new(tab.x + 34, tab.bottom() - 4, 40, 3), 1, t.accent);
            }
            let w = f.measure(name, 14, Weight::Regular);
            let weight = if self.tab == i { Weight::Semibold } else { Weight::Regular };
            f.draw(cv, tab.x + (tab.w - w) / 2, tab.y + 6, name, 14, weight, t.text());
        }
        let body = Rect::new(r.x, r.y + 48, r.w, r.h - 48);
        let (cxp, cyp) = (body.x + body.w / 2, body.y + 150);
        match self.tab {
            0 => {
                let now = sys::query_time();
                let (fx, fy, rad) = (cxp as f32, cyp as f32, 118.0);
                cv.circle(fx, fy, rad, t.surface());
                cv.ring(fx, fy, rad, 2.0, t.border());
                for i in 0..60 {
                    let (dx, dy) = dial(i);
                    let (inner, width) = if i % 5 == 0 { (rad - 14.0, 2.5) } else { (rad - 7.0, 1.0) };
                    cv.line(
                        fx + dx * inner,
                        fy + dy * inner,
                        fx + dx * (rad - 3.0),
                        fy + dy * (rad - 3.0),
                        width,
                        t.text_dim(),
                    );
                }
                let hour_step = (now.hour as u32 % 12) * 5 + now.minute as u32 / 12;
                for (step, len, width, color) in [
                    (hour_step, 60.0, 5.0, t.text()),
                    (now.minute as u32, 88.0, 3.5, t.text()),
                    (now.second as u32, 100.0, 1.5, t.accent),
                ] {
                    let (dx, dy) = dial(step);
                    cv.line(fx - dx * 12.0, fy - dy * 12.0, fx + dx * len, fy + dy * len, width, color);
                }
                cv.circle(fx, fy, 5.0, t.accent);
                let (h12, ampm) = match now.hour {
                    0 => (12, "AM"),
                    1..=11 => (now.hour, "AM"),
                    12 => (12, "PM"),
                    h => (h - 12, "PM"),
                };
                let time = format!("{h12}:{:02}:{:02} {ampm}", now.minute, now.second);
                let tw = f.measure(&time, 36, Weight::Semibold);
                f.draw(cv, cxp - tw / 2, body.y + 284, &time, 36, Weight::Semibold, t.text());
                let date = format!(
                    "{}, {} {}, {}",
                    WEEKDAYS.get(now.weekday as usize).copied().unwrap_or(""),
                    MONTHS.get(now.month as usize - 1).copied().unwrap_or(""),
                    now.day,
                    now.year
                );
                let dw = f.measure(&date, 16, Weight::Regular);
                f.draw(cv, cxp - dw / 2, body.y + 336, &date, 16, Weight::Regular, t.text_dim());
            }
            1 => {
                let text = fmt_duration(self.stopwatch(), true);
                let tw = f.measure(&text, 56, Weight::Semibold);
                f.draw(cv, cxp - tw / 2, body.y + 30, &text, 56, Weight::Semibold, t.text());
                let list = Rect::new(body.x + 40, body.y + 120, body.w - 80, body.h - 200);
                cv.rounded_box(list, 6, t.border(), t.surface());
                if self.laps.is_empty() {
                    f.draw(cv, list.x + 16, list.y + 14, "Laps appear here", 14, Weight::Regular, t.text_dim());
                }
                let n = self.laps.len();
                for (i, lap) in self.laps.iter().enumerate() {
                    let y = list.y + 10 + i as i32 * 26;
                    if y + 24 > list.bottom() {
                        break;
                    }
                    let split = lap - self.laps.get(i + 1).copied().unwrap_or(0);
                    f.draw(cv, list.x + 16, y, &format!("Lap {}", n - i), 14, Weight::Regular, t.text());
                    f.draw(cv, list.x + 140, y, &fmt_duration(split, true), 14, Weight::Regular, t.text_dim());
                    f.draw(cv, list.right() - 120, y, &fmt_duration(*lap, true), 14, Weight::Regular, t.text());
                }
            }
            _ => {
                let left = self.timer_remaining();
                let (fx, fy, rad) = (cxp as f32, (body.y + 140) as f32, 110.0);
                cv.ring(fx, fy, rad, 8.0, t.hover());
                // Progress: dots around the ring for the remaining fraction.
                let frac = (left * 60 / self.timer_set.max(1)) as u32;
                for i in 0..frac.min(60) {
                    let (dx, dy) = dial(i);
                    cv.circle(fx + dx * rad, fy + dy * rad, 5.0, t.accent);
                }
                let text = fmt_duration(left.div_ceil(1000) * 1000, false);
                let tw = f.measure(&text, 44, Weight::Semibold);
                f.draw(cv, cxp - tw / 2, body.y + 112, &text, 44, Weight::Semibold, t.text());
                let sub = if self.timer_done {
                    "Time's up!".into()
                } else {
                    format!("{} minute timer", self.timer_set / 60_000)
                };
                let sw = f.measure(&sub, 14, Weight::Regular);
                let color = if self.timer_done { t.accent } else { t.text_dim() };
                f.draw(cv, cxp - sw / 2, body.y + 170, &sub, 14, Weight::Regular, color);
                if self.timer_done {
                    cv.fill_rounded(Rect::new(body.x + 40, body.y + 270, body.w - 80, 6), 3, with_alpha(t.accent, 200));
                }
            }
        }
        for (b, label) in self.buttons() {
            let b = b.offset(r.x, r.y);
            ui::button(cv, f, &t, b, label, label == "Start", false);
        }
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if (8..40).contains(&y) && x >= 12 {
            let i = ((x - 12) / 116) as usize;
            if i < 3 {
                self.tab = i;
            }
            return Vec::new();
        }
        if let Some((_, label)) = self.buttons().into_iter().find(|(b, _)| b.contains(x, y)) {
            self.press(label);
        }
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        if e.key == key::CHAR && e.ch == ' ' as u32 {
            let first = self.buttons().first().map(|b| b.1);
            if let Some(label) = first {
                self.press(label);
            }
        }
        Vec::new()
    }
}
