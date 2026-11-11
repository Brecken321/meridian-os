//! Calculator: a standard calculator (keyboard or mouse) with a history
//! of results.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{InputEvent, key};

use crate::apps::{Action, App, AppKind, Ctx, MenuItem, ctrl};
use crate::font::Weight;
use crate::gfx::{Canvas, Rect, rgb, shade};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "-",
            Op::Mul => "\u{00D7}",
            Op::Div => "\u{00F7}",
        }
    }
}

const KEYS: [[&str; 4]; 6] = [
    ["%", "CE", "C", "\u{232B}"],
    ["1/x", "x\u{00B2}", "\u{221A}x", "\u{00F7}"],
    ["7", "8", "9", "\u{00D7}"],
    ["4", "5", "6", "-"],
    ["1", "2", "3", "+"],
    ["\u{00B1}", "0", ".", "="],
];

pub struct Calculator {
    display: String,
    /// Left operand and pending operator.
    pending: Option<(f64, Op)>,
    expression: String,
    /// The display holds a result: the next digit starts a new number.
    fresh: bool,
    last: Option<(Op, f64)>,
    error: bool,
    history: Vec<(String, String)>,
    w: i32,
    h: i32,
}

fn sqrt(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut g = if x > 1.0 { x / 2.0 } else { 1.0 };
    for _ in 0..80 {
        let n = 0.5 * (g + x / g);
        if n == g {
            break;
        }
        g = n;
    }
    g
}

fn abs(x: f64) -> f64 {
    if x < 0.0 { -x } else { x }
}

/// Formats like a pocket calculator: up to 15 significant digits, no
/// trailing zeros, scientific notation for very large or small values.
pub fn format_number(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        return "Error".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    let a = abs(v);
    if !(1e-9..1e16).contains(&a) {
        let s = format!("{v:.9e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let m = m.trim_end_matches('0').trim_end_matches('.');
        return format!("{m}e{e}");
    }
    let mut digits = 0;
    let mut t = a;
    while t >= 1.0 {
        t /= 10.0;
        digits += 1;
    }
    let decimals = (15 - digits).max(0) as usize;
    let s = format!("{v:.decimals$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// Adds thousands separators to the integer part.
fn grouped(s: &str) -> String {
    if s.contains('e') || s == "Error" {
        return s.to_string();
    }
    let (sign, rest) = if let Some(r) = s.strip_prefix('-') { ("-", r) } else { ("", s) };
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let mut r = format!("{sign}{out}");
    if let Some(f) = frac {
        r.push('.');
        r.push_str(f);
    }
    r
}

impl Calculator {
    pub fn new() -> Self {
        Self {
            display: "0".into(),
            pending: None,
            expression: String::new(),
            fresh: true,
            last: None,
            error: false,
            history: Vec::new(),
            w: 560,
            h: 520,
        }
    }

    fn value(&self) -> f64 {
        self.display.parse().unwrap_or(0.0)
    }

    fn set(&mut self, v: f64) {
        if v.is_finite() {
            self.display = format_number(v);
        } else {
            self.display = "Cannot divide by zero".into();
            self.error = true;
        }
        self.fresh = true;
    }

    fn apply(a: f64, op: Op, b: f64) -> f64 {
        match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => {
                if b == 0.0 {
                    f64::INFINITY
                } else {
                    a / b
                }
            }
        }
    }

    fn clear(&mut self) {
        self.display = "0".into();
        self.pending = None;
        self.expression.clear();
        self.fresh = true;
        self.last = None;
        self.error = false;
    }

    fn press(&mut self, k: &str) {
        if self.error && k != "C" && k != "CE" {
            self.clear();
        }
        match k {
            "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                if self.fresh || self.display == "0" {
                    self.display = k.to_string();
                    self.fresh = false;
                } else if self.display.chars().filter(char::is_ascii_digit).count() < 16 {
                    self.display.push_str(k);
                }
                if self.pending.is_none() && self.last.is_some() {
                    self.last = None;
                    self.expression.clear();
                }
            }
            "." => {
                if self.fresh {
                    self.display = "0.".into();
                    self.fresh = false;
                } else if !self.display.contains('.') {
                    self.display.push('.');
                }
            }
            "\u{232B}" => {
                if !self.fresh {
                    self.display.pop();
                    if self.display.is_empty() || self.display == "-" {
                        self.display = "0".into();
                    }
                }
            }
            "CE" => {
                self.display = "0".into();
                self.fresh = true;
                self.error = false;
            }
            "C" => self.clear(),
            "\u{00B1}" => {
                let v = -self.value();
                self.display = format_number(v);
            }
            "%" => {
                // Like Windows: a percentage of the left operand.
                let v = match self.pending {
                    Some((a, Op::Add | Op::Sub)) => a * self.value() / 100.0,
                    _ => self.value() / 100.0,
                };
                self.set(v);
            }
            "1/x" => {
                let v = self.value();
                self.expression = format!("1/({})", format_number(v));
                self.set(if v == 0.0 { f64::INFINITY } else { 1.0 / v });
            }
            "x\u{00B2}" => {
                let v = self.value();
                self.expression = format!("sqr({})", format_number(v));
                self.set(v * v);
            }
            "\u{221A}x" => {
                let v = self.value();
                if v < 0.0 {
                    self.display = "Invalid input".into();
                    self.error = true;
                } else {
                    self.expression = format!("sqrt({})", format_number(v));
                    self.set(sqrt(v));
                }
            }
            "+" | "-" | "\u{00D7}" | "\u{00F7}" => {
                let op = match k {
                    "+" => Op::Add,
                    "-" => Op::Sub,
                    "\u{00D7}" => Op::Mul,
                    _ => Op::Div,
                };
                let v = self.value();
                let left = match self.pending {
                    Some((a, p)) if !self.fresh => Self::apply(a, p, v),
                    Some((a, _)) => a,
                    None => v,
                };
                self.pending = Some((left, op));
                self.expression = format!("{} {}", format_number(left), op.symbol());
                self.set(left);
                self.last = None;
            }
            "=" => {
                let v = self.value();
                let (a, op, b) = match (self.pending.take(), self.last) {
                    (Some((a, op)), _) => (a, op, v),
                    (None, Some((op, b))) => (v, op, b),
                    _ => return,
                };
                let r = Self::apply(a, op, b);
                let expr = format!("{} {} {} =", format_number(a), op.symbol(), format_number(b));
                self.expression = expr.clone();
                self.last = Some((op, b));
                self.set(r);
                if !self.error {
                    self.history.insert(0, (expr, self.display.clone()));
                    self.history.truncate(20);
                }
            }
            _ => {}
        }
    }

    fn pad(&self) -> Rect {
        let pw = if self.w >= 520 { self.w - 220 } else { self.w };
        Rect::new(0, 110, pw, self.h - 110)
    }

    fn key_rect(&self, row: usize, col: usize) -> Rect {
        let p = self.pad();
        let (kw, kh) = ((p.w - 12) / 4, (p.h - 8) / 6);
        Rect::new(p.x + 6 + col as i32 * kw, p.y + row as i32 * kh, kw - 4, kh - 4)
    }
}

impl App for Calculator {
    fn kind(&self) -> AppKind {
        AppKind::Calculator
    }
    fn title(&self) -> String {
        "Calculator".into()
    }
    fn size(&self) -> (i32, i32) {
        (560, 520)
    }
    fn min_size(&self) -> (i32, i32) {
        (300, 420)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        cv.fill(r, t.window());
        let p = self.pad();
        // Display.
        let ew = f.measure(&self.expression, 14, Weight::Regular);
        f.draw(cv, r.x + p.w - ew - 16, r.y + 12, &self.expression, 14, Weight::Regular, t.text_dim());
        let shown = grouped(&self.display);
        let mut size = 44u16;
        while size > 18 && f.measure(&shown, size, Weight::Semibold) > p.w - 32 {
            size -= 4;
        }
        let dw = f.measure(&shown, size, Weight::Semibold);
        f.draw(cv, r.x + p.w - dw - 16, r.y + 40 + (44 - size as i32) / 2, &shown, size, Weight::Semibold, t.text());
        // Keys.
        for (row, keys) in KEYS.iter().enumerate() {
            for (col, k) in keys.iter().enumerate() {
                let kr = self.key_rect(row, col).offset(r.x, r.y);
                let digit = k.len() == 1 && k.as_bytes()[0].is_ascii_digit() || *k == "." || *k == "\u{00B1}";
                let (bg, fg) = if *k == "=" {
                    (t.accent, t.accent_text())
                } else if digit {
                    (if t.dark { rgb(0x3B, 0x3B, 0x3B) } else { rgb(0xFF, 0xFF, 0xFF) }, t.text())
                } else {
                    (if t.dark { rgb(0x32, 0x32, 0x32) } else { rgb(0xF6, 0xF6, 0xF6) }, t.text())
                };
                cv.rounded_box(kr, 5, if *k == "=" { bg } else { shade(bg, if t.dark { 8 } else { -8 }) }, bg);
                let (mx, my) = (kr.x as f32 + kr.w as f32 / 2.0, kr.y as f32 + kr.h as f32 / 2.0);
                if *k == "\u{232B}" {
                    // Backspace: a left-pointing tag with a cross.
                    let pts = [(-9.0, 0.0), (-4.0, -6.0), (9.0, -6.0), (9.0, 6.0), (-4.0, 6.0), (-9.0, 0.0)];
                    for w in pts.windows(2) {
                        cv.line(mx + w[0].0, my + w[0].1, mx + w[1].0, my + w[1].1, 1.3, fg);
                    }
                    cv.line(mx - 1.0, my - 3.0, mx + 5.0, my + 3.0, 1.3, fg);
                    cv.line(mx + 5.0, my - 3.0, mx - 1.0, my + 3.0, 1.3, fg);
                    continue;
                }
                if *k == "\u{221A}x" {
                    // A radical sign over x.
                    cv.line(mx - 13.0, my + 1.0, mx - 10.0, my - 1.0, 1.3, fg);
                    cv.line(mx - 10.0, my - 1.0, mx - 6.0, my + 7.0, 1.3, fg);
                    cv.line(mx - 6.0, my + 7.0, mx - 1.0, my - 8.0, 1.3, fg);
                    cv.line(mx - 1.0, my - 8.0, mx + 12.0, my - 8.0, 1.3, fg);
                    f.draw(cv, kr.x + kr.w / 2 + 1, kr.y + kr.h / 2 - 9, "x", 16, Weight::Regular, fg);
                    continue;
                }
                let size = if digit { 20 } else { 16 };
                let weight = if digit { Weight::Semibold } else { Weight::Regular };
                let kw = f.measure(k, size, weight);
                let kh = f.line_height(size);
                f.draw(cv, kr.x + (kr.w - kw) / 2, kr.y + (kr.h - kh) / 2, k, size, weight, fg);
            }
        }
        // History.
        if p.w < r.w {
            let hx = r.x + p.w + 8;
            cv.fill(Rect::new(hx - 8, r.y, 1, r.h), t.border());
            f.draw(cv, hx + 8, r.y + 12, "History", 14, Weight::Semibold, t.text());
            if self.history.is_empty() {
                f.draw(cv, hx + 8, r.y + 44, "There's no history yet", 12, Weight::Regular, t.text_dim());
            }
            for (i, (expr, result)) in self.history.iter().enumerate() {
                let y = r.y + 44 + i as i32 * 54;
                if y + 50 > r.bottom() {
                    break;
                }
                let w1 = f.measure(expr, 12, Weight::Regular);
                f.draw_fit(cv, (r.right() - 16 - w1).max(hx + 8), y, expr, 196, 12, Weight::Regular, t.text_dim());
                let res = grouped(result);
                let w2 = f.measure(&res, 20, Weight::Semibold);
                f.draw(cv, r.right() - 16 - w2, y + 18, &res, 20, Weight::Semibold, t.text());
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        for (row, keys) in KEYS.iter().enumerate() {
            for (col, k) in keys.iter().enumerate() {
                if self.key_rect(row, col).contains(x, y) {
                    self.press(k);
                    return Vec::new();
                }
            }
        }
        // A click on a history entry brings its result back.
        let p = self.pad();
        if x > p.w && y >= 44 {
            let i = ((y - 44) / 54) as usize;
            if let Some((_, result)) = self.history.get(i) {
                self.display = result.clone();
                self.fresh = true;
            }
        }
        Vec::new()
    }

    fn right_click(&mut self, x: i32, y: i32, cx: &mut Ctx) -> Vec<Action> {
        vec![Action::Menu(
            vec![
                MenuItem::new("Copy", 1),
                MenuItem::when("Paste", 2, !cx.clipboard.text.is_empty()),
                MenuItem::separator(),
                MenuItem::when("Clear history", 3, !self.history.is_empty()),
            ],
            x,
            y,
        )]
    }

    fn menu(&mut self, id: u32, cx: &mut Ctx) -> Vec<Action> {
        match id {
            1 => cx.clipboard.text = self.display.clone(),
            2 => {
                let text: String = cx.clipboard.text.chars().filter(|c| !matches!(c, ',' | ' ')).collect();
                if let Ok(v) = text.trim().parse::<f64>() {
                    self.display = format_number(v);
                    self.fresh = false;
                }
            }
            3 => self.history.clear(),
            _ => {}
        }
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, cx: &mut Ctx) -> Vec<Action> {
        if ctrl(e, 'c') {
            return self.menu(1, cx);
        }
        if ctrl(e, 'v') {
            return self.menu(2, cx);
        }
        let k = match e.key {
            key::ENTER => "=",
            key::BACKSPACE => "\u{232B}",
            key::ESCAPE => "C",
            key::DELETE => "CE",
            key::CHAR => match char::from_u32(e.ch).unwrap_or(' ') {
                c @ '0'..='9' => ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"][c as usize - '0' as usize],
                '.' | ',' => ".",
                '+' => "+",
                '-' => "-",
                '*' | 'x' | 'X' => "\u{00D7}",
                '/' => "\u{00F7}",
                '=' => "=",
                '%' => "%",
                'r' | 'R' => "1/x",
                'q' | 'Q' => "x\u{00B2}",
                '@' => "\u{221A}x",
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        self.press(k);
        Vec::new()
    }
}
